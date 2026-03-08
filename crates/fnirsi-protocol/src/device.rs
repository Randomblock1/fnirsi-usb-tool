//! Device identification and enumeration.
//!
//! Maps USB VID/PID pairs to FNIRSI model names and selects the correct
//! protocol variant (FNB58-family vs FNB48-family).

use serde::Serialize;
use std::fmt;

/// Known FNIRSI device types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum DeviceType {
    Fnb48,
    Fnb48s,
    Fnb58,
    C1,
    Fnac28,
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fnb48 => write!(f, "FNB48"),
            Self::Fnb48s => write!(f, "FNB48S"),
            Self::Fnb58 => write!(f, "FNB58"),
            Self::C1 => write!(f, "C1"),
            Self::Fnac28 => write!(f, "FNAC28"),
        }
    }
}

/// USB Vendor/Product ID pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbId {
    pub vid: u16,
    pub pid: u16,
}

/// Known VID/PID pairs for data-mode devices.
pub const KNOWN_DEVICES: &[(UsbId, DeviceType)] = &[
    (
        UsbId {
            vid: 0x0483,
            pid: 0x003A,
        },
        DeviceType::Fnb48,
    ),
    (
        UsbId {
            vid: 0x2E3C,
            pid: 0x0049,
        },
        DeviceType::Fnb48s,
    ),
    (
        UsbId {
            vid: 0x2E3C,
            pid: 0x5558,
        },
        DeviceType::Fnb58,
    ),
    // C1 and FNAC28 share VID/PID 0483:003B; disambiguated by product string.
    (
        UsbId {
            vid: 0x0483,
            pid: 0x003B,
        },
        DeviceType::C1,
    ),
];

/// VID/PID for DFU (firmware update) mode.
pub const DFU_USB_ID: UsbId = UsbId {
    vid: 0x0483,
    pid: 0x0038,
};

/// Information about a connected device.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub device_type: DeviceType,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    #[serde(skip)]
    pub path: Option<Vec<u8>>,
}

impl fmt::Display for DeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (VID:{:04X} PID:{:04X})",
            self.device_type, self.vid, self.pid
        )?;
        if let Some(ref mfr) = self.manufacturer {
            write!(f, " mfr={mfr}")?;
        }
        if let Some(ref prod) = self.product {
            write!(f, " product={prod}")?;
        }
        if let Some(ref ser) = self.serial {
            write!(f, " serial={ser}")?;
        }
        Ok(())
    }
}

/// Detect device type from VID/PID, optionally using the product string to
/// distinguish C1 from FNAC28 (they share VID/PID 0483:003B).
#[must_use] 
pub fn detect_device_type(vid: u16, pid: u16, product_string: Option<&str>) -> Option<DeviceType> {
    for &(ref id, dtype) in KNOWN_DEVICES {
        if id.vid == vid && id.pid == pid {
            // C1 and FNAC28 share this VID/PID; check the product string.
            if vid == 0x0483 && pid == 0x003B
                && let Some(prod) = product_string {
                    if prod.to_lowercase().contains("c1") {
                        return Some(DeviceType::C1);
                    }
                    return Some(DeviceType::Fnac28);
                }
            return Some(dtype);
        }
    }
    None
}

/// Whether a device type uses the "FNB58/FNB48S" protocol variant.
///
/// These devices use VID 0x2E3C and require a different initialization sequence
/// and a 1-second keep-alive interval instead of 3 ms.
#[must_use] 
pub const fn is_fnb58_variant(device_type: DeviceType) -> bool {
    matches!(device_type, DeviceType::Fnb58 | DeviceType::Fnb48s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_fnb58() {
        assert_eq!(
            detect_device_type(0x2E3C, 0x5558, None),
            Some(DeviceType::Fnb58)
        );
    }

    #[test]
    fn test_detect_fnb48() {
        assert_eq!(
            detect_device_type(0x0483, 0x003A, None),
            Some(DeviceType::Fnb48)
        );
    }

    #[test]
    fn test_detect_c1_vs_fnac28() {
        assert_eq!(
            detect_device_type(0x0483, 0x003B, Some("FNIRSI C1")),
            Some(DeviceType::C1)
        );
        assert_eq!(
            detect_device_type(0x0483, 0x003B, Some("USB Tester")),
            Some(DeviceType::Fnac28)
        );
        // No product string defaults to C1 (first match in table)
        assert_eq!(
            detect_device_type(0x0483, 0x003B, None),
            Some(DeviceType::C1)
        );
    }

    #[test]
    fn test_detect_unknown() {
        assert_eq!(detect_device_type(0x1234, 0x5678, None), None);
    }

    #[test]
    fn test_is_fnb58_variant() {
        assert!(is_fnb58_variant(DeviceType::Fnb58));
        assert!(is_fnb58_variant(DeviceType::Fnb48s));
        assert!(!is_fnb58_variant(DeviceType::Fnb48));
        assert!(!is_fnb58_variant(DeviceType::C1));
    }
}
