#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fnirsi_protocol::{Sample, ble, cfn, csv_utils, dfu, usb::UsbDevice};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

// Global Ctrl-C flag, set by a background thread listening for SIGINT.
static INTERRUPTED: std::sync::LazyLock<Arc<AtomicBool>> = std::sync::LazyLock::new(|| {
    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);
    std::thread::spawn(move || {
        if let Ok(rt) = tokio::runtime::Runtime::new() {
            rt.block_on(async {
                let _ = tokio::signal::ctrl_c().await;
                flag_clone.store(true, Ordering::SeqCst);
            });
        }
    });
    flag
});

#[derive(Parser)]
#[command(name = "fnirsi-cli")]
#[command(about = "FNIRSI USB power meter CLI tool")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List connected FNIRSI devices and show device info.
    Info,

    /// Stream measurement data from the device.
    ///
    /// Output format is inferred from the file extension of --output:
    ///   .csv   → CSV
    ///   .jsonl → JSON Lines
    ///   .xlsx  → Excel Spreadsheet
    ///   .parquet → Parquet (when built with `--features parquet`)
    ///
    /// If --output is not given, tab-separated text is printed to stdout.
    /// Use --json to print JSON Lines to stdout instead.
    Log {
        /// Output file path. Format inferred from extension (.csv, .jsonl, .xlsx, .parquet).
        #[arg(long)]
        output: Option<PathBuf>,

        /// Output JSON Lines to stdout (ignored if --output is set).
        #[arg(long)]
        json: bool,

        /// Use Bluetooth LE instead of USB.
        #[arg(long)]
        ble: bool,

        /// Duration to log (e.g. "30s", "5m"). Runs indefinitely if not set.
        #[arg(long)]
        duration: Option<String>,

        /// Enable CRC validation on received packets.
        #[arg(long)]
        crc: bool,

        /// Target output sample rate in Hz (e.g. 10, 1). Drops samples to match.
        /// USB native rate is ~100 Hz; BLE native rate is ~10 Hz.
        #[arg(long)]
        rate: Option<f64>,

        /// Stop after recording this many samples.
        #[arg(long)]
        max_samples: Option<u64>,
    },

    /// Flash firmware to the device (DFU update).
    Flash {
        /// Path to the .ufn firmware file.
        firmware: PathBuf,
    },

    /// Convert a CFN offline recording to another format.
    ///
    /// Output format is inferred from the file extension of OUTPUT:
    ///   .csv   → CSV (default)
    ///   .jsonl → JSON Lines
    ///   .xlsx  → Excel Spreadsheet
    ///   .parquet → Parquet (when built with `--features parquet`)
    Convert {
        /// Input file path (.cfn, .csv, .jsonl, .xlsx, .parquet).
        input: PathBuf,

        /// Output file path (.csv, .jsonl, .xlsx, .parquet).
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("fnirsi=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Info => cmd_info(),
        Commands::Log {
            output,
            json,
            ble,
            duration,
            crc,
            rate,
            max_samples,
        } => {
            if ble {
                cmd_log_ble(
                    output.as_deref(),
                    json,
                    duration.as_deref(),
                    rate,
                    max_samples,
                )
            } else {
                cmd_log_usb(
                    output.as_deref(),
                    json,
                    duration.as_deref(),
                    crc,
                    rate,
                    max_samples,
                )
            }
        }
        Commands::Flash { firmware } => cmd_flash(&firmware),
        Commands::Convert { input, output } => cmd_convert(&input, &output),
    }
}

fn cmd_info() -> Result<()> {
    println!(
        "{} {}",
        "FNIRSI Device Scanner".bold(),
        "─".repeat(40).dimmed()
    );

    let devices = UsbDevice::list_devices().context("Failed to enumerate USB devices")?;

    if devices.is_empty() {
        println!("{}", "  No FNIRSI devices found via USB.".yellow());
    } else {
        println!("  {} USB device(s) found:\n", devices.len().green().bold());
        for (i, dev) in devices.iter().enumerate() {
            println!(
                "  {}  {} (VID:{} PID:{})",
                format!("[{}]", i + 1).dimmed(),
                dev.device_type.to_string().cyan().bold(),
                format!("{:04X}", dev.vid).yellow(),
                format!("{:04X}", dev.pid).yellow(),
            );
            if let Some(ref mfr) = dev.manufacturer {
                println!("       Manufacturer: {}", mfr.dimmed());
            }
            if let Some(ref prod) = dev.product {
                println!("       Product:      {}", prod.dimmed());
            }
            if let Some(ref ser) = dev.serial {
                println!("       Serial:       {}", ser.dimmed());
            }
            println!();
        }
    }

    // Check for DFU mode devices
    let dfu_devices = UsbDevice::list_dfu_devices().unwrap_or_default();
    if !dfu_devices.is_empty() {
        println!(
            "  {} device(s) in DFU mode:\n",
            dfu_devices.len().yellow().bold()
        );
        for dev in &dfu_devices {
            println!(
                "    {} (VID:{} PID:{})",
                "DFU".red().bold(),
                format!("{:04X}", dev.vid).yellow(),
                format!("{:04X}", dev.pid).yellow(),
            );
        }
    }

    // Scan for BLE devices
    eprintln!("  {} Scanning for Bluetooth LE devices (3s)...", "●".cyan());
    let rt = tokio::runtime::Runtime::new()?;
    let ble_devices = rt
        .block_on(ble::scan_devices(Duration::from_secs(3)))
        .unwrap_or_default();

    if ble_devices.is_empty() {
        println!("{}", "  No FNIRSI BLE devices found.".yellow());
    } else {
        println!(
            "\n  {} BLE device(s) found:\n",
            ble_devices.len().green().bold()
        );
        for (i, dev) in ble_devices.iter().enumerate() {
            println!(
                "  {}  {} ({})",
                format!("[{}]", i + 1).dimmed(),
                dev.name.as_deref().unwrap_or("unknown").cyan().bold(),
                dev.address.yellow(),
            );
        }
    }

    Ok(())
}

/// Inferred output format from a file extension or stdout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Csv,
    Jsonl,
    Xlsx,
    #[cfg(feature = "parquet")]
    Parquet,
    /// JSON Lines written to stdout.
    JsonStdout,
    /// Tab-separated written to stdout.
    TabStdout,
}

impl OutputFormat {
    fn from_path(path: &std::path::Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        match ext.as_str() {
            "csv" => Ok(Self::Csv),
            "jsonl" | "ndjson" => Ok(Self::Jsonl),
            "xlsx" => Ok(Self::Xlsx),
            "parquet" | "parq" => {
                #[cfg(feature = "parquet")]
                {
                    Ok(Self::Parquet)
                }
                #[cfg(not(feature = "parquet"))]
                {
                    anyhow::bail!(
                        "Parquet support is disabled. Rebuild fnirsi-cli with `--features parquet`."
                    )
                }
            }
            other => {
                anyhow::bail!(
                    "Unknown output extension '.{other}'. Use {}.",
                    supported_output_extensions()
                )
            }
        }
    }
}

const fn supported_output_extensions() -> &'static str {
    if cfg!(feature = "parquet") {
        ".csv, .jsonl, .xlsx, or .parquet"
    } else {
        ".csv, .jsonl, or .xlsx"
    }
}

/// Streaming output writer that dispatches to the selected format.
struct LogOutput {
    fmt: OutputFormat,
    /// Whether to include USB-only columns (`dp_v`, `dn_v`, `temp_c`, raw_*).
    include_usb_fields: bool,
    csv_writer: Option<csv::Writer<std::fs::File>>,
    jsonl_writer: Option<std::io::BufWriter<std::fs::File>>,
    buffered_output_path: Option<PathBuf>,
    buffered_samples: Vec<Sample>,
}

impl LogOutput {
    /// Create a new output writer for the given path / stdout mode.
    fn new(
        output: Option<&std::path::Path>,
        json_stdout: bool,
        include_usb_fields: bool,
    ) -> Result<Self> {
        let (fmt, csv_writer, jsonl_writer, buffered_output_path) = if let Some(path) = output {
            let fmt = OutputFormat::from_path(path)?;
            match fmt {
                OutputFormat::Csv => {
                    let w = csv::Writer::from_path(path).context("Failed to create CSV file")?;
                    (fmt, Some(w), None, None)
                }
                OutputFormat::Jsonl => {
                    let f = std::fs::File::create(path).context("Failed to create JSONL file")?;
                    (fmt, None, Some(std::io::BufWriter::new(f)), None)
                }
                OutputFormat::Xlsx => (fmt, None, None, Some(path.to_path_buf())),
                #[cfg(feature = "parquet")]
                OutputFormat::Parquet => (fmt, None, None, Some(path.to_path_buf())),
                _ => unreachable!(),
            }
        } else {
            let fmt = if json_stdout {
                OutputFormat::JsonStdout
            } else {
                OutputFormat::TabStdout
            };
            (fmt, None, None, None)
        };

        Ok(Self {
            fmt,
            include_usb_fields,
            csv_writer,
            jsonl_writer,
            buffered_output_path,
            buffered_samples: Vec::new(),
        })
    }

    /// Write a single sample to the output.
    fn write(&mut self, s: &Sample) -> Result<()> {
        use std::io::Write as _;
        match self.fmt {
            OutputFormat::Csv => {
                if let Some(w) = &mut self.csv_writer {
                    if self.include_usb_fields {
                        w.serialize(s)?;
                    } else {
                        w.serialize(fnirsi_protocol::csv_utils::BleSampleView::from(s))?;
                    }
                }
            }
            OutputFormat::Jsonl => {
                if let Some(w) = &mut self.jsonl_writer {
                    let line = if self.include_usb_fields {
                        serde_json::to_string(s)?
                    } else {
                        serde_json::to_string(&csv_utils::BleSampleView::from(s))?
                    };
                    w.write_all(line.as_bytes())?;
                    w.write_all(b"\n")?;
                }
            }
            OutputFormat::Xlsx => {
                self.buffered_samples.push(*s);
            }
            #[cfg(feature = "parquet")]
            OutputFormat::Parquet => {
                self.buffered_samples.push(*s);
            }
            OutputFormat::JsonStdout => {
                if self.include_usb_fields {
                    println!("{}", serde_json::to_string(s)?);
                } else {
                    println!(
                        "{}",
                        serde_json::to_string(&csv_utils::BleSampleView::from(s))?
                    );
                }
            }
            OutputFormat::TabStdout => {
                if self.include_usb_fields {
                    println!(
                        "{:.3}\t{:.5}\t{:.5}\t{:.5}\t{:.3}\t{:.3}\t{:.1}",
                        s.timestamp_ms as f64 / 1000.0,
                        s.voltage_v,
                        s.current_a,
                        s.power_w,
                        s.dp_v,
                        s.dn_v,
                        s.temp_c
                    );
                } else {
                    println!(
                        "{:.3}\t{:.5}\t{:.5}\t{:.5}",
                        s.timestamp_ms as f64 / 1000.0,
                        s.voltage_v,
                        s.current_a,
                        s.power_w,
                    );
                }
            }
        }
        Ok(())
    }

    /// Flush all buffered output.
    fn flush(&mut self) -> Result<()> {
        use std::io::Write as _;
        if let Some(w) = &mut self.csv_writer {
            w.flush()?;
        }
        if let Some(w) = &mut self.jsonl_writer {
            w.flush()?;
        }
        if let Some(path) = &self.buffered_output_path {
            match self.fmt {
                OutputFormat::Xlsx => {
                    fnirsi_protocol::csv_utils::write_xlsx(
                        path,
                        self.buffered_samples.iter(),
                        self.include_usb_fields,
                    )?;
                }
                #[cfg(feature = "parquet")]
                OutputFormat::Parquet => {
                    fnirsi_protocol::csv_utils::write_parquet(
                        path,
                        self.buffered_samples.iter(),
                        self.include_usb_fields,
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Stream live measurement data from a USB-connected device.
fn cmd_log_usb(
    output: Option<&std::path::Path>,
    json: bool,
    duration: Option<&str>,
    validate_crc: bool,
    rate: Option<f64>,
    max_samples: Option<u64>,
) -> Result<()> {
    let mut device = UsbDevice::connect_first().context("Failed to connect to FNIRSI device")?;
    let info = device.info().clone();

    eprintln!(
        "{} Connected to {} (VID:{:04X} PID:{:04X})",
        "✓".green().bold(),
        info.device_type.to_string().cyan(),
        info.vid,
        info.pid,
    );

    device
        .start_streaming()
        .context("Failed to start streaming")?;
    eprintln!("{} Streaming started", "✓".green().bold());

    let mut out = LogOutput::new(output, json, /* include_usb_fields = */ true)?;

    // Print stdout header for tab-separated mode.
    if out.fmt == OutputFormat::TabStdout {
        println!("time_s\tvoltage_V\tcurrent_A\tpower_W\tdp_V\tdn_V\ttemp_C");
    }

    let deadline = duration
        .map(parse_duration)
        .transpose()?
        .map(|d| std::time::Instant::now() + d);

    let mut energy_ws = 0.0_f64;
    let mut capacity_as = 0.0_f64;
    let mut last_time: Option<std::time::Instant> = None;
    // For rate-limiting: accumulate fractional credits so we emit exactly the
    // right number of samples even when the native batch size doesn't divide evenly.
    let native_rate = 100.0_f64; // USB native ~100 Hz
    // credit starts at 1.0 so the very first sample is always emitted.
    let mut rate_credit = 1.0_f64;
    let rate_step: f64 = rate.map_or(1.0, |r| r / native_rate).clamp(0.0, 1.0);
    let mut samples_written: u64 = 0;

    let start_time = std::time::Instant::now();

    loop {
        if let Some(dl) = deadline
            && std::time::Instant::now() >= dl
        {
            eprintln!("{} Duration elapsed, stopping.", "●".yellow());
            break;
        }

        device.send_keepalive_if_needed()?;

        if let Some(mut samples) =
            device.read_samples_timeout(Duration::from_secs(5), validate_crc)?
        {
            let now = std::time::Instant::now();
            let count = samples.len() as f64;
            let dt = last_time.map_or(0.0, |prev| now.duration_since(prev).as_secs_f64() / count);
            last_time = Some(now);

            let elapsed = start_time.elapsed().as_millis() as u64;
            for (i, s) in samples.iter_mut().enumerate() {
                s.timestamp_ms = elapsed.saturating_sub(40) + (i as u64 * 10);
                if dt > 0.0 {
                    energy_ws += f64::from(s.power_w) * dt;
                    capacity_as += f64::from(s.current_a) * dt;
                }
                rate_credit += rate_step;
                if rate_credit >= 1.0 {
                    rate_credit -= 1.0;
                    out.write(s)?;
                    samples_written += 1;
                    if max_samples.is_some_and(|m| samples_written >= m) {
                        eprintln!("{} Max samples reached, stopping.", "●".yellow());
                        out.flush()?;
                        return Ok(());
                    }
                }
            }
        }

        if is_interrupted() {
            eprintln!("\n{} Interrupted, stopping.", "●".yellow());
            break;
        }
    }

    out.flush()?;

    eprintln!(
        "{} Energy: {:.4} Wh  |  Capacity: {:.2} mAh",
        "●".cyan().bold(),
        energy_ws / 3600.0,
        capacity_as / 3.6,
    );

    Ok(())
}

/// Stream live measurement data from a BLE-connected device.
fn cmd_log_ble(
    output: Option<&std::path::Path>,
    json: bool,
    duration: Option<&str>,
    rate: Option<f64>,
    max_samples: Option<u64>,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        eprintln!("{} Scanning for BLE devices...", "●".cyan());

        let devices = ble::scan_devices(Duration::from_secs(5))
            .await
            .context("BLE scan failed")?;

        if devices.is_empty() {
            eprintln!("{}", "  No FNIRSI BLE devices found.".yellow());
            return Ok(());
        }

        for dev in &devices {
            eprintln!(
                "  Found: {} at {}",
                dev.name.as_deref().unwrap_or("unknown").cyan(),
                dev.address.yellow()
            );
        }

        let target = &devices[0];
        eprintln!(
            "{} Connecting to {}...",
            "●".cyan(),
            target.address.yellow()
        );

        let (mut rx, handle) = ble::connect_and_stream(&target.address, Duration::from_secs(3))
            .await
            .context("BLE connection failed")?;

        eprintln!("{} BLE streaming started", "✓".green().bold());

        let mut out = LogOutput::new(output, json, /* include_usb_fields = */ false)?;
        if out.fmt == OutputFormat::TabStdout {
            println!("time_s\tvoltage_V\tcurrent_A\tpower_W");
        }

        let deadline = duration
            .map(parse_duration)
            .transpose()?
            .map(|d| std::time::Instant::now() + d);

        let mut energy_ws = 0.0_f64;
        let mut capacity_as = 0.0_f64;
        let mut last_time: Option<std::time::Instant> = None;
        let native_ble_rate = 10.0_f64; // BLE native ~10 Hz
        let mut rate_credit = 1.0_f64;
        let rate_step: f64 = rate.map_or(1.0, |r| r / native_ble_rate).clamp(0.0, 1.0);
        let mut samples_written: u64 = 0;

        let start_time = std::time::Instant::now();
        loop {
            if let Some(dl) = deadline
                && std::time::Instant::now() >= dl
            {
                eprintln!("{} Duration elapsed, stopping.", "●".yellow());
                break;
            }

            if is_interrupted() {
                eprintln!("\n{} Interrupted, stopping.", "●".yellow());
                break;
            }

            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Some(mut sample)) => {
                    let now = std::time::Instant::now();
                    let dt = last_time.map_or(0.0, |p| now.duration_since(p).as_secs_f64());
                    last_time = Some(now);

                    sample.timestamp_ms = start_time.elapsed().as_millis() as u64;
                    energy_ws += f64::from(sample.power_w) * dt;
                    capacity_as += f64::from(sample.current_a) * dt;
                    rate_credit += rate_step;
                    if rate_credit >= 1.0 {
                        rate_credit -= 1.0;
                        out.write(&sample)?;
                        samples_written += 1;
                        if max_samples.is_some_and(|m| samples_written >= m) {
                            eprintln!("{} Max samples reached, stopping.", "●".yellow());
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }

        drop(rx);
        let _ = handle.await;

        out.flush()?;

        eprintln!(
            "{} Energy: {:.4} Wh  |  Capacity: {:.2} mAh",
            "●".cyan().bold(),
            energy_ws / 3600.0,
            capacity_as / 3.6,
        );

        Ok(())
    })
}

/// Flash a `.ufn` firmware file to a device in DFU mode.
fn cmd_flash(firmware_path: &std::path::Path) -> Result<()> {
    let firmware = dfu::read_firmware_file(firmware_path).context("Failed to read firmware")?;

    let version = parse_firmware_version(firmware_path).unwrap_or(0);

    eprintln!(
        "{} Loaded firmware: {} ({} bytes, version {})",
        "✓".green().bold(),
        firmware_path.display().cyan(),
        firmware.len().yellow(),
        version.yellow()
    );

    // Look for a device in DFU mode.
    let dfu_devices = UsbDevice::list_dfu_devices().unwrap_or_default();
    if dfu_devices.is_empty() {
        eprintln!("{}", "No device found in DFU mode.".red());
        eprintln!("Put the FNB58 into firmware update mode first.");
        return Ok(());
    }

    let api = hidapi::HidApi::new()?;
    let dfu_info = &dfu_devices[0];
    let dfu_dev = if let Some(ref path) = dfu_info.path {
        let cstr = std::ffi::CString::new(path.clone())?;
        api.open_path(&cstr)?
    } else {
        api.open(dfu_info.vid, dfu_info.pid)?
    };

    let pb = ProgressBar::new(firmware.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("━╸─"),
    );

    dfu::flash_firmware(&dfu_dev, &firmware, version, |written, _total| {
        pb.set_position(written as u64);
    })?;

    pb.finish_with_message("done");
    eprintln!("{} Firmware update complete!", "✓".green().bold());
    eprintln!(
        "{} The device does not automatically restart. Unplug and replug it to boot the new firmware.",
        "ℹ".cyan().bold()
    );

    Ok(())
}

/// Try to parse a firmware version from a filename like "Fnb58V1.11.ufn".
///
/// Extracts the digits after "V" and removes the dot, so "V1.11" → 111, "V0.68" → 68.
fn parse_firmware_version(path: &std::path::Path) -> Option<u16> {
    let stem = path.file_stem()?.to_str()?;
    let v_pos = stem.to_lowercase().find('v')?;
    let version_str = &stem[v_pos + 1..];
    // Remove dots and parse: "1.11" → "111", "0.68" → "068" → 68
    let digits: String = version_str.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Parse a human-readable duration string using `humantime`.
fn parse_duration(s: &str) -> Result<Duration> {
    humantime::parse_duration(s.trim()).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Check whether a Ctrl-C interrupt has been received.
fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

fn read_samples_from_path(path: &std::path::Path) -> Result<Vec<Sample>> {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    match extension.as_str() {
        "cfn" => cfn::read_cfn(path).map(|(samples, _rate)| samples),
        "csv" => csv_utils::read_csv(path),
        "jsonl" | "ndjson" => csv_utils::read_jsonl(path),
        "xlsx" => csv_utils::read_xlsx(path),
        "parquet" | "parq" => {
            #[cfg(feature = "parquet")]
            {
                csv_utils::read_parquet(path)
            }
            #[cfg(not(feature = "parquet"))]
            {
                anyhow::bail!(
                    "Parquet support is disabled. Rebuild fnirsi-cli with `--features parquet`."
                )
            }
        }
        _ => anyhow::bail!(
            "Unsupported input file format. Use .cfn, .csv, .jsonl, .xlsx{}.",
            if cfg!(feature = "parquet") {
                ", or .parquet"
            } else {
                ""
            }
        ),
    }
}

fn write_samples_to_path(path: &std::path::Path, samples: &[Sample]) -> Result<()> {
    match OutputFormat::from_path(path)? {
        OutputFormat::Csv => csv_utils::write_csv(path, samples.iter(), true),
        OutputFormat::Jsonl => csv_utils::write_jsonl(path, samples.iter(), true),
        OutputFormat::Xlsx => csv_utils::write_xlsx(path, samples.iter(), true),
        #[cfg(feature = "parquet")]
        OutputFormat::Parquet => csv_utils::write_parquet(path, samples.iter(), true),
        OutputFormat::JsonStdout | OutputFormat::TabStdout => unreachable!(),
    }
}

/// Convert a supported recording file to another structured format.
fn cmd_convert(input: &std::path::Path, output: &std::path::Path) -> Result<()> {
    println!(
        "{} {}",
        "FNIRSI Format Converter".bold(),
        "─".repeat(40).dimmed()
    );

    println!("{} Reading input file: {}", "➤".blue(), input.display());

    let samples = read_samples_from_path(input)?;
    println!("  {} Loaded {} samples", "✓".green(), samples.len());

    let out_ext = output
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    println!(
        "{} Writing {} to: {}",
        "➤".blue(),
        out_ext.to_uppercase(),
        output.display()
    );

    write_samples_to_path(output, &samples)?;

    println!("  {} Success!", "✓".green());
    Ok(())
}
