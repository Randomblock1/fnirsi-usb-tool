//! Read/write helpers for CSV, JSON Lines, and XLSX sample data.

use crate::sample::Sample;
use anyhow::Context;
use std::io::Write as _;

use serde::Serialize;

#[derive(Serialize)]
pub struct BleSampleView {
    pub timestamp_ms: u64,
    pub voltage_v: f32,
    pub current_a: f32,
    pub power_w: f32,
}

impl From<&Sample> for BleSampleView {
    fn from(s: &Sample) -> Self {
        Self {
            timestamp_ms: s.timestamp_ms,
            voltage_v: s.voltage_v,
            current_a: s.current_a,
            power_w: s.power_w,
        }
    }
}

/// Write samples as CSV. When `include_usb_fields` is false (BLE mode), the
/// `dp_v`, `dn_v`, `temp_c`, `raw_voltage`, and `raw_current` columns are
/// omitted from both the header and every data row.
pub fn write_csv<'a>(
    path: &std::path::Path,
    samples: impl Iterator<Item = &'a Sample>,
    include_usb_fields: bool,
) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
    for s in samples {
        if include_usb_fields {
            wtr.serialize(s)?;
        } else {
            wtr.serialize(BleSampleView::from(s))?;
        }
    }
    wtr.flush()?;
    Ok(())
}

/// Write samples as JSON Lines. When `include_usb_fields` is false, USB-only
/// fields (`dp_v`, `dn_v`, `temp_c`, `raw_voltage`, `raw_current`) are omitted.
pub fn write_jsonl<'a>(
    path: &std::path::Path,
    samples: impl Iterator<Item = &'a Sample>,
    include_usb_fields: bool,
) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut wtr = std::io::BufWriter::new(file);
    for s in samples {
        let line = if include_usb_fields {
            serde_json::to_string(s)?
        } else {
            serde_json::to_string(&BleSampleView::from(s))?
        };
        wtr.write_all(line.as_bytes())?;
        wtr.write_all(b"\n")?;
    }
    wtr.flush()?;
    Ok(())
}

/// Read samples from a CSV file. Fields absent from the header (e.g. BLE
/// exports that omit `dp_v`, `dn_v`, etc.) deserialise to their defaults (0).
pub fn read_csv(path: &std::path::Path) -> anyhow::Result<Vec<Sample>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut samples = Vec::new();
    for result in rdr.deserialize() {
        let s: Sample = result?;
        samples.push(s);
    }
    Ok(samples)
}

/// Read samples from a JSON Lines file. Missing keys default to 0 / 0.0.
pub fn read_jsonl(path: &std::path::Path) -> anyhow::Result<Vec<Sample>> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let rdr = std::io::BufReader::new(file);
    let mut samples = Vec::new();
    for line in rdr.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let s: Sample = serde_json::from_str(&line)?;
        samples.push(s);
    }
    Ok(samples)
}

/// Write samples as an XLSX spreadsheet. When `include_usb_fields` is false, USB-only columns are omitted.
pub fn write_xlsx<'a>(
    path: &std::path::Path,
    samples: impl Iterator<Item = &'a Sample>,
    include_usb_fields: bool,
) -> anyhow::Result<()> {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let worksheet = workbook.add_worksheet();

    if include_usb_fields {
        worksheet.write_row(
            0,
            0,
            [
                "timestamp_ms",
                "voltage_v",
                "current_a",
                "power_w",
                "dp_v",
                "dn_v",
                "temp_c",
                "raw_voltage",
                "raw_current",
            ],
        )?;
    } else {
        worksheet.write_row(0, 0, ["timestamp_ms", "voltage_v", "current_a", "power_w"])?;
    }

    let mut row = 1;
    for s in samples {
        worksheet.write_number(row, 0, s.timestamp_ms as f64)?;
        worksheet.write_number(row, 1, f64::from(s.voltage_v))?;
        worksheet.write_number(row, 2, f64::from(s.current_a))?;
        worksheet.write_number(row, 3, f64::from(s.power_w))?;

        if include_usb_fields {
            worksheet.write_number(row, 4, f64::from(s.dp_v))?;
            worksheet.write_number(row, 5, f64::from(s.dn_v))?;
            worksheet.write_number(row, 6, f64::from(s.temp_c))?;
            worksheet.write_number(row, 7, f64::from(s.raw_voltage))?;
            worksheet.write_number(row, 8, f64::from(s.raw_current))?;
        }
        row += 1;
    }

    workbook.save(path)?;
    Ok(())
}

/// Read samples from an XLSX file. Missing fields deserialise to defaults (0).
pub fn read_xlsx(path: &std::path::Path) -> anyhow::Result<Vec<Sample>> {
    use calamine::{Data, DataType, Reader, Xlsx, open_workbook};
    let mut workbook: Xlsx<_> = open_workbook(path).context("Failed to open xlsx")?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .context("No sheets in workbook")?;

    let mut samples = Vec::new();

    if let Ok(range) = workbook.worksheet_range(&sheet_name) {
        let mut rows = range.rows();
        let header = rows.next().context("Empty sheet")?;

        let mut idx_map = std::collections::HashMap::new();
        for (i, cell) in header.iter().enumerate() {
            if let Some(s) = cell.get_string() {
                idx_map.insert(s, i);
            }
        }

        for row in rows {
            let get_num = |name: &str| -> f64 {
                idx_map
                    .get(name)
                    .and_then(|&idx| row.get(idx))
                    .and_then(|c: &Data| c.get_float().or_else(|| c.get_int().map(|i| i as f64)))
                    .unwrap_or(0.0)
            };

            samples.push(Sample {
                timestamp_ms: get_num("timestamp_ms") as u64,
                voltage_v: get_num("voltage_v") as f32,
                current_a: get_num("current_a") as f32,
                power_w: get_num("power_w") as f32,
                dp_v: get_num("dp_v") as f32,
                dn_v: get_num("dn_v") as f32,
                temp_c: get_num("temp_c") as f32,
                raw_voltage: get_num("raw_voltage") as u32,
                raw_current: get_num("raw_current") as u32,
            });
        }
    }

    Ok(samples)
}
