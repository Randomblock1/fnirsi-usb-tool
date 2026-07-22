//! Packet decoding and command definitions for the FNIRSI wire protocol.
//!
//! All FNIRSI USB power meters exchange fixed 64-byte HID reports.  Data
//! packets carry four 15-byte measurement samples plus header and CRC bytes.

use crate::crc;
use crate::sample::Sample;
use thiserror::Error;

/// Errors from packet decoding.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("packet too short: expected 64 bytes, got {0}")]
    PacketTooShort(usize),
    #[error("invalid header byte: expected 0xAA, got 0x{0:02X}")]
    InvalidHeader(u8),
    #[error("CRC mismatch: expected 0x{expected:02X}, got 0x{actual:02X}")]
    CrcMismatch { expected: u8, actual: u8 },
    #[error("not a data packet: type 0x{0:02X}")]
    NotDataPacket(u8),
}

/// HID report/packet size used by all FNIRSI devices.
pub const PACKET_SIZE: usize = 64;

/// Number of samples per data packet.
pub const SAMPLES_PER_PACKET: usize = 4;

/// Size of one sample within a data packet.
const SAMPLE_SIZE: usize = 15;

// --- Initialization commands (64 bytes each, including 0xAA header + CRC) ---

/// Command AA81: initial identification/handshake.
pub const CMD_INIT_AA81: [u8; 64] = {
    let mut buf = [0u8; 64];
    buf[0] = 0xAA;
    buf[1] = 0x81;
    buf[63] = 0x8E;
    buf
};

/// Command AA82: setup / request data stream.
pub const CMD_SETUP_AA82: [u8; 64] = {
    let mut buf = [0u8; 64];
    buf[0] = 0xAA;
    buf[1] = 0x82;
    buf[63] = 0x96;
    buf
};

/// Command AA83: keep-alive / continue data stream.
pub const CMD_KEEPALIVE_AA83: [u8; 64] = {
    let mut buf = [0u8; 64];
    buf[0] = 0xAA;
    buf[1] = 0x83;
    buf[63] = 0x9E;
    buf
};

/// Packet type byte values.
pub mod packet_type {
    /// Data packet containing 4 measurement samples.
    pub const DATA: u8 = 0x04;
    /// Device info / response packet.
    pub const INFO: u8 = 0x03;
}

/// Build the initialization command sequence.
///
/// FNB58/FNB48S: `[AA81, AA82, AA82]`
/// FNB48/C1:     `[AA81, AA82, AA83]`
#[must_use]
pub fn init_commands(is_fnb58_variant: bool) -> Vec<[u8; 64]> {
    if is_fnb58_variant {
        vec![CMD_INIT_AA81, CMD_SETUP_AA82, CMD_SETUP_AA82]
    } else {
        vec![CMD_INIT_AA81, CMD_SETUP_AA82, CMD_KEEPALIVE_AA83]
    }
}

/// Keep-alive interval for different device variants.
#[must_use]
pub const fn keepalive_interval(is_fnb58_variant: bool) -> std::time::Duration {
    if is_fnb58_variant {
        std::time::Duration::from_secs(1)
    } else {
        std::time::Duration::from_millis(3)
    }
}

/// Raw voltage/current LSB = 10 uV / 10 uA.
const VI_SCALE: f32 = 1.0 / 100_000.0;
/// Raw D+/D- LSB = 1 mV.
const DATA_LINE_SCALE: f32 = 1.0 / 1_000.0;
/// Raw temperature LSB = 0.1 degC.
const TEMP_SCALE: f32 = 1.0 / 10.0;

/// Decode a single 15-byte sample from within a data packet.
fn decode_sample(data: &[u8]) -> Sample {
    debug_assert!(data.len() >= SAMPLE_SIZE);

    let raw_voltage = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let raw_current = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let dp_raw = u16::from_le_bytes([data[8], data[9]]);
    let dn_raw = u16::from_le_bytes([data[10], data[11]]);
    // data[12] is an unknown constant (always 0x01).
    let temp_raw = u16::from_le_bytes([data[13], data[14]]);

    let voltage_v = raw_voltage as f32 * VI_SCALE;
    let current_a = raw_current as f32 * VI_SCALE;

    Sample {
        timestamp_ms: 0,
        voltage_v,
        current_a,
        power_w: voltage_v * current_a,
        dp_v: f32::from(dp_raw) * DATA_LINE_SCALE,
        dn_v: f32::from(dn_raw) * DATA_LINE_SCALE,
        temp_c: f32::from(temp_raw) * TEMP_SCALE,
        raw_voltage,
        raw_current,
    }
}

/// Decode a 64-byte data packet into 4 samples.
///
/// Validates the header (0xAA), packet type (0x04), and CRC.
pub fn decode_data_packet(
    packet: &[u8; 64],
    validate_crc: bool,
) -> Result<[Sample; 4], ProtocolError> {
    if packet[0] != 0xAA {
        return Err(ProtocolError::InvalidHeader(packet[0]));
    }

    if packet[1] != packet_type::DATA {
        return Err(ProtocolError::NotDataPacket(packet[1]));
    }

    if validate_crc {
        let expected = packet[63];
        let actual = crc::data_crc(&packet[1..63]);
        if actual != expected {
            return Err(ProtocolError::CrcMismatch { expected, actual });
        }
    }

    let mut samples = [Sample {
        timestamp_ms: 0,
        voltage_v: 0.0,
        current_a: 0.0,
        power_w: 0.0,
        dp_v: 0.0,
        dn_v: 0.0,
        temp_c: 0.0,
        raw_voltage: 0,
        raw_current: 0,
    }; 4];

    for (i, sample) in samples.iter_mut().enumerate().take(SAMPLES_PER_PACKET) {
        let offset = 2 + i * SAMPLE_SIZE;
        *sample = decode_sample(&packet[offset..offset + SAMPLE_SIZE]);
    }

    Ok(samples)
}

/// Try to decode a raw 64-byte buffer that might or might not be a data packet.
///
/// Returns `None` for non-data packets (e.g. info responses) instead of an error.
#[must_use]
pub fn try_decode_data_packet(packet: &[u8; 64], validate_crc: bool) -> Option<[Sample; 4]> {
    if packet[0] != 0xAA || packet[1] != packet_type::DATA {
        return None;
    }
    decode_data_packet(packet, validate_crc).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic data packet with known values for testing.
    fn make_test_packet(voltage: u32, current: u32, dp: u16, dn: u16, temp: u16) -> [u8; 64] {
        let mut pkt = [0u8; 64];
        pkt[0] = 0xAA;
        pkt[1] = packet_type::DATA;

        for i in 0..4 {
            let off = 2 + i * SAMPLE_SIZE;
            pkt[off..off + 4].copy_from_slice(&voltage.to_le_bytes());
            pkt[off + 4..off + 8].copy_from_slice(&current.to_le_bytes());
            pkt[off + 8..off + 10].copy_from_slice(&dp.to_le_bytes());
            pkt[off + 10..off + 12].copy_from_slice(&dn.to_le_bytes());
            pkt[off + 12] = 0x01;
            pkt[off + 13..off + 15].copy_from_slice(&temp.to_le_bytes());
        }

        // Set correct CRC
        pkt[63] = crc::data_crc(&pkt[1..63]);
        pkt
    }

    #[test]
    fn test_decode_5v_1a() {
        // 5.00000 V => 500000, 1.00000 A => 100000
        let pkt = make_test_packet(500_000, 100_000, 2900, 100, 250);
        let samples = decode_data_packet(&pkt, true).unwrap();

        let s = &samples[0];
        assert!((s.voltage_v - 5.0).abs() < 1e-5);
        assert!((s.current_a - 1.0).abs() < 1e-5);
        assert!((s.power_w - 5.0).abs() < 1e-5);
        assert!((s.dp_v - 2.9).abs() < 1e-3);
        assert!((s.dn_v - 0.1).abs() < 1e-3);
        assert!((s.temp_c - 25.0).abs() < 1e-1);
    }

    #[test]
    fn test_decode_all_four_samples() {
        let pkt = make_test_packet(900_000, 300_000, 0, 0, 300);
        let samples = decode_data_packet(&pkt, true).unwrap();
        assert_eq!(samples.len(), 4);
        for s in &samples {
            assert!((s.voltage_v - 9.0).abs() < 1e-5);
            assert!((s.current_a - 3.0).abs() < 1e-5);
        }
    }

    #[test]
    fn test_crc_validation_fails_on_corruption() {
        let mut pkt = make_test_packet(500_000, 100_000, 0, 0, 250);
        pkt[10] ^= 0xFF; // corrupt a byte
        let result = decode_data_packet(&pkt, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_crc_validation_skipped() {
        let mut pkt = make_test_packet(500_000, 100_000, 0, 0, 250);
        pkt[10] ^= 0xFF; // corrupt a byte
        // With CRC validation disabled, it should still decode (wrong values though)
        let result = decode_data_packet(&pkt, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_wrong_header() {
        let mut pkt = [0u8; 64];
        pkt[0] = 0xBB; // wrong header
        let result = decode_data_packet(&pkt, false);
        assert!(matches!(result, Err(ProtocolError::InvalidHeader(0xBB))));
    }

    #[test]
    fn test_non_data_packet() {
        let mut pkt = [0u8; 64];
        pkt[0] = 0xAA;
        pkt[1] = 0x03; // info packet, not data
        let result = decode_data_packet(&pkt, false);
        assert!(matches!(result, Err(ProtocolError::NotDataPacket(0x03))));
    }

    #[test]
    fn test_try_decode_returns_none_for_non_data() {
        let mut pkt = [0u8; 64];
        pkt[0] = 0xAA;
        pkt[1] = 0x03;
        assert!(try_decode_data_packet(&pkt, false).is_none());
    }

    #[test]
    fn test_init_commands_fnb58() {
        let cmds = init_commands(true);
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0][1], 0x81);
        assert_eq!(cmds[1][1], 0x82);
        assert_eq!(cmds[2][1], 0x82); // FNB58 sends AA82 twice
    }

    #[test]
    fn test_init_commands_fnb48() {
        let cmds = init_commands(false);
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0][1], 0x81);
        assert_eq!(cmds[1][1], 0x82);
        assert_eq!(cmds[2][1], 0x83); // FNB48 sends AA83
    }
}
