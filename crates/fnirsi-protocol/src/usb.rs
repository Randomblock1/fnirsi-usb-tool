//! USB HID transport layer.
//!
//! Wraps `hidapi` to discover, connect to, and stream data from FNIRSI USB
//! power meters.  Handles device enumeration, initialization handshakes,
//! keep-alive signalling, and raw HID report I/O.

use crate::device::{self, DFU_USB_ID, DeviceInfo, DeviceType};
use crate::protocol::{self, PACKET_SIZE};
use crate::sample::Sample;
use hidapi::HidApi;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Errors from USB HID operations.
#[derive(Debug, Error)]
pub enum UsbError {
    #[error("hidapi error: {0}")]
    Hid(#[from] hidapi::HidError),
    #[error("no FNIRSI device found")]
    DeviceNotFound,
    #[error("device not connected")]
    NotConnected,
    #[error("protocol error: {0}")]
    Protocol(#[from] protocol::ProtocolError),
    #[error("write failed")]
    WriteFailed,
}

/// USB HID transport for FNIRSI devices.
pub struct UsbDevice {
    device: hidapi::HidDevice,
    info: DeviceInfo,
    is_fnb58_variant: bool,
    keepalive_interval: Duration,
    last_keepalive: Instant,
}

impl UsbDevice {
    /// Enumerate all connected FNIRSI devices.
    pub fn list_devices() -> Result<Vec<DeviceInfo>, UsbError> {
        let api = HidApi::new()?;
        let mut devices = Vec::new();

        for dev_info in api.device_list() {
            let vid = dev_info.vendor_id();
            let pid = dev_info.product_id();

            let product_string = dev_info
                .product_string()
                .map(std::string::ToString::to_string);
            if let Some(device_type) =
                device::detect_device_type(vid, pid, product_string.as_deref())
            {
                devices.push(DeviceInfo {
                    device_type,
                    vid,
                    pid,
                    manufacturer: dev_info
                        .manufacturer_string()
                        .map(std::string::ToString::to_string),
                    product: product_string.clone(),
                    serial: dev_info
                        .serial_number()
                        .map(std::string::ToString::to_string),
                    path: Some(dev_info.path().as_bytes().to_vec()),
                });
            }
        }

        Ok(devices)
    }

    /// Enumerate devices in DFU mode.
    pub fn list_dfu_devices() -> Result<Vec<DeviceInfo>, UsbError> {
        let api = HidApi::new()?;
        let mut devices = Vec::new();

        for dev_info in api.device_list() {
            if dev_info.vendor_id() == DFU_USB_ID.vid && dev_info.product_id() == DFU_USB_ID.pid {
                devices.push(DeviceInfo {
                    device_type: DeviceType::Fnb58,
                    vid: DFU_USB_ID.vid,
                    pid: DFU_USB_ID.pid,
                    manufacturer: dev_info
                        .manufacturer_string()
                        .map(std::string::ToString::to_string),
                    product: dev_info
                        .product_string()
                        .map(std::string::ToString::to_string),
                    serial: dev_info
                        .serial_number()
                        .map(std::string::ToString::to_string),
                    path: Some(dev_info.path().as_bytes().to_vec()),
                });
            }
        }

        Ok(devices)
    }

    /// Connect to the first available FNIRSI device.
    pub fn connect_first() -> Result<Self, UsbError> {
        let devices = Self::list_devices()?;
        let info = devices.into_iter().next().ok_or(UsbError::DeviceNotFound)?;
        Self::connect_device(info)
    }

    /// Connect to a specific device by its info.
    pub fn connect_device(info: DeviceInfo) -> Result<Self, UsbError> {
        let api = HidApi::new()?;

        let device = if let Some(ref path) = info.path {
            let cstr =
                std::ffi::CString::new(path.clone()).map_err(|_| UsbError::DeviceNotFound)?;
            api.open_path(&cstr)?
        } else {
            api.open(info.vid, info.pid)?
        };

        device.set_blocking_mode(false)?;

        let is_fnb58_variant = device::is_fnb58_variant(info.device_type);
        let keepalive_interval = protocol::keepalive_interval(is_fnb58_variant);

        info!(
            "Connected to {} ({:04X}:{:04X})",
            info.device_type, info.vid, info.pid
        );

        Ok(Self {
            device,
            info,
            is_fnb58_variant,
            keepalive_interval,
            last_keepalive: Instant::now(),
        })
    }

    /// Get device info.
    #[must_use]
    pub const fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Send the initialization handshake to start data streaming.
    pub fn start_streaming(&mut self) -> Result<(), UsbError> {
        let commands = protocol::init_commands(self.is_fnb58_variant);
        for cmd in &commands {
            self.write_report(cmd)?;
            // If this delay isn't here, the device will crash
            std::thread::sleep(Duration::from_millis(25));
        }
        self.last_keepalive = Instant::now();
        info!("Streaming started");
        Ok(())
    }

    /// Read one packet from the device. Returns `None` if no data is available
    /// (non-blocking mode).
    pub fn read_packet(&self) -> Result<Option<[u8; PACKET_SIZE]>, UsbError> {
        let mut buf = [0u8; PACKET_SIZE];
        let n = self.device.read(&mut buf)?;
        if n == 0 { Ok(None) } else { Ok(Some(buf)) }
    }

    /// Read one packet with a timeout. Returns `None` on timeout.
    pub fn read_packet_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<[u8; PACKET_SIZE]>, UsbError> {
        let mut buf = [0u8; PACKET_SIZE];
        let n = self
            .device
            .read_timeout(&mut buf, timeout.as_millis() as i32)?;
        if n == 0 { Ok(None) } else { Ok(Some(buf)) }
    }

    /// Read and decode one data packet. Returns `None` if no data packet is
    /// available or if the packet is not a data packet.
    pub fn read_samples(&self, validate_crc: bool) -> Result<Option<[Sample; 4]>, UsbError> {
        Ok(self
            .read_packet()?
            .and_then(|pkt| protocol::try_decode_data_packet(&pkt, validate_crc)))
    }

    /// Read and decode with timeout.
    pub fn read_samples_timeout(
        &self,
        timeout: Duration,
        validate_crc: bool,
    ) -> Result<Option<[Sample; 4]>, UsbError> {
        Ok(self
            .read_packet_timeout(timeout)?
            .and_then(|pkt| protocol::try_decode_data_packet(&pkt, validate_crc)))
    }

    /// Send keep-alive if enough time has elapsed since the last one.
    pub fn send_keepalive_if_needed(&mut self) -> Result<(), UsbError> {
        if self.last_keepalive.elapsed() >= self.keepalive_interval {
            self.send_keepalive()?;
        }
        Ok(())
    }

    /// Send keep-alive unconditionally.
    pub fn send_keepalive(&mut self) -> Result<(), UsbError> {
        self.write_report(&protocol::CMD_KEEPALIVE_AA83)?;
        self.last_keepalive = Instant::now();
        debug!("Keep-alive sent");
        Ok(())
    }

    /// Write a raw 64-byte HID report to the device.
    pub fn write_report(&self, data: &[u8; PACKET_SIZE]) -> Result<(), UsbError> {
        // Prepend report ID 0x00; hidapi requires this even for devices
        // that don't use numbered reports.
        let mut buf = [0u8; PACKET_SIZE + 1];
        buf[0] = 0x00;
        buf[1..].copy_from_slice(data);
        let written = self.device.write(&buf)?;
        if written == 0 {
            warn!("HID write returned 0 bytes");
            return Err(UsbError::WriteFailed);
        }
        Ok(())
    }

    /// Whether this device is an FNB58/FNB48S variant.
    #[must_use]
    pub const fn is_fnb58_variant(&self) -> bool {
        self.is_fnb58_variant
    }

    /// Get the keepalive interval for this device.
    #[must_use]
    pub const fn keepalive_interval(&self) -> Duration {
        self.keepalive_interval
    }
}

/// Conversion trait so `CStr` paths can be turned into byte slices.
trait AsBytes {
    fn as_bytes(&self) -> &[u8];
}

impl AsBytes for std::ffi::CStr {
    fn as_bytes(&self) -> &[u8] {
        Self::to_bytes(self)
    }
}
