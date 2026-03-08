//! DFU (Device Firmware Update) protocol for FNIRSI devices.
//!
//! Implements the proprietary firmware update sequence used by the FNB58
//! and related devices: erase flash, then write in 58-byte chunks with
//! CRC-8 integrity checks.

use crate::crc;
use crate::protocol::PACKET_SIZE;
use crate::usb::UsbError;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info};

/// Errors from DFU operations.
#[derive(Debug, Error)]
pub enum DfuError {
    #[error("USB error: {0}")]
    Usb(#[from] UsbError),
    #[error("hidapi error: {0}")]
    Hid(#[from] hidapi::HidError),
    #[error("no response from device")]
    NoResponse,
    #[error("firmware file is empty")]
    EmptyFirmware,
    #[error("firmware file too large: {0} bytes")]
    FirmwareTooLarge(usize),
    #[error("failed to read firmware file: {0}")]
    IoError(#[from] std::io::Error),
}

/// Maximum payload per DFU write packet.
///
/// 64 bytes total - 1 endpoint byte - 4 param bytes - 1 CRC byte = 58 bytes.
const MAX_DFU_PAYLOAD: usize = PACKET_SIZE - 6;

/// DFU endpoint bytes.
const EP_START_UPDATE: u8 = 0x28;
const EP_WRITE_DATA: u8 = 0x2B;

/// Timeout for erase operation (flash erase takes a while).
const ERASE_TIMEOUT: Duration = Duration::from_secs(15);

/// Timeout for individual write operations.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Read a .ufn firmware file from disk.
pub fn read_firmware_file(path: &std::path::Path) -> Result<Vec<u8>, DfuError> {
    let data = std::fs::read(path)?;
    if data.is_empty() {
        return Err(DfuError::EmptyFirmware);
    }
    info!("Loaded firmware: {} bytes from {:?}", data.len(), path);
    Ok(data)
}

/// Build a DFU packet.
///
/// Layout: `[endpoint, param0..3, payload..., 0-padding, CRC]`
fn make_dfu_packet(ep: u8, param: u32, payload: &[u8]) -> [u8; PACKET_SIZE] {
    let mut buf = [0u8; PACKET_SIZE];

    buf[0] = ep;
    buf[1] = param as u8;
    buf[2] = (param >> 8) as u8;
    buf[3] = (param >> 16) as u8;
    buf[4] = (param >> 24) as u8;

    let len = payload.len().min(MAX_DFU_PAYLOAD);
    buf[5..5 + len].copy_from_slice(&payload[..len]);

    // CRC covers bytes 0..63 (everything except the last byte)
    buf[PACKET_SIZE - 1] = crc::dfu_crc(&buf[..PACKET_SIZE - 1]);

    buf
}

/// Perform a full DFU firmware upload to an FNIRSI device.
///
/// `progress_callback` is called with `(bytes_written, total_bytes)` after each chunk.
pub fn flash_firmware(
    device: &hidapi::HidDevice,
    firmware: &[u8],
    firmware_version: u16,
    progress_callback: impl Fn(usize, usize),
) -> Result<(), DfuError> {
    let fw_size = firmware.len();

    info!(
        "Starting DFU: {} bytes, version code {}",
        fw_size, firmware_version
    );

    // Step 1: Start update (erase flash)
    info!("Erasing flash...");
    let mut start_payload = [0u8; 6];
    start_payload[0] = firmware_version as u8;
    start_payload[1] = (firmware_version >> 8) as u8;
    start_payload[2] = fw_size as u8;
    start_payload[3] = (fw_size >> 8) as u8;
    start_payload[4] = (fw_size >> 16) as u8;
    start_payload[5] = (fw_size >> 24) as u8;

    let pkt = make_dfu_packet(EP_START_UPDATE, start_payload.len() as u32, &start_payload);
    write_and_wait_response(device, &pkt, ERASE_TIMEOUT)?;

    // Step 2: Write firmware in chunks
    info!("Writing firmware...");
    let mut offset = 0;
    let mut chunk_id: usize = 0;

    while offset < fw_size {
        let remaining = fw_size - offset;
        let len = remaining.min(MAX_DFU_PAYLOAD);
        let chunk = &firmware[offset..offset + len];

        // Build the per-chunk address parameter (matches the reference app).
        let i = chunk_id + 1;
        let i_low = (i & 0xFF) as u8;
        let i_high = ((i >> 8) & 0xFF) as u8;
        let addr: u32 =
            (0x3A) | (((i % 0x32) as u32) << 8) | (u32::from(i_high) << 16) | (u32::from(i_low) << 24);

        let pkt = make_dfu_packet(EP_WRITE_DATA, addr, chunk);
        write_and_wait_response(device, &pkt, WRITE_TIMEOUT)?;

        offset += len;
        chunk_id += 1;
        progress_callback(offset, fw_size);

        debug!("Wrote chunk {chunk_id}: {len} bytes (total: {offset}/{fw_size})");
    }

    info!(
        "DFU complete: {} bytes written in {} chunks",
        fw_size, chunk_id
    );
    Ok(())
}

/// Write a packet and wait for a response.
fn write_and_wait_response(
    device: &hidapi::HidDevice,
    packet: &[u8; PACKET_SIZE],
    timeout: Duration,
) -> Result<(), DfuError> {
    // Prepend report ID 0x00 for hidapi.
    let mut buf = [0u8; PACKET_SIZE + 1];
    buf[0] = 0x00;
    buf[1..].copy_from_slice(packet);
    device.write(&buf)?;

    // Read response
    let mut response = [0u8; PACKET_SIZE];
    let n = device
        .read_timeout(&mut response, timeout.as_millis() as i32)?;

    if n == 0 {
        return Err(DfuError::NoResponse);
    }

    debug!("DFU response: {} bytes", n);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_dfu_packet_structure() {
        let payload = [0x42u8; 6];
        let pkt = make_dfu_packet(0x28, 0x00000006, &payload);

        assert_eq!(pkt[0], 0x28); // endpoint
        assert_eq!(pkt[1], 0x06); // param low byte
        assert_eq!(pkt[5..11], payload); // payload

        // CRC is the last byte, computed from bytes 0..63
        let expected_crc = crc::dfu_crc(&pkt[..63]);
        assert_eq!(pkt[63], expected_crc);
    }

    #[test]
    fn test_make_dfu_packet_zeros() {
        let pkt = make_dfu_packet(0x00, 0, &[]);
        // All zeros except CRC
        for i in 0..63 {
            assert_eq!(pkt[i], 0);
        }
    }

    #[test]
    fn test_max_payload_size() {
        // Payload larger than MAX_DFU_PAYLOAD should be truncated
        let payload = [0xAA; 100];
        let pkt = make_dfu_packet(0x2B, 0, &payload);
        // Verify it doesn't overflow — packet should be exactly 64 bytes
        assert_eq!(pkt.len(), 64);
        // The first MAX_DFU_PAYLOAD bytes of the payload area should be 0xAA
        for i in 5..5 + MAX_DFU_PAYLOAD {
            assert_eq!(pkt[i], 0xAA);
        }
    }
}
