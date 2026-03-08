//! Parser for `.cfn` offline recordings.
//!
//! CFN is a proprietary binary format used by the FNIRSI PC software to
//! store captured measurement sessions.  Each file contains a sample rate,
//! channel metadata, and a flat array of timestamped data points.

use crate::sample::Sample;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::Read;

/// Read a little-endian `f64` from `buf` at `*offset`, advancing the offset.
fn read_f64(buf: &[u8], offset: &mut usize) -> Result<f64> {
    if *offset + 8 > buf.len() {
        bail!("Unexpected EOF reading f64");
    }
    let val = f64::from_le_bytes(buf[*offset..*offset + 8].try_into().unwrap());
    *offset += 8;
    Ok(val)
}

/// Read a little-endian `u32` from `buf` at `*offset`, advancing the offset.
fn read_u32(buf: &[u8], offset: &mut usize) -> Result<u32> {
    if *offset + 4 > buf.len() {
        bail!("Unexpected EOF reading u32");
    }
    let val = u32::from_le_bytes(buf[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    Ok(val)
}

/// Read a little-endian `u16` from `buf` at `*offset`, advancing the offset.
fn read_u16(buf: &[u8], offset: &mut usize) -> Result<u16> {
    if *offset + 2 > buf.len() {
        bail!("Unexpected EOF reading u16");
    }
    let val = u16::from_le_bytes(buf[*offset..*offset + 2].try_into().unwrap());
    *offset += 2;
    Ok(val)
}

/// Read a single `u8` from `buf` at `*offset`, advancing the offset.
fn read_u8(buf: &[u8], offset: &mut usize) -> Result<u8> {
    if *offset + 1 > buf.len() {
        bail!("Unexpected EOF reading u8");
    }
    let val = buf[*offset];
    *offset += 1;
    Ok(val)
}

/// Parse a `.cfn` file into a `Vec<Sample>` and the original sample rate.
///
/// Returns `(samples, sample_rate_hz)`.  A zero or negative sample rate in
/// the file header is clamped to 100 Hz to prevent divide-by-zero errors.
pub fn read_cfn(path: &std::path::Path) -> Result<(Vec<Sample>, f64)> {
    let mut file = File::open(path).context("Failed to open CFN file")?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).context("Failed to read file")?;

    let mut offset = 0;

    let sample_rate = read_f64(&buf, &mut offset)?;
    let _start_curr = read_u32(&buf, &mut offset)?;
    let _stop_curr = read_u32(&buf, &mut offset)?;
    let _stop_time = read_u32(&buf, &mut offset)?;
    let ch_count = read_u16(&buf, &mut offset)?;

    let mut channel_types = Vec::new();
    for _ in 0..ch_count {
        let ch_type = read_u16(&buf, &mut offset)?;
        channel_types.push(ch_type);

        offset += 4; // skip 4 unknown bytes

        let has_min_max = read_u8(&buf, &mut offset)?;
        if has_min_max == 1 {
            offset += 16; // skip min/max bounds (8 bytes each)
        }
    }

    let pts_count = read_u32(&buf, &mut offset)?;

    let mut samples = Vec::with_capacity(pts_count as usize);
    for _ in 0..pts_count {
        let time = read_f64(&buf, &mut offset)?;

        let mut sample = Sample {
            timestamp_ms: (time * 1000.0) as u64,
            voltage_v: 0.0,
            current_a: 0.0,
            dp_v: 0.0,
            dn_v: 0.0,
            temp_c: 0.0,
            power_w: 0.0,
            raw_voltage: 0,
            raw_current: 0,
        };

        for &ch_type in &channel_types {
            let val = read_f64(&buf, &mut offset)? as f32;
            match ch_type {
                0 => sample.voltage_v = val,
                1 => sample.current_a = val,
                2 => sample.dp_v = val,
                3 => sample.dn_v = val,
                4 => sample.power_w = val,
                // Accumulated capacity (5) and energy (6) are ignored;
                // they are recalculated during playback.
                _ => {}
            }
        }
        samples.push(sample);
    }

    // Clamp to 100 Hz if the stored rate would cause division by zero.
    let final_rate = if sample_rate <= 0.0 {
        100.0
    } else {
        sample_rate
    };
    Ok((samples, final_rate))
}
