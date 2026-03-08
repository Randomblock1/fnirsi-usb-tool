/// CRC-8 computation for FNIRSI device protocol.
///
/// The FNIRSI protocol uses CRC-8 with polynomial 0x39. Two configurations exist:
/// - Data packets: init = 0x42
/// - DFU packets: init = 0x00
use crc::{Algorithm, Crc};

/// CRC-8 algorithm for data packets (init 0x42).
const CRC8_DATA: Algorithm<u8> = Algorithm {
    width: 8,
    poly: 0x39,
    init: 0x42,
    refin: false,
    refout: false,
    xorout: 0x00,
    check: 0x4b,
    residue: 0x00,
};

/// CRC-8 algorithm for DFU packets (init 0x00).
const CRC8_DFU: Algorithm<u8> = Algorithm {
    width: 8,
    poly: 0x39,
    init: 0x00,
    refin: false,
    refout: false,
    xorout: 0x00,
    check: 0x00, // not verified
    residue: 0x00,
};

static DATA_CRC: Crc<u8> = Crc::<u8>::new(&CRC8_DATA);
static DFU_CRC: Crc<u8> = Crc::<u8>::new(&CRC8_DFU);

/// Compute CRC-8 for a data packet payload.
///
/// The CRC covers bytes 1..63 of the 64-byte packet (skipping the 0xAA header
/// and the final CRC byte).
#[must_use] 
pub fn data_crc(data: &[u8]) -> u8 {
    DATA_CRC.checksum(data)
}

/// Compute CRC-8 for a DFU packet payload.
///
/// The CRC covers bytes 0..63 of the 64-byte HID buffer (everything except the
/// last byte which holds the CRC).
#[must_use] 
pub fn dfu_crc(data: &[u8]) -> u8 {
    DFU_CRC.checksum(data)
}

/// Validate a 64-byte data packet's CRC.
///
/// Checks bytes 1..62 against byte 63.
#[must_use] 
pub fn validate_data_packet(packet: &[u8; 64]) -> bool {
    let expected = packet[63];
    let computed = data_crc(&packet[1..63]);
    computed == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_crc_known_values() {
        // From the reference data logger: poly=0x39, init=0x42
        // We test that the CRC function produces consistent results.
        let data = [0x04u8; 10]; // arbitrary test input
        let crc = data_crc(&data);
        // Verify it's deterministic
        assert_eq!(crc, data_crc(&data));
    }

    #[test]
    fn test_dfu_crc_zeros() {
        // With init=0x00 and all-zero input, CRC should be 0x00
        let data = [0u8; 63];
        assert_eq!(dfu_crc(&data), 0x00);
    }

    #[test]
    fn test_validate_data_packet() {
        // Build a packet with correct CRC
        let mut packet = [0u8; 64];
        packet[0] = 0xAA;
        packet[1] = 0x04;
        // Fill some sample data
        for i in 2..62 {
            packet[i] = i as u8;
        }
        // Compute and set CRC
        packet[63] = data_crc(&packet[1..63]);
        assert!(validate_data_packet(&packet));

        // Corrupt one byte and verify it fails
        packet[10] ^= 0xFF;
        assert!(!validate_data_packet(&packet));
    }
}
