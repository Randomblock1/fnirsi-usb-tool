//! Read/write helpers for CSV, JSON Lines, XLSX, and optional Parquet sample data.

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
        if include_usb_fields {
            serde_json::to_writer(&mut wtr, s)?;
        } else {
            serde_json::to_writer(&mut wtr, &BleSampleView::from(s))?;
        }
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

/// Write samples as Parquet. Available with the `parquet` feature.
#[cfg(feature = "parquet")]
pub fn write_parquet<'a>(
    path: &std::path::Path,
    samples: impl Iterator<Item = &'a Sample>,
    include_usb_fields: bool,
) -> anyhow::Result<()> {
    use arrow_array::{ArrayRef, Float32Array, RecordBatch, UInt32Array, UInt64Array};
    use arrow_schema::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let samples: Vec<&Sample> = samples.collect();

    let mut fields = vec![
        Field::new("timestamp_ms", DataType::UInt64, false),
        Field::new("voltage_v", DataType::Float32, false),
        Field::new("current_a", DataType::Float32, false),
        Field::new("power_w", DataType::Float32, false),
    ];
    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from(
            samples
                .iter()
                .map(|sample| sample.timestamp_ms)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            samples
                .iter()
                .map(|sample| sample.voltage_v)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            samples
                .iter()
                .map(|sample| sample.current_a)
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            samples
                .iter()
                .map(|sample| sample.power_w)
                .collect::<Vec<_>>(),
        )),
    ];

    if include_usb_fields {
        fields.extend([
            Field::new("dp_v", DataType::Float32, false),
            Field::new("dn_v", DataType::Float32, false),
            Field::new("temp_c", DataType::Float32, false),
            Field::new("raw_voltage", DataType::UInt32, false),
            Field::new("raw_current", DataType::UInt32, false),
        ]);
        columns.extend([
            Arc::new(Float32Array::from(
                samples.iter().map(|sample| sample.dp_v).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(Float32Array::from(
                samples.iter().map(|sample| sample.dn_v).collect::<Vec<_>>(),
            )),
            Arc::new(Float32Array::from(
                samples
                    .iter()
                    .map(|sample| sample.temp_c)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt32Array::from(
                samples
                    .iter()
                    .map(|sample| sample.raw_voltage)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt32Array::from(
                samples
                    .iter()
                    .map(|sample| sample.raw_current)
                    .collect::<Vec<_>>(),
            )),
        ]);
    }

    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns)?;
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

/// Read samples from a Parquet file. Available with the `parquet` feature.
#[cfg(feature = "parquet")]
pub fn read_parquet(path: &std::path::Path) -> anyhow::Result<Vec<Sample>> {
    use arrow_array::{
        Array, Float32Array, Float64Array, Int32Array, Int64Array, RecordBatch, UInt32Array,
        UInt64Array,
    };
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    // A column's Arrow type is invariant across a RecordBatch, so resolve each
    // named column to a typed accessor once per batch and keep the per-cell
    // work in the row loop to a null check plus a value read. All six numeric
    // variants are retained so foreign parquet files (which may use wider or
    // signed integer/float widths than we write) still ingest.
    enum Col<'a> {
        F32(&'a Float32Array),
        F64(&'a Float64Array),
        U64(&'a UInt64Array),
        U32(&'a UInt32Array),
        I64(&'a Int64Array),
        I32(&'a Int32Array),
        Absent,
    }

    impl<'a> Col<'a> {
        fn resolve(batch: &'a RecordBatch, name: &str) -> Self {
            let Ok(idx) = batch.schema().index_of(name) else {
                return Col::Absent;
            };
            let any = batch.column(idx).as_any();
            if let Some(values) = any.downcast_ref::<Float32Array>() {
                return Col::F32(values);
            }
            if let Some(values) = any.downcast_ref::<Float64Array>() {
                return Col::F64(values);
            }
            if let Some(values) = any.downcast_ref::<UInt64Array>() {
                return Col::U64(values);
            }
            if let Some(values) = any.downcast_ref::<UInt32Array>() {
                return Col::U32(values);
            }
            if let Some(values) = any.downcast_ref::<Int64Array>() {
                return Col::I64(values);
            }
            if let Some(values) = any.downcast_ref::<Int32Array>() {
                return Col::I32(values);
            }
            Col::Absent
        }

        fn at(&self, row: usize) -> Option<f64> {
            match self {
                Col::F32(values) => (!values.is_null(row)).then(|| f64::from(values.value(row))),
                Col::F64(values) => (!values.is_null(row)).then(|| values.value(row)),
                Col::U64(values) => (!values.is_null(row)).then(|| values.value(row) as f64),
                Col::U32(values) => (!values.is_null(row)).then(|| f64::from(values.value(row))),
                Col::I64(values) => (!values.is_null(row)).then(|| values.value(row) as f64),
                Col::I32(values) => (!values.is_null(row)).then(|| f64::from(values.value(row))),
                Col::Absent => None,
            }
        }
    }

    let file = std::fs::File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut samples = Vec::new();

    for batch in reader {
        let batch = batch?;
        let timestamp = Col::resolve(&batch, "timestamp_ms");
        let voltage = Col::resolve(&batch, "voltage_v");
        let current = Col::resolve(&batch, "current_a");
        let power = Col::resolve(&batch, "power_w");
        let dp = Col::resolve(&batch, "dp_v");
        let dn = Col::resolve(&batch, "dn_v");
        let temp = Col::resolve(&batch, "temp_c");
        let raw_voltage = Col::resolve(&batch, "raw_voltage");
        let raw_current = Col::resolve(&batch, "raw_current");

        for row in 0..batch.num_rows() {
            samples.push(Sample {
                timestamp_ms: timestamp.at(row).unwrap_or(0.0) as u64,
                voltage_v: voltage.at(row).unwrap_or(0.0) as f32,
                current_a: current.at(row).unwrap_or(0.0) as f32,
                power_w: power.at(row).unwrap_or(0.0) as f32,
                dp_v: dp.at(row).unwrap_or(0.0) as f32,
                dn_v: dn.at(row).unwrap_or(0.0) as f32,
                temp_c: temp.at(row).unwrap_or(0.0) as f32,
                raw_voltage: raw_voltage.at(row).unwrap_or(0.0) as u32,
                raw_current: raw_current.at(row).unwrap_or(0.0) as u32,
            });
        }
    }

    Ok(samples)
}

#[cfg(test)]
mod jsonl_tests {
    use super::{read_jsonl, write_jsonl};
    use crate::sample::Sample;

    fn samples() -> [Sample; 2] {
        [
            Sample {
                timestamp_ms: 100,
                voltage_v: 5.1,
                current_a: 1.2,
                power_w: 6.12,
                dp_v: 0.8,
                dn_v: 0.1,
                temp_c: 31.5,
                raw_voltage: 510_000,
                raw_current: 120_000,
            },
            Sample {
                timestamp_ms: 110,
                voltage_v: 9.0,
                current_a: 2.0,
                power_w: 18.0,
                dp_v: 0.0,
                dn_v: 0.0,
                temp_c: 32.0,
                raw_voltage: 900_000,
                raw_current: 200_000,
            },
        ]
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "fnirsi-protocol-{tag}-{unique}-{}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn jsonl_round_trip_preserves_samples_with_usb_fields() {
        let samples = samples();
        let path = temp_path("jsonl-usb");

        write_jsonl(&path, samples.iter(), true).unwrap();
        let decoded = read_jsonl(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.len(), samples.len());
        for (a, b) in decoded.iter().zip(samples.iter()) {
            assert_eq!(a.timestamp_ms, b.timestamp_ms);
            assert!((a.voltage_v - b.voltage_v).abs() < f32::EPSILON);
            assert!((a.current_a - b.current_a).abs() < f32::EPSILON);
            assert!((a.power_w - b.power_w).abs() < f32::EPSILON);
            assert!((a.dp_v - b.dp_v).abs() < f32::EPSILON);
            assert!((a.dn_v - b.dn_v).abs() < f32::EPSILON);
            assert!((a.temp_c - b.temp_c).abs() < f32::EPSILON);
            assert_eq!(a.raw_voltage, b.raw_voltage);
            assert_eq!(a.raw_current, b.raw_current);
        }
    }

    #[test]
    fn jsonl_round_trip_preserves_samples_without_usb_fields() {
        let samples = samples();
        let path = temp_path("jsonl-ble");

        write_jsonl(&path, samples.iter(), false).unwrap();
        let decoded = read_jsonl(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.len(), samples.len());
        for (a, b) in decoded.iter().zip(samples.iter()) {
            assert_eq!(a.timestamp_ms, b.timestamp_ms);
            assert!((a.voltage_v - b.voltage_v).abs() < f32::EPSILON);
            assert!((a.current_a - b.current_a).abs() < f32::EPSILON);
            assert!((a.power_w - b.power_w).abs() < f32::EPSILON);
            // USB-only fields are omitted from BLE output, so they deserialize to defaults.
            assert!(a.dp_v.abs() < f32::EPSILON);
            assert!(a.dn_v.abs() < f32::EPSILON);
            assert!(a.temp_c.abs() < f32::EPSILON);
            assert_eq!(a.raw_voltage, 0);
            assert_eq!(a.raw_current, 0);
        }
    }
}

#[cfg(all(test, feature = "parquet"))]
mod tests {
    use super::{read_parquet, write_parquet};
    use crate::sample::Sample;

    #[test]
    fn parquet_round_trip_preserves_samples() {
        let samples = [
            Sample {
                timestamp_ms: 100,
                voltage_v: 5.1,
                current_a: 1.2,
                power_w: 6.12,
                dp_v: 0.8,
                dn_v: 0.1,
                temp_c: 31.5,
                raw_voltage: 510_000,
                raw_current: 120_000,
            },
            Sample {
                timestamp_ms: 110,
                voltage_v: 9.0,
                current_a: 2.0,
                power_w: 18.0,
                dp_v: 0.0,
                dn_v: 0.0,
                temp_c: 32.0,
                raw_voltage: 900_000,
                raw_current: 200_000,
            },
        ];

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fnirsi-protocol-{unique}-{}.parquet",
            std::process::id()
        ));

        write_parquet(&path, samples.iter(), true).unwrap();
        let decoded = read_parquet(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.len(), samples.len());
        assert_eq!(decoded[0].timestamp_ms, samples[0].timestamp_ms);
        assert_eq!(decoded[0].raw_current, samples[0].raw_current);
        assert_eq!(decoded[1].timestamp_ms, samples[1].timestamp_ms);
        assert_eq!(decoded[1].raw_voltage, samples[1].raw_voltage);
        assert!((decoded[0].voltage_v - samples[0].voltage_v).abs() < f32::EPSILON);
        assert!((decoded[1].power_w - samples[1].power_w).abs() < f32::EPSILON);
    }

    #[test]
    fn parquet_missing_usb_columns_default_to_zero() {
        // Source carries non-zero USB fields; BLE-mode write drops those columns
        // entirely, so reading back must resolve them to `Absent` and default to 0.
        let samples = [Sample {
            timestamp_ms: 42,
            voltage_v: 5.0,
            current_a: 1.0,
            power_w: 5.0,
            dp_v: 0.7,
            dn_v: 0.2,
            temp_c: 25.0,
            raw_voltage: 12_345,
            raw_current: 6_789,
        }];

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fnirsi-protocol-ble-{unique}-{}.parquet",
            std::process::id()
        ));

        write_parquet(&path, samples.iter(), false).unwrap();
        let decoded = read_parquet(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.len(), 1);
        // Columns present in the BLE file are preserved.
        assert_eq!(decoded[0].timestamp_ms, 42);
        assert!((decoded[0].voltage_v - 5.0).abs() < f32::EPSILON);
        assert!((decoded[0].power_w - 5.0).abs() < f32::EPSILON);
        // Columns absent from the file default to zero.
        assert!(decoded[0].dp_v.abs() < f32::EPSILON);
        assert!(decoded[0].dn_v.abs() < f32::EPSILON);
        assert!(decoded[0].temp_c.abs() < f32::EPSILON);
        assert_eq!(decoded[0].raw_voltage, 0);
        assert_eq!(decoded[0].raw_current, 0);
    }
}
