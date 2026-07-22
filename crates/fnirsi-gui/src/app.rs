use crate::plots::{PlotConfig, PlotState};
use eframe::egui;
use fnirsi_protocol::{
    DeviceType, SamplePacket, ble, cfn, csv_utils, device::DeviceInfo, sample::Sample,
    usb::UsbDevice,
};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const APP_TITLE: &str = "FNIRSI Power Meter";

/// Messages sent from the reader thread to the GUI.
///
/// `Sample`/`Packet` mirror the shape each transport actually produces (BLE
/// notifies one sample at a time, USB decodes 4 per HID report), so neither
/// reader thread needs to heap-allocate a `Vec` just to hand samples over.
pub enum DeviceMessage {
    Connected(DeviceInfo),
    Status(String),
    Sample(Sample),
    Packet(SamplePacket),
    Error(String),
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    Usb,
    Bluetooth,
}

impl std::fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usb => write!(f, "USB"),
            Self::Bluetooth => write!(f, "Bluetooth"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    #[default]
    Csv,
    Jsonl,
    Xlsx,
    #[cfg(feature = "parquet")]
    Parquet,
}

impl ExportFormat {
    const fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV (.csv)",
            Self::Jsonl => "JSON Lines (.jsonl)",
            Self::Xlsx => "Excel Spreadsheet (.xlsx)",
            #[cfg(feature = "parquet")]
            Self::Parquet => "Parquet (.parquet)",
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Jsonl => "jsonl",
            Self::Xlsx => "xlsx",
            #[cfg(feature = "parquet")]
            Self::Parquet => "parquet",
        }
    }

    const fn default_filename(self) -> &'static str {
        match self {
            Self::Csv => "fnirsi_log.csv",
            Self::Jsonl => "fnirsi_log.jsonl",
            Self::Xlsx => "fnirsi_log.xlsx",
            #[cfg(feature = "parquet")]
            Self::Parquet => "fnirsi_log.parquet",
        }
    }
}

/// Buffer size presets.
///
/// Each entry holds `(max_samples, display_label)`.  The per-sample overhead
/// is the `Sample` struct plus two `f32` values for the energy/capacity
/// side-buffers.
const SIZEOF_SAMPLE: usize = std::mem::size_of::<Sample>() + 2 * std::mem::size_of::<f32>();
const BUFFER_PRESETS: &[(usize, &str)] = &[
    (100_000 / SIZEOF_SAMPLE, "100 KB"),
    (500_000 / SIZEOF_SAMPLE, "500 KB"),
    (1_000_000 / SIZEOF_SAMPLE, "1 MB"),
    (10_000_000 / SIZEOF_SAMPLE, "10 MB"),
    (50_000_000 / SIZEOF_SAMPLE, "50 MB"),
    (100_000_000 / SIZEOF_SAMPLE, "100 MB"),
    (500_000_000 / SIZEOF_SAMPLE, "500 MB"),
    (1_000_000_000 / SIZEOF_SAMPLE, "1 GB"),
];

/// Sample rate dividers.
///
/// Each entry is `(divisor, label)`.  The divisor is applied to the native
/// ~100 Hz USB rate (or ~10 Hz BLE rate).
const RATE_PRESETS: &[(usize, &str)] = &[
    (1, "100 Hz"),
    (2, "50 Hz"),
    (5, "20 Hz"),
    (10, "10 Hz"),
    (50, "2 Hz"),
    (100, "1 Hz"),
];

/// Main application state.
pub struct FnirsiApp {
    connected: bool,
    device_info: Option<DeviceInfo>,
    rx: Option<mpsc::Receiver<DeviceMessage>>,
    stop_tx: Option<mpsc::Sender<()>>,
    plots: PlotState,
    latest: Option<Sample>,
    energy_ws: f64,
    capacity_as: f64,
    status: String,
    validate_crc: bool,
    /// Index into `BUFFER_PRESETS`.
    buffer_preset_idx: usize,
    /// Index into `RATE_PRESETS`.
    rate_preset_idx: usize,
    /// Native sample counter (used for downsampling).
    sample_counter: usize,
    plot_config: PlotConfig,
    lod_enabled: bool,
    circular_buffer: bool,
    paused: bool,
    connection_mode: ConnectionMode,
    /// Whether the export-format dialog is open.
    show_export_dialog: bool,
    /// Currently selected export format in the dialog.
    export_format: ExportFormat,
    /// Raw text the user types into the duration-limit field (e.g. "30s", "5m").
    duration_input: String,
    /// Parsed stop-after duration, set when connecting.
    duration_limit: Option<Duration>,
    /// Timestamp (ms) of the latest sample received; used to enforce the duration limit.
    recording_ms: u64,
    /// Absolute timestamp (ms) when the current duration measurement started.
    duration_start_ms: u64,
    /// Imported file name shown in the native window title.
    imported_file_name: Option<String>,
    /// Imported file stem reused as the default export basename.
    imported_file_stem: Option<String>,
    /// Last native window title applied to avoid redundant viewport commands.
    last_window_title: String,
    /// Handle to the background reader thread.
    reader_thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Indicates whether the GUI is shutting down and waiting for background tasks.
    shutting_down: bool,
}

impl FnirsiApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default_state()
    }

    /// State shared by the real constructor and tests, which have no
    /// `eframe::CreationContext` to pass in.
    fn default_state() -> Self {
        Self {
            connected: false,
            device_info: None,
            rx: None,
            stop_tx: None,
            plots: PlotState::new(BUFFER_PRESETS[3].0),
            latest: None,
            energy_ws: 0.0,
            capacity_as: 0.0,
            status: "Disconnected".to_string(),
            validate_crc: true,
            buffer_preset_idx: 3, // 10 MB default
            rate_preset_idx: 0,   // 100 Hz default
            sample_counter: 0,
            plot_config: PlotConfig {
                voltage: true,
                d_lines: false,
                current: true,
                power: true,
                temperature: true,
                energy: false,
                capacity: false,
            },
            lod_enabled: true,
            circular_buffer: false,
            paused: false,
            connection_mode: ConnectionMode::Usb,
            show_export_dialog: false,
            export_format: ExportFormat::Csv,
            duration_input: String::new(),
            duration_limit: None,
            recording_ms: 0,
            duration_start_ms: 0,
            imported_file_name: None,
            imported_file_stem: None,
            last_window_title: APP_TITLE.to_string(),
            reader_thread_handle: None,
            shutting_down: false,
        }
    }

    fn window_title(&self) -> String {
        self.imported_file_name.as_ref().map_or_else(
            || APP_TITLE.to_string(),
            |name| format!("{APP_TITLE} - {name}"),
        )
    }

    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = self.window_title();
        if title != self.last_window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_window_title = title;
        }
    }

    fn clear_imported_file(&mut self) {
        self.imported_file_name = None;
        self.imported_file_stem = None;
    }

    fn set_imported_file(&mut self, path: &Path) {
        self.imported_file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        self.imported_file_stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned);
    }

    fn export_default_filename(&self, fmt: ExportFormat) -> String {
        match &self.imported_file_stem {
            Some(stem) if !stem.is_empty() => format!("{stem}.{}", fmt.extension()),
            _ => fmt.default_filename().to_string(),
        }
    }

    /// Connect to a device and start the background reader thread.
    fn connect(&mut self) {
        self.reset_accumulators();
        self.paused = false;
        self.recording_ms = 0;
        self.duration_start_ms = 0;
        self.duration_limit = parse_duration_str(&self.duration_input);

        let (tx, rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let validate_crc = self.validate_crc;
        let mode = self.connection_mode;

        self.rx = Some(rx);
        self.stop_tx = Some(stop_tx);

        let handle = std::thread::spawn(move || match mode {
            ConnectionMode::Usb => reader_thread(tx, stop_rx, validate_crc),
            ConnectionMode::Bluetooth => ble_reader_thread(tx, stop_rx),
        });
        self.reader_thread_handle = Some(handle);

        self.status = "Connecting...".to_string();
    }

    /// Stop the background reader thread and mark the device as disconnected.
    fn disconnect(&mut self) {
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        self.rx = None;
        self.connected = false;
        self.device_info = None;
        self.status = "Disconnected".to_string();
    }

    /// Clear all plot data and energy/capacity accumulators.
    fn reset_accumulators(&mut self) {
        self.energy_ws = 0.0;
        self.capacity_as = 0.0;
        self.duration_start_ms = self.recording_ms;
        self.plots.clear();
        let new_cap = BUFFER_PRESETS[self.buffer_preset_idx].0;
        self.plots.set_capacity(new_cap);
        self.clear_imported_file();
    }

    /// Export the current buffer in the specified format via a save dialog.
    fn export_with_format(&self, fmt: ExportFormat) {
        let samples = self.plots.samples();
        if samples.is_empty() {
            return;
        }

        let ext = fmt.extension();
        let file = rfd::FileDialog::new()
            .set_title(fmt.label())
            .add_filter(fmt.label(), &[ext])
            .set_file_name(self.export_default_filename(fmt))
            .save_file();

        if let Some(path) = file {
            let filtered = samples.iter().filter(|s| !s.voltage_v.is_nan());
            let result = match fmt {
                ExportFormat::Csv => csv_utils::write_csv(&path, filtered, true),
                ExportFormat::Jsonl => csv_utils::write_jsonl(&path, filtered, true),
                ExportFormat::Xlsx => csv_utils::write_xlsx(&path, filtered, true),
                #[cfg(feature = "parquet")]
                ExportFormat::Parquet => csv_utils::write_parquet(&path, filtered, true),
            };
            match result {
                Ok(()) => {
                    tracing::info!("Exported {} samples to {:?} ({ext})", samples.len(), path);
                }
                Err(e) => {
                    tracing::error!("Export failed: {e}");
                }
            }
        }
    }

    /// Import a data file (CSV, CFN, JSONL, XLSX), disconnecting if necessary.
    fn import_file(&mut self) {
        let dialog = rfd::FileDialog::new().set_title("Import Data File");
        #[cfg(feature = "parquet")]
        let dialog = dialog.add_filter(
            "Data files",
            &["csv", "cfn", "jsonl", "xlsx", "parquet", "parq"],
        );
        #[cfg(not(feature = "parquet"))]
        let dialog = dialog.add_filter("Data files", &["csv", "cfn", "jsonl", "xlsx"]);
        let file = dialog.pick_file();

        if let Some(path) = file {
            let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

            let res = if extension.eq_ignore_ascii_case("cfn") {
                cfn::read_cfn(&path)
            } else if extension.eq_ignore_ascii_case("xlsx") {
                csv_utils::read_xlsx(&path).map(|s| (s, 100.0))
            } else if extension.eq_ignore_ascii_case("parquet")
                || extension.eq_ignore_ascii_case("parq")
            {
                #[cfg(feature = "parquet")]
                {
                    csv_utils::read_parquet(&path).map(|s| (s, 100.0))
                }
                #[cfg(not(feature = "parquet"))]
                {
                    Err(anyhow::anyhow!(
                        "Parquet support is disabled. Rebuild fnirsi-gui with `--features parquet`."
                    ))
                }
            } else if extension.eq_ignore_ascii_case("jsonl") {
                csv_utils::read_jsonl(&path).map(|s| (s, 100.0))
            } else {
                csv_utils::read_csv(&path).map(|s| (s, 100.0))
            };

            match res {
                Ok((samples, sample_rate)) => {
                    tracing::info!("Imported {} samples from {:?}", samples.len(), path);
                    self.disconnect();
                    self.reset_accumulators();
                    self.set_imported_file(&path);

                    let default_dt = 1.0 / sample_rate;
                    let mut prev_sample: Option<Sample> = None;
                    self.plots.reserve(samples.len());
                    for s in samples {
                        let e_wh = self.energy_ws / 3600.0;
                        let c_mah = self.capacity_as / 3.6;
                        self.plots.push_unlimited(&s, e_wh, c_mah);

                        let (dt, avg_power, avg_current) = prev_sample.map_or_else(
                            || (default_dt, f64::from(s.power_w), f64::from(s.current_a)),
                            |prev| {
                                let dt_ms = s.timestamp_ms.saturating_sub(prev.timestamp_ms);
                                let dt = if dt_ms > 0 {
                                    dt_ms as f64 / 1000.0
                                } else {
                                    default_dt
                                };
                                (
                                    dt,
                                    f64::midpoint(f64::from(s.power_w), f64::from(prev.power_w)),
                                    f64::midpoint(
                                        f64::from(s.current_a),
                                        f64::from(prev.current_a),
                                    ),
                                )
                            },
                        );

                        self.energy_ws += avg_power * dt;
                        self.capacity_as += avg_current * dt;
                        prev_sample = Some(s);
                    }
                    self.status = format!("Imported: {}", path.display());
                }
                Err(e) => {
                    tracing::error!("Import failed: {e}");
                    self.status = format!("Import error: {e}");
                }
            }
        }
    }

    /// Drain incoming `DeviceMessage`s from the reader thread.
    fn process_messages(&mut self) {
        // Take the receiver out so `self` isn't borrowed while match arms below
        // call back into `&mut self` (e.g. `process_sample_batch`); put it back
        // once drained.
        let Some(rx) = self.rx.take() else {
            return;
        };
        while let Ok(msg) = rx.try_recv() {
            match msg {
                DeviceMessage::Status(s) => {
                    self.status = s;
                }
                DeviceMessage::Connected(info) => {
                    self.status = format!("Connected: {}", info.device_type);
                    self.device_info = Some(info);
                    self.connected = true;
                }
                // BLE delivers one sample per notification, USB decodes 4 per HID
                // report; both feed the same per-sample logic over a slice so it
                // isn't duplicated per transport.
                DeviceMessage::Sample(s) => self.process_sample_batch(std::slice::from_ref(&s)),
                DeviceMessage::Packet(packet) => self.process_sample_batch(&packet),
                DeviceMessage::Error(e) => {
                    self.status = format!("Error: {e}");
                    self.connected = false;
                }
                DeviceMessage::Disconnected => {
                    self.status = "Disconnected".to_string();
                    self.connected = false;
                }
            }
        }
        self.rx = Some(rx);
    }

    /// Process one arrived batch of samples (a single BLE notification or one
    /// USB packet) through the shared per-sample logic.
    fn process_sample_batch(&mut self, batch: &[Sample]) {
        let divider = RATE_PRESETS[self.rate_preset_idx].0;
        for &s in batch {
            if !self.process_one_sample(s, divider) {
                // Duration limit was hit; skip remaining samples in this batch.
                break;
            }
        }

        // Update status with duration progress if a limit is active.
        if self.connected
            && !self.paused
            && let Some(limit) = self.duration_limit
        {
            let elapsed_ms = self.recording_ms.saturating_sub(self.duration_start_ms);
            let elapsed_secs = elapsed_ms as f64 / 1000.0;
            let limit_secs = limit.as_secs_f64();
            self.status = format!("Recording... {elapsed_secs:.1}s / {limit_secs:.1}s");
        }
    }

    /// Fold a single sample into the running energy/capacity totals and the
    /// plot buffers. Returns `false` if the duration limit was just reached,
    /// telling the caller to stop feeding it further samples from this batch.
    fn process_one_sample(&mut self, s: Sample, divider: usize) -> bool {
        // Check duration limit against sample timestamp so it triggers as soon
        // as the first over-limit sample arrives, not on the next GUI repaint
        // (which could be ~50ms later).
        if self.connected
            && !self.paused
            && let Some(limit) = self.duration_limit
        {
            let elapsed_ms = s.timestamp_ms.saturating_sub(self.duration_start_ms);
            if elapsed_ms >= limit.as_millis() as u64 {
                tracing::info!("Duration limit reached ({limit:?}), pausing.");
                self.status = format!(
                    "Duration elapsed ({}), paused.",
                    self.duration_input.trim()
                );
                self.paused = true;
                // Insert a NaN sentinel to visually break the plot line here.
                let mut sentinel = s;
                sentinel.timestamp_ms += 1;
                sentinel.voltage_v = f32::NAN;
                sentinel.current_a = f32::NAN;
                sentinel.power_w = f32::NAN;
                sentinel.dp_v = f32::NAN;
                sentinel.dn_v = f32::NAN;
                sentinel.temp_c = f32::NAN;
                self.plots.push(&sentinel, f64::NAN, f64::NAN);
                self.latest = Some(s);
                return false;
            }
        }

        if !self.paused {
            let (dt, avg_power, avg_current) = self.latest.map_or_else(
                || (0.0, f64::from(s.power_w), f64::from(s.current_a)),
                |prev| {
                    (
                        s.timestamp_ms.saturating_sub(prev.timestamp_ms) as f64 / 1000.0,
                        f64::midpoint(f64::from(s.power_w), f64::from(prev.power_w)),
                        f64::midpoint(f64::from(s.current_a), f64::from(prev.current_a)),
                    )
                },
            );

            if dt > 0.0 {
                self.energy_ws += avg_power * dt;
                self.capacity_as += avg_current * dt;
            }

            if self.sample_counter.is_multiple_of(divider)
                && (self.circular_buffer || self.plots.sample_count() < self.plots.capacity())
            {
                let e_wh = self.energy_ws / 3600.0;
                let c_mah = self.capacity_as / 3.6;
                self.plots.push(&s, e_wh, c_mah);
            }
            self.sample_counter = self.sample_counter.wrapping_add(1);
        }
        self.latest = Some(s);
        self.recording_ms = s.timestamp_ms;
        true
    }
}

impl eframe::App for FnirsiApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
        if let Some(handle) = self.reader_thread_handle.take() {
            let _ = handle.join();
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested())
            && self
                .reader_thread_handle
                .as_ref()
                .is_some_and(|h| !h.is_finished())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.shutting_down {
                self.shutting_down = true;
                // Trigger disconnect to gracefully terminate the stream but don't block
                if let Some(stop) = self.stop_tx.take() {
                    let _ = stop.send(());
                }
                self.rx = None;
                self.connected = false;
                self.device_info = None;
            }
        }

        if self.shutting_down {
            if self
                .reader_thread_handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
            {
                self.reader_thread_handle.take();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }

            egui::CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Disconnecting...").heading());
                });
            });
            ctx.request_repaint();
            return;
        }

        self.process_messages();
        self.sync_window_title(ctx);

        // Repaint periodically when waiting for connections, otherwise wait for GUI interaction.
        // Use a slower rate when paused (no new data arriving) to reduce unnecessary work.
        if self.connected || self.rx.is_some() {
            let interval = if self.paused { 200 } else { 50 };
            ctx.request_repaint_after(Duration::from_millis(interval));
        }

        // Top panel
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("FNIRSI Power Meter");
                ui.separator();

                egui::ComboBox::from_id_salt("connection_mode")
                    .width(0.0)
                    .selected_text(self.connection_mode.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.connection_mode, ConnectionMode::Usb, "USB");
                        ui.selectable_value(
                            &mut self.connection_mode,
                            ConnectionMode::Bluetooth,
                            "Bluetooth",
                        );
                    })
                    .response
                    .on_hover_text("Select connection interface");

                // Connect / Disconnect
                if self.connected {
                    if ui
                        .button("⏹ Disconnect")
                        .on_hover_text("Disconnect from the device and stop streaming")
                        .clicked()
                    {
                        self.disconnect();
                    }

                    let pause_text = if self.paused {
                        "▶ Resume"
                    } else {
                        "⏸ Pause"
                    };
                    if ui
                        .button(pause_text)
                        .on_hover_text(
                            "Pause or resume the live graph (data is ignored while paused)",
                        )
                        .clicked()
                    {
                        self.paused = !self.paused;
                        if self.paused {
                            self.status = format!("Paused (at {})", self.status.replace("Recording... ", ""));
                        } else {
                            self.duration_start_ms = self.recording_ms;
                            if let Some(limit) = self.duration_limit {
                                let elapsed_ms =
                                    self.recording_ms.saturating_sub(self.duration_start_ms);
                                self.status = format!(
                                    "Recording... {:.1}s / {:.1}s",
                                    elapsed_ms as f64 / 1000.0,
                                    limit.as_secs_f64()
                                );
                            } else {
                                self.status = "Recording...".to_string();
                            }
                        }

                        // Insert a NaN sentinel to visually break the line.
                        if self.paused
                            && let Some(mut last) = self.latest {
                                last.timestamp_ms += 1;
                                last.voltage_v = f32::NAN;
                                last.current_a = f32::NAN;
                                last.power_w = f32::NAN;
                                last.dp_v = f32::NAN;
                                last.dn_v = f32::NAN;
                                last.temp_c = f32::NAN;
                                self.plots.push(&last, f64::NAN, f64::NAN);
                            }
                    }
                } else if self.rx.is_some() {
                    if ui
                        .button(format!("⏹ {}", self.status))
                        .on_hover_text("Cancel connection attempt")
                        .clicked()
                    {
                        self.disconnect();
                    }
                } else if ui
                    .button("▶ Connect")
                    .on_hover_text(
                        "Connect to the first available FNIRSI device and start streaming",
                    )
                    .clicked()
                {
                    self.connect();
                }

                if ui
                    .button("🔄 Reset")
                    .on_hover_text("Clear all plots and reset energy/capacity accumulators")
                    .clicked()
                {
                    self.reset_accumulators();
                }

                ui.checkbox(&mut self.validate_crc, "CRC").on_hover_text(
                    "Validate CRC-8 checksums on incoming packets.\nRejects corrupted data.",
                );

                ui.separator();

                let current_label = BUFFER_PRESETS[self.buffer_preset_idx].1;
                egui::ComboBox::from_id_salt("buffer_size")
                    .width(0.0)
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        for (i, (_, label)) in BUFFER_PRESETS.iter().enumerate() {
                            if ui
                                .selectable_value(&mut self.buffer_preset_idx, i, *label)
                                .changed()
                                && self.imported_file_name.is_none()
                            {
                                let new_cap = BUFFER_PRESETS[self.buffer_preset_idx].0;
                                self.plots.set_capacity(new_cap);
                            }
                        }
                    })
                    .response
                    .on_hover_text("Number of samples to keep in the plot buffer");

                ui.checkbox(&mut self.circular_buffer, "Circular")
                    .on_hover_text("If unchecked, stops recording when the plot buffer is full");
                ui.checkbox(&mut self.lod_enabled, "LOD").on_hover_text(
                    "Use zoom-aware min/max decimation to improve rendering performance for large datasets. Disable to always draw the full buffer.",
                );

                // Sample rate selector
                let divider = RATE_PRESETS[self.rate_preset_idx].0;
                let base_rate = match self.connection_mode {
                    ConnectionMode::Usb => 100.0,
                    ConnectionMode::Bluetooth => 10.0,
                };
                let current_rate = base_rate / (divider as f64);
                let current_rate_str = if current_rate >= 1.0 {
                    format!("{current_rate:.0} Hz")
                } else {
                    format!("{current_rate:.2} Hz")
                };

                egui::ComboBox::from_id_salt("sample_rate")
                    .width(0.0)
                    .selected_text(&current_rate_str)
                    .show_ui(ui, |ui| {
                        for (i, &(div, _)) in RATE_PRESETS.iter().enumerate() {
                            let rate = base_rate / (div as f64);
                            let label = if rate >= 1.0 {
                                format!("{rate:.0} Hz")
                            } else {
                                format!("{rate:.2} Hz")
                            };
                            ui.selectable_value(&mut self.rate_preset_idx, i, label);
                        }
                    })
                    .response
                    .on_hover_text("Plotting and logging sample rate (downsampling level)");

                // Duration limit input
                ui.label("⏱").on_hover_text(
                    "Stop after duration (e.g. 30 or 30s = 30 sec, 5m, 2h). Leave blank to run indefinitely.",
                );
                let dur_edit = egui::TextEdit::singleline(&mut self.duration_input)
                    .desired_width(48.0)
                    .hint_text("∞ s");
                let dur_response = ui.add(dur_edit);
                if dur_response.changed() {
                    self.duration_limit = parse_duration_str(&self.duration_input);
                }
                if dur_response.hovered() {
                    ui.ctx().clone().output_mut(|o| {
                        o.cursor_icon = egui::CursorIcon::Text;
                    });
                }
                // Paint a coloured outline: green = valid, red = invalid, none = empty.
                if !self.duration_input.trim().is_empty() {
                    let stroke_color = if self.duration_limit.is_some() {
                        egui::Color32::from_rgb(80, 200, 80)
                    } else {
                        egui::Color32::from_rgb(220, 60, 60)
                    };
                    ui.painter().rect_stroke(
                        dur_response.rect,
                        egui::CornerRadius::same(3),
                        egui::Stroke::new(2.0, stroke_color),
                        egui::StrokeKind::Outside,
                    );
                }
                dur_response.on_hover_text("Auto-pause after this duration (e.g. 30s, 5m, 2h 10m 7s). Leave blank to run indefinitely.");

                ui.separator();

                let button_response = ui.button("📈 Show/Hide Graphs");
                egui::Popup::menu(&button_response)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        let mut toggle = |b: &mut bool, label: &str, tooltip: &str| {
                            ui.checkbox(b, label).on_hover_text(tooltip);
                        };

                        toggle(&mut self.plot_config.voltage, "Voltage (V)", "Show Voltage Plot");
                        toggle(
                            &mut self.plot_config.d_lines,
                            "D+/D− Lines",
                            "Show D+/D− Data Lines on Voltage Plot",
                        );
                        toggle(&mut self.plot_config.current, "Current (A)", "Show Current Plot");
                        toggle(&mut self.plot_config.power, "Power (W)", "Show Power Plot");
                        toggle(
                            &mut self.plot_config.temperature,
                            "Temperature (°C)",
                            "Show Temperature Plot",
                        );
                        toggle(&mut self.plot_config.energy, "Energy (Wh)", "Show Energy Plot");
                        toggle(
                            &mut self.plot_config.capacity,
                            "Capacity (mAh)",
                            "Show Capacity Plot",
                        );
                    });

                ui.separator();

                // Export / import buttons.
                if ui
                    .button("💾 Export")
                    .on_hover_text("Export buffered samples (choose format)")
                    .clicked()
                    && !self.plots.samples().is_empty()
                {
                    self.show_export_dialog = true;
                }
                if ui
                    .button("📂 Import")
                    .on_hover_text(
                        "Import a CSV or CFN file and view it interactively (disconnects device)",
                    )
                    .clicked()
                {
                    self.import_file();
                }
            });
        });

        // Bottom panel: live readings + memory info
        egui::TopBottomPanel::bottom("status_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let status_label = ui.label(egui::RichText::new(&self.status).strong().color(
                    if self.connected {
                        egui::Color32::LIGHT_GREEN
                    } else {
                        ui.visuals().text_color()
                    },
                ));

                if self.connected
                    && let Some(ref info) = self.device_info
                {
                    status_label.on_hover_text(format!("{info}"));
                }

                ui.separator();

                if let Some(ref s) = self.latest {
                    let stat = |ui: &mut egui::Ui, name: &str, val: f64, unit: &str| {
                        ui.label(
                            egui::RichText::new(format!("{name}: {val:.3} {unit}")).monospace(),
                        );
                        ui.separator();
                    };

                    stat(ui, "V", f64::from(s.voltage_v), "V");
                    stat(ui, "I", f64::from(s.current_a), "A");
                    stat(ui, "P", f64::from(s.power_w), "W");

                    if self.connection_mode == ConnectionMode::Usb {
                        stat(ui, "D+", f64::from(s.dp_v), "V");
                        stat(ui, "D−", f64::from(s.dn_v), "V");
                        stat(ui, "T", f64::from(s.temp_c), "°C");
                    }

                    ui.label(
                        egui::RichText::new(format!(
                            "E: {:.3} Wh  C: {:.1} mAh",
                            self.energy_ws / 3600.0,
                            self.capacity_as / 3.6,
                        ))
                        .monospace(),
                    );
                }

                ui.separator();

                // Right-align memory and duration readouts.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mem = self.plots.memory_bytes();
                    let mem_str = if mem < 1000 {
                        format!("{mem} B")
                    } else if mem < 1000 * 1000 {
                        format!("{:.1} KB", mem as f64 / 1000.0)
                    } else {
                        format!("{:.1} MB", mem as f64 / (1000.0 * 1000.0))
                    };

                    let divider = RATE_PRESETS[self.rate_preset_idx].0;
                    let base_rate = match self.connection_mode {
                        ConnectionMode::Usb => 100.0,
                        ConnectionMode::Bluetooth => 10.0,
                    };
                    let saved_rate_hz = base_rate / (divider as f64);
                    let capacity_secs = self.plots.capacity() as f64 / saved_rate_hz;
                    let cap_str = if capacity_secs < 60.0 {
                        format!("{capacity_secs:.0}s")
                    } else if capacity_secs < 3600.0 {
                        format!("{:.1}m", capacity_secs / 60.0)
                    } else if capacity_secs < 86400.0 {
                        format!("{:.1}h", capacity_secs / 3600.0)
                    } else {
                        format!("{:.1}d", capacity_secs / 86400.0)
                    };

                    let elapsed_secs = self.recording_ms as f64 / 1000.0;
                    let elapsed_str = if elapsed_secs < 60.0 {
                        format!("{elapsed_secs:.0}s")
                    } else if elapsed_secs < 3600.0 {
                        format!(
                            "{}m {:.0}s",
                            (elapsed_secs / 60.0).floor(),
                            elapsed_secs % 60.0
                        )
                    } else if elapsed_secs < 86400.0 {
                        format!(
                            "{}h {}m {:.0}s",
                            (elapsed_secs / 3600.0).floor(),
                            ((elapsed_secs % 3600.0) / 60.0).floor(),
                            elapsed_secs % 60.0
                        )
                    } else {
                        format!(
                            "{}d {}h",
                            (elapsed_secs / 86400.0).floor(),
                            ((elapsed_secs % 86400.0) / 3600.0).floor()
                        )
                    };

                    if self.imported_file_name.is_some() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({mem_str})",
                                self.plots.sample_count(),
                            ))
                            .monospace()
                            .weak(),
                        )
                        .on_hover_text("Total samples loaded (est. memory)");
                    } else {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}/{} ({mem_str}, {elapsed_str} / {cap_str} max)",
                                self.plots.sample_count(),
                                self.plots.capacity(),
                            ))
                            .monospace()
                            .weak(),
                        )
                        .on_hover_text(
                            "Samples in buffer / capacity (est. memory, elapsed / max duration)",
                        );
                    }
                });
            });
        });

        // Export format dialog (modal window).
        if self.show_export_dialog {
            // Return (close_requested, maybe_format) to avoid borrow
            // conflicts with `Window::open`.
            let result = egui::Window::new("Export Data")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Choose export format:");
                    ui.add_space(6.0);
                    ui.radio_value(
                        &mut self.export_format,
                        ExportFormat::Csv,
                        ExportFormat::Csv.label(),
                    );
                    ui.radio_value(
                        &mut self.export_format,
                        ExportFormat::Jsonl,
                        ExportFormat::Jsonl.label(),
                    );
                    ui.radio_value(
                        &mut self.export_format,
                        ExportFormat::Xlsx,
                        ExportFormat::Xlsx.label(),
                    );
                    #[cfg(feature = "parquet")]
                    ui.radio_value(
                        &mut self.export_format,
                        ExportFormat::Parquet,
                        ExportFormat::Parquet.label(),
                    );
                    ui.add_space(8.0);
                    let mut close = false;
                    let mut export_fmt: Option<ExportFormat> = None;
                    ui.horizontal(|ui| {
                        if ui.button("📥 Export").clicked() {
                            export_fmt = Some(self.export_format);
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                    (close, export_fmt)
                });

            if let Some(inner) = result.and_then(|r| r.inner) {
                let (close, export_fmt) = inner;
                if close {
                    self.show_export_dialog = false;
                }
                if let Some(fmt) = export_fmt {
                    self.export_with_format(fmt);
                }
            }
        }

        // Controls hint panel (below the plots, above the status bar)
        egui::TopBottomPanel::bottom("controls_hint")
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "Controls: Scroll (+shift horizontal) • Ctrl+Scroll to zoom • Drag to pan • Right click drag to zoom to area • Double-click to reset",
                        )
                        .size(13.5),
                    );
                });
            });

        // Central panel: plots
        egui::CentralPanel::default()
            .frame({
                let mut f = egui::Frame::central_panel(ctx.style().as_ref());
                f.inner_margin.bottom = 0;
                f
            })
            .show(ctx, |ui| {
                let mut cfg = self.plot_config;
                if self.connection_mode != ConnectionMode::Usb {
                    cfg.d_lines = false;
                    cfg.temperature = false;
                }

                self.plots.show(ui, cfg, self.lod_enabled);
            });
    }
}

/// Background thread that connects to the device and streams samples.
#[allow(clippy::needless_pass_by_value)]
fn reader_thread(tx: mpsc::Sender<DeviceMessage>, stop_rx: mpsc::Receiver<()>, validate_crc: bool) {
    let mut device = match UsbDevice::connect_first() {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(DeviceMessage::Error(format!("Connect failed: {e}")));
            return;
        }
    };

    let _ = tx.send(DeviceMessage::Connected(device.info().clone()));

    if let Err(e) = device.start_streaming() {
        let _ = tx.send(DeviceMessage::Error(format!("Streaming failed: {e}")));
        return;
    }

    let mut sample_index: u64 = 0;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if let Err(e) = device.send_keepalive_if_needed() {
            let _ = tx.send(DeviceMessage::Error(format!("Keepalive error: {e}")));
            break;
        }

        match device.read_samples_timeout(Duration::from_secs(5), validate_crc) {
            Ok(Some(mut samples)) => {
                for (i, s) in samples.iter_mut().enumerate() {
                    s.timestamp_ms = (sample_index + i as u64) * 10;
                }
                sample_index += samples.len() as u64;
                if tx.send(DeviceMessage::Packet(samples)).is_err() {
                    return;
                }
            }
            Ok(None) => {}
            Err(e) => {
                let _ = tx.send(DeviceMessage::Error(format!("Read error: {e}")));
                break;
            }
        }
    }

    let _ = tx.send(DeviceMessage::Disconnected);
}

/// Background thread that connects to the device and streams samples over BLE.
#[allow(clippy::needless_pass_by_value)]
fn ble_reader_thread(tx: mpsc::Sender<DeviceMessage>, stop_rx: mpsc::Receiver<()>) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(DeviceMessage::Error(format!(
                "Failed to create tokio runtime: {e}"
            )));
            return;
        }
    };

    rt.block_on(async {
        let _ = tx.send(DeviceMessage::Status(
            "Scanning for Bluetooth devices (5s)...".to_string(),
        ));

        // Scan for available BLE devices.
        let devices = match ble::scan_devices(Duration::from_secs(5)).await {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(DeviceMessage::Error(format!("BLE Scan Error: {e}")));
                return;
            }
        };

        let Some(device) = devices.first() else {
            let _ = tx.send(DeviceMessage::Error(
                "No FNIRSI BLE devices found. Try again".to_string(),
            ));
            return;
        };

        let device_name = device.name.as_deref().unwrap_or("device");
        let _ = tx.send(DeviceMessage::Status(format!(
            "Connecting to {device_name}..."
        )));

        let tx_clone = tx.clone();
        let (mut rx, handle) =
            match ble::connect_and_stream(&device.address, Duration::from_secs(2), move |msg| {
                let _ = tx_clone.send(DeviceMessage::Status(msg));
            })
            .await
            {
                Ok(rx) => rx,
                Err(e) => {
                    let _ = tx.send(DeviceMessage::Error(format!("BLE Connect Error: {e}")));
                    return;
                }
            };

        let _ = tx.send(DeviceMessage::Connected(DeviceInfo {
            device_type: DeviceType::Fnb58, // Treat all BLE devices generically
            vid: 0,
            pid: 0,
            manufacturer: None,
            product: device.name.clone(),
            serial: None,
            path: None,
        }));

        let start_time = Instant::now();
        loop {
            // Check for a stop signal from the GUI.
            if stop_rx.try_recv().is_ok() {
                break;
            }

            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Some(mut sample)) => {
                    sample.timestamp_ms = start_time.elapsed().as_millis() as u64;
                    if tx.send(DeviceMessage::Sample(sample)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }

        drop(rx);
        let _ = handle.await;

        let _ = tx.send(DeviceMessage::Disconnected);
    });
}

/// Parse a human duration string (e.g. "30", "30s", "5m", "1m 30s", "1h 5m 20s")
/// using the `humantime` crate. Returns `None` if the string is empty or invalid.
/// A bare number with no alphabetic characters is treated as seconds (e.g. "30" → "30s").
fn parse_duration_str(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // If the string contains no letters, treat it as a number of seconds.
    if s.chars().any(char::is_alphabetic) {
        humantime::parse_duration(s).ok()
    } else {
        let with_unit = format!("{s}s");
        humantime::parse_duration(&with_unit).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(timestamp_ms: u64, voltage_v: f32, current_a: f32) -> Sample {
        Sample {
            timestamp_ms,
            voltage_v,
            current_a,
            power_w: voltage_v * current_a,
            dp_v: 0.0,
            dn_v: 0.0,
            temp_c: 0.0,
            raw_voltage: 0,
            raw_current: 0,
        }
    }

    /// `DeviceMessage` travels through an `mpsc` channel; each `send` heap-allocates
    /// one node sized for the largest variant. Measured here (64-bit): `Sample` =
    /// 40 B, `SamplePacket` ([Sample; 4]) = 160 B, so `Packet` is now that largest
    /// variant and `DeviceMessage` = 168 B (160 B payload + 8 B tag, 8-byte aligned).
    /// Before this change the largest variant was `Connected(DeviceInfo)` at ~104 B
    /// (dominated by 3x `Option<String>` + `Option<Vec<u8>>`), so every message sent
    /// through the channel got ~60 B bigger.
    ///
    /// That's a real, honest cost, but it doesn't add allocations: the channel
    /// node is one heap allocation either way. What it removes is the *second*
    /// allocation that used to ride along on the hot path -- `samples.to_vec()`
    /// on the USB side and `vec![sample]` on the BLE side, both now gone. USB
    /// packets arrive as a unit of 4 already-decoded samples, so `Packet` keeps
    /// that shape instead of exploding it into 4 separate `Sample` sends (which
    /// would avoid the size bump but quadruple the number of channel operations
    /// on the USB path for every packet). Given USB packets arrive at ~25 Hz and
    /// BLE samples at ~10 Hz, trading a fixed, inline size increase for one fewer
    /// allocation and fewer channel round-trips per packet is the right call here.
    #[test]
    fn device_message_size() {
        assert_eq!(std::mem::size_of::<Sample>(), 40);
        assert_eq!(std::mem::size_of::<SamplePacket>(), 160);
        assert_eq!(std::mem::size_of::<DeviceMessage>(), 168);
    }

    #[test]
    fn process_one_sample_accumulates_energy_and_updates_latest() {
        let mut app = FnirsiApp::default_state();
        app.connected = true;

        assert!(app.process_one_sample(sample(0, 5.0, 1.0), 1));
        assert_eq!(app.latest.map(|s| s.timestamp_ms), Some(0));
        assert!(app.energy_ws.abs() < 1e-9); // first sample has no preceding dt

        // 1000 ms later at the same power -> 5 W held for 1 s = 5 Ws of energy.
        assert!(app.process_one_sample(sample(1000, 5.0, 1.0), 1));
        assert!((app.energy_ws - 5.0).abs() < 1e-9);
        assert!((app.capacity_as - 1.0).abs() < 1e-9);
        assert_eq!(app.latest.map(|s| s.timestamp_ms), Some(1000));
        assert_eq!(app.recording_ms, 1000);
    }

    #[test]
    fn process_one_sample_stops_batch_at_duration_limit() {
        let mut app = FnirsiApp::default_state();
        app.connected = true;
        app.duration_limit = Some(Duration::from_millis(500));
        app.duration_start_ms = 0;

        assert!(app.process_one_sample(sample(0, 5.0, 1.0), 1));
        assert!(!app.paused);

        // Past the 500 ms limit: should pause and tell the caller to stop.
        let keep_going = app.process_one_sample(sample(600, 5.0, 1.0), 1);
        assert!(!keep_going);
        assert!(app.paused);
    }

    #[test]
    fn process_sample_batch_processes_every_sample_in_a_packet() {
        let mut app = FnirsiApp::default_state();
        app.connected = true;

        let packet: SamplePacket = [
            sample(0, 1.0, 1.0),
            sample(10, 1.0, 1.0),
            sample(20, 1.0, 1.0),
            sample(30, 1.0, 1.0),
        ];
        app.process_sample_batch(&packet);

        assert_eq!(app.recording_ms, 30);
        assert_eq!(app.sample_counter, 4);
    }
}
