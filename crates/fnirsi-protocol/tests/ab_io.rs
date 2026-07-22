//! A/B benchmark: XLSX import throughput.
//! Run with: `cargo test -p fnirsi-protocol --release --test ab_io -- --ignored --nocapture`

use fnirsi_protocol::Sample;
use fnirsi_protocol::csv_utils::{read_xlsx, write_xlsx};

#[test]
#[ignore = "A/B benchmark"]
#[allow(clippy::cast_precision_loss, clippy::suboptimal_flops)]
fn ab_read_xlsx_50k() {
    let n = 50_000_usize;
    let samples: Vec<Sample> = (0..n)
        .map(|i| Sample {
            timestamp_ms: (i as u64) * 10,
            voltage_v: 5.0 + (i % 100) as f32 * 0.001,
            current_a: 1.0,
            power_w: 5.0,
            dp_v: 2.9,
            dn_v: 0.1,
            temp_c: 25.0,
            raw_voltage: 500_000,
            raw_current: 100_000,
        })
        .collect();

    let path = std::env::temp_dir().join(format!("fnirsi-ab-io-{}.xlsx", std::process::id()));
    write_xlsx(&path, samples.iter(), true).unwrap();

    let mut total = std::time::Duration::ZERO;
    let iters = 5;
    for _ in 0..iters {
        let start = std::time::Instant::now();
        let decoded = read_xlsx(&path).unwrap();
        total += start.elapsed();
        assert_eq!(decoded.len(), n);
        std::hint::black_box(decoded);
    }
    let _ = std::fs::remove_file(&path);
    println!(
        "AB_RESULT read_xlsx_50k: {:.1} ms/read",
        total.as_secs_f64() * 1000.0 / f64::from(iters)
    );
}
