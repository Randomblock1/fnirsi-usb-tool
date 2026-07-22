#![allow(
    clippy::missing_errors_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::similar_names,
    clippy::cast_sign_loss
)]

//! FNIRSI USB power meter protocol library.
//!
//! This crate implements the communication protocol for FNIRSI FNB48, FNB48S,
//! FNB58, C1, and FNAC28 USB power meters. It supports:
//!
//! - USB HID communication (via `hidapi`)
//! - Bluetooth LE communication (via `btleplug`)
//! - Measurement data decoding (voltage, current, D+/D−, temperature)
//! - DFU firmware updates

pub mod ble;
pub mod cfn;
pub mod crc;
pub mod csv_utils;
pub mod device;
pub mod dfu;
pub mod protocol;
pub mod sample;
pub mod usb;

// Re-export key types at crate root for convenience.
pub use device::{DeviceInfo, DeviceType};
pub use protocol::ProtocolError;
pub use sample::{Sample, SamplePacket};
