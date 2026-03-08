//! Measurement sample types.

use serde::{Deserialize, Serialize};

/// A single measurement sample from an FNIRSI USB power meter.
///
/// Fields marked `#[serde(default)]` may be absent in BLE-originated files
/// (which do not carry D+/D−, temperature, or raw ADC values). Missing fields
/// deserialize to 0 / 0.0 rather than failing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Sample {
    /// Timestamp in milliseconds (usually relative to stream start).
    pub timestamp_ms: u64,
    /// Voltage in volts.
    pub voltage_v: f32,
    /// Current in amps.
    pub current_a: f32,
    /// Power in watts (computed: voltage × current).
    pub power_w: f32,
    /// D+ data line voltage in volts (USB only; 0.0 for BLE).
    #[serde(default)]
    pub dp_v: f32,
    /// D− data line voltage in volts (USB only; 0.0 for BLE).
    #[serde(default)]
    pub dn_v: f32,
    /// Temperature in degrees Celsius (USB only; 0.0 for BLE).
    #[serde(default)]
    pub temp_c: f32,
    /// Raw voltage register value (u32 LE, unit = 10 µV; 0 for BLE).
    #[serde(default)]
    pub raw_voltage: u32,
    /// Raw current register value (u32 LE, unit = 10 µA; 0 for BLE).
    #[serde(default)]
    pub raw_current: u32,
}

/// A decoded data packet containing 4 samples.
pub type SamplePacket = [Sample; 4];
