//! Bluetooth Low Energy transport layer.
//!
//! Discovers, connects to, and streams data from BLE-capable FNIRSI devices
//! (currently the FNB58).  Uses `btleplug` for GATT operations and `tokio`
//! for async I/O.

use crate::sample::Sample;
use serde::Serialize;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// BLE GATT UUIDs used by FNIRSI devices.
pub mod gatt {
    use btleplug::api::bleuuid::uuid_from_u16;
    use uuid::Uuid;

    /// Notification characteristic — device sends measurement data here.
    pub const NOTIFY_CHAR: Uuid = uuid_from_u16(0xFFE4);

    /// Write characteristic — host sends commands here.
    pub const WRITE_CHAR: Uuid = uuid_from_u16(0xFFE9);
}

/// Errors from BLE operations.
#[derive(Debug, Error)]
pub enum BleError {
    #[error("btleplug error: {0}")]
    Btleplug(#[from] btleplug::Error),
    #[error("no FNIRSI device found via BLE")]
    DeviceNotFound,
    #[error("characteristic not found: {0}")]
    CharacteristicNotFound(String),
    #[error("device not connected")]
    NotConnected,
    #[error("scan timeout")]
    ScanTimeout,
}

/// Information about a BLE-discovered FNIRSI device.
#[derive(Debug, Clone, Serialize)]
pub struct BleDeviceInfo {
    pub name: Option<String>,
    pub address: String,
}

/// BLE initialization commands (4 bytes each, much shorter than the 64-byte
/// USB HID packets).
const BLE_CMD_INIT: [u8; 4] = [0xAA, 0x81, 0x00, 0xF4];
const BLE_CMD_START: [u8; 4] = [0xAA, 0x82, 0x00, 0xA7];

/// Decode a BLE AA07 voltage/current notification.
///
/// BLE uses a different, shorter packet format than USB:
/// - Header: AA 07 04
/// - 2 bytes LE voltage (÷1000 → volts)
/// - 2 bytes LE current (÷1000 → amps)
#[must_use]
pub fn decode_ble_aa07(data: &[u8]) -> Option<Sample> {
    // Scan for the AA 07 04 header sequence anywhere in the payload.
    for i in 0..data.len().saturating_sub(6) {
        if data[i] == 0xAA && data[i + 1] == 0x07 && data[i + 2] == 0x04 {
            let voltage_raw = u16::from_le_bytes([data[i + 3], data[i + 4]]);
            let current_raw = u16::from_le_bytes([data[i + 5], data[i + 6]]);

            let voltage_v = f32::from(voltage_raw) / 1000.0;
            let current_a = f32::from(current_raw) / 1000.0;

            return Some(Sample {
                timestamp_ms: 0,
                voltage_v,
                current_a,
                power_w: voltage_v * current_a,
                dp_v: 0.0,
                dn_v: 0.0,
                temp_c: 0.0,
                raw_voltage: u32::from(voltage_raw),
                raw_current: u32::from(current_raw),
            });
        }
    }
    None
}

/// Scan for FNIRSI BLE devices.
///
/// Scans for `scan_duration` and returns all devices with "FNB" or "FNIRSI"
/// in their advertised name.
pub async fn scan_devices(scan_duration: Duration) -> Result<Vec<BleDeviceInfo>, BleError> {
    use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
    use btleplug::platform::Manager;

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or(BleError::DeviceNotFound)?;

    info!("Starting BLE scan for {scan_duration:?}...");
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(scan_duration).await;
    adapter.stop_scan().await?;

    let peripherals = adapter.peripherals().await?;
    let mut devices = Vec::new();

    for p in peripherals {
        if let Some(props) = p.properties().await? {
            let name: Option<String> = props.local_name.clone();
            let address = props.address.to_string();

            if let Some(ref n) = name {
                let n_lower: String = n.to_lowercase();
                if n_lower.contains("fnb") || n_lower.contains("fnirsi") {
                    debug!("Found FNIRSI BLE device: {n} at {address}");
                    devices.push(BleDeviceInfo { name, address });
                }
            }
        }
    }

    info!("BLE scan complete, found {} device(s)", devices.len());
    Ok(devices)
}

/// Connect to an FNIRSI device over BLE and start receiving samples.
///
/// Returns a channel receiver that yields `Sample` values as they arrive.
pub async fn connect_and_stream<F>(
    address: &str,
    scan_duration: Duration,
    mut on_status: F,
) -> Result<(mpsc::Receiver<Sample>, tokio::task::JoinHandle<()>), BleError>
where
    F: FnMut(String) + Send + 'static,
{
    use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
    use btleplug::platform::Manager;
    use futures_util::StreamExt;

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or(BleError::DeviceNotFound)?;

    // First, try to find the peripheral in the adapter's existing cache.
    let mut target_peripheral = None;
    for p in adapter.peripherals().await? {
        if let Some(props) = p.properties().await?
            && props.address.to_string() == address
        {
            target_peripheral = Some(p);
            break;
        }
    }

    // If not found in cache, fall back to scanning for it.
    if target_peripheral.is_none() {
        on_status(format!("Scanning for {address}..."));
        adapter.start_scan(ScanFilter::default()).await?;
        tokio::time::sleep(scan_duration).await;
        adapter.stop_scan().await?;

        for p in adapter.peripherals().await? {
            if let Some(props) = p.properties().await?
                && props.address.to_string() == address
            {
                target_peripheral = Some(p);
                break;
            }
        }
    }

    let peripheral = target_peripheral.ok_or(BleError::DeviceNotFound)?;

    info!("Connecting to BLE device at {address}...");
    on_status("Connecting... (takes time)".to_string());
    peripheral.connect().await?;
    on_status("Discovering services...".to_string());
    peripheral.discover_services().await?;

    // Locate the GATT write and notify characteristics.
    let chars = peripheral.characteristics();
    let write_char = chars
        .iter()
        .find(|c| c.uuid == gatt::WRITE_CHAR)
        .cloned()
        .ok_or_else(|| BleError::CharacteristicNotFound("write (FFE9)".to_string()))?;
    let notify_char = chars
        .iter()
        .find(|c| c.uuid == gatt::NOTIFY_CHAR)
        .cloned()
        .ok_or_else(|| BleError::CharacteristicNotFound("notify (FFE4)".to_string()))?;

    // Subscribe to notifications
    on_status("Subscribing to notifications...".to_string());
    peripheral.subscribe(&notify_char).await?;

    // Send init commands
    on_status("Initializing device...".to_string());
    peripheral
        .write(&write_char, &BLE_CMD_INIT, WriteType::WithoutResponse)
        .await?;
    peripheral
        .write(&write_char, &BLE_CMD_START, WriteType::WithoutResponse)
        .await?;

    info!("BLE streaming started");

    // Spawn task to read notification stream and decode samples
    let (tx, rx) = mpsc::channel::<Sample>(256);
    let mut notification_stream = peripheral.notifications().await?;

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                opt = notification_stream.next() => {
                    match opt {
                        Some(notification) => {
                            if let Some(sample) = decode_ble_aa07(&notification.value)
                                && tx.send(sample).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                () = tx.closed() => {
                    break;
                }
            }
        }
        info!("BLE notification stream ended");
        if let Err(e) = peripheral.disconnect().await {
            warn!("Error disconnecting BLE: {e}");
        } else {
            info!("Disconnected from BLE device");
        }
    });

    Ok((rx, handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_ble_aa07() {
        // AA 07 04, voltage 5000 (5.0V), current 1500 (1.5A)
        let data: Vec<u8> = vec![0xAA, 0x07, 0x04, 0x88, 0x13, 0xDC, 0x05];
        let sample = decode_ble_aa07(&data).unwrap();
        assert!((sample.voltage_v - 5.0).abs() < 0.01);
        assert!((sample.current_a - 1.5).abs() < 0.01);
    }

    #[test]
    fn test_decode_ble_aa07_in_middle() {
        // Prefix garbage + AA 07 04 + data
        let data: Vec<u8> = vec![0x00, 0xFF, 0xAA, 0x07, 0x04, 0xE8, 0x03, 0xF4, 0x01];
        let sample = decode_ble_aa07(&data).unwrap();
        assert!((sample.voltage_v - 1.0).abs() < 0.01);
        assert!((sample.current_a - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_decode_ble_aa07_too_short() {
        let data: Vec<u8> = vec![0xAA, 0x07, 0x04, 0xE8];
        assert!(decode_ble_aa07(&data).is_none());
    }

    #[test]
    fn test_decode_ble_aa07_no_match() {
        let data: Vec<u8> = vec![0xBB, 0x07, 0x04, 0xE8, 0x03, 0xF4, 0x01];
        assert!(decode_ble_aa07(&data).is_none());
    }
}
