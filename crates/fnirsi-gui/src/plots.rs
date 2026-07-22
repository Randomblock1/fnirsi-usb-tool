//! Real-time measurement plots.

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use fnirsi_protocol::Sample;
use std::collections::VecDeque;

const POINTS_PER_PIXEL: f32 = 8.0;

/// `[start, end)` bounds of decimation bucket `b` of `buckets` total, covering local
/// indices `[0, slice_len)`. Pure integer math (no float multiply/round per bucket).
/// `(b + 1) <= buckets` guarantees `end <= slice_len`, so callers never need to clamp.
fn bucket_bounds(b: usize, slice_len: usize, buckets: usize) -> (usize, usize) {
    (b * slice_len / buckets, (b + 1) * slice_len / buckets)
}

/// Emit one decimated bucket for a single lane: both extremes at the bucket-center
/// `x` (`lo` then `hi`), or a single NaN sentinel to open a gap when the fold never
/// saw a finite value (an all-NaN bucket leaves `lo == +INFINITY`).
///
/// Equal-x points render as the vertical bar that min-max decimation already draws.
fn emit_bucket(pts: &mut Vec<[f64; 2]>, x: f64, lo: f64, hi: f64) {
    if lo.is_infinite() {
        pts.push([x, f64::NAN]);
    } else {
        pts.push([x, lo]);
        pts.push([x, hi]);
    }
}

/// Hot-path subset of a [`Sample`] kept in the scanned plot buffer.
///
/// Drops the export-only raw ADC registers (`raw_voltage` / `raw_current`),
/// which are never read while plotting; those live in the parallel
/// `raw_adc` side-buffer and are re-joined only on export. Field names mirror
/// [`Sample`] so extraction closures and timestamp lookups read identically.
#[derive(Debug, Clone, Copy)]
struct PlotSample {
    timestamp_ms: u64,
    voltage_v: f32,
    current_a: f32,
    power_w: f32,
    dp_v: f32,
    dn_v: f32,
    temp_c: f32,
}

// The whole point of the hot/cold split: keep the scanned sample at 32 bytes.
const _: () = assert!(std::mem::size_of::<PlotSample>() == 32);

impl From<&Sample> for PlotSample {
    fn from(s: &Sample) -> Self {
        Self {
            timestamp_ms: s.timestamp_ms,
            voltage_v: s.voltage_v,
            current_a: s.current_a,
            power_w: s.power_w,
            dp_v: s.dp_v,
            dn_v: s.dn_v,
            temp_c: s.temp_c,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PlotConfig {
    pub voltage: bool,
    pub d_lines: bool,
    pub current: bool,
    pub power: bool,
    pub temperature: bool,
    pub energy: bool,
    pub capacity: bool,
}

/// Circular-buffered storage for the real-time measurement plots.
pub struct PlotState {
    all_samples: VecDeque<PlotSample>,
    /// Export-only raw ADC registers `(raw_voltage, raw_current)`, kept in
    /// lockstep with `all_samples` so the cold path can rebuild full `Sample`s.
    raw_adc: VecDeque<(u32, u32)>,
    energy_wh: VecDeque<f32>,
    capacity_mah: VecDeque<f32>,
    sample_capacity: usize,
    generation: u64,
    // Cached latest non-NaN values to avoid O(n) reverse scans each frame.
    latest_voltage: Option<f64>,
    latest_current: Option<f64>,
    latest_power: Option<f64>,
    latest_temp: Option<f64>,
    latest_energy: Option<f64>,
    latest_capacity: Option<f64>,
}

impl PlotState {
    /// Create a new plot state with the given sample capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            all_samples: VecDeque::with_capacity(capacity),
            raw_adc: VecDeque::with_capacity(capacity),
            energy_wh: VecDeque::with_capacity(capacity),
            capacity_mah: VecDeque::with_capacity(capacity),
            sample_capacity: capacity,
            generation: 0,
            latest_voltage: None,
            latest_current: None,
            latest_power: None,
            latest_temp: None,
            latest_energy: None,
            latest_capacity: None,
        }
    }

    /// Append a sample, discarding the oldest entry if the buffer is full.
    pub fn push(&mut self, sample: &Sample, energy_wh: f64, capacity_mah: f64) {
        if self.all_samples.len() >= self.sample_capacity {
            self.all_samples.pop_front();
            self.raw_adc.pop_front();
            self.energy_wh.pop_front();
            self.capacity_mah.pop_front();
        }
        self.all_samples.push_back(PlotSample::from(sample));
        self.raw_adc.push_back((sample.raw_voltage, sample.raw_current));
        self.energy_wh.push_back(energy_wh as f32);
        self.capacity_mah.push_back(capacity_mah as f32);

        // Update cached latest non-NaN values.
        if !sample.voltage_v.is_nan() {
            self.latest_voltage = Some(f64::from(sample.voltage_v));
        }
        if !sample.current_a.is_nan() {
            self.latest_current = Some(f64::from(sample.current_a));
        }
        if !sample.power_w.is_nan() {
            self.latest_power = Some(f64::from(sample.power_w));
        }
        if !sample.temp_c.is_nan() {
            self.latest_temp = Some(f64::from(sample.temp_c));
        }
        let ewh = energy_wh as f32;
        if !ewh.is_nan() {
            self.latest_energy = Some(energy_wh);
        }
        let cmah = capacity_mah as f32;
        if !cmah.is_nan() {
            self.latest_capacity = Some(capacity_mah);
        }
    }

    /// Pre-allocate storage for `additional` more samples ahead of a known-size bulk import,
    /// so `push_unlimited` doesn't repeatedly reallocate and copy as the deques grow.
    pub fn reserve(&mut self, additional: usize) {
        self.all_samples.reserve_exact(additional);
        self.raw_adc.reserve_exact(additional);
        self.energy_wh.reserve_exact(additional);
        self.capacity_mah.reserve_exact(additional);
    }

    /// Append a sample without checking or enforcing the `sample_capacity`.
    /// Used when importing existing files to show the complete dataset.
    pub fn push_unlimited(&mut self, sample: &Sample, energy_wh: f64, capacity_mah: f64) {
        self.all_samples.push_back(PlotSample::from(sample));
        self.raw_adc.push_back((sample.raw_voltage, sample.raw_current));
        self.energy_wh.push_back(energy_wh as f32);
        self.capacity_mah.push_back(capacity_mah as f32);

        // Update cached latest non-NaN values.
        if !sample.voltage_v.is_nan() {
            self.latest_voltage = Some(f64::from(sample.voltage_v));
        }
        if !sample.current_a.is_nan() {
            self.latest_current = Some(f64::from(sample.current_a));
        }
        if !sample.power_w.is_nan() {
            self.latest_power = Some(f64::from(sample.power_w));
        }
        if !sample.temp_c.is_nan() {
            self.latest_temp = Some(f64::from(sample.temp_c));
        }
        let ewh = energy_wh as f32;
        if !ewh.is_nan() {
            self.latest_energy = Some(energy_wh);
        }
        let cmah = capacity_mah as f32;
        if !cmah.is_nan() {
            self.latest_capacity = Some(capacity_mah);
        }
    }

    pub fn clear(&mut self) {
        self.all_samples.clear();
        self.raw_adc.clear();
        self.energy_wh.clear();
        self.capacity_mah.clear();
        self.generation += 1;
        self.latest_voltage = None;
        self.latest_current = None;
        self.latest_power = None;
        self.latest_temp = None;
        self.latest_energy = None;
        self.latest_capacity = None;
    }

    /// Resize the buffer, dropping oldest entries if needed.
    pub fn set_capacity(&mut self, new_capacity: usize) {
        self.sample_capacity = new_capacity;
        let excess = self.all_samples.len().saturating_sub(new_capacity);
        if excess > 0 {
            self.all_samples.drain(..excess);
            self.raw_adc.drain(..excess);
            self.energy_wh.drain(..excess);
            self.capacity_mah.drain(..excess);
        }
    }

    pub const fn capacity(&self) -> usize {
        self.sample_capacity
    }

    pub fn sample_count(&self) -> usize {
        self.all_samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all_samples.is_empty()
    }

    /// Retained bytes per stored sample: the hot [`PlotSample`] plus the parallel
    /// raw-ADC tuple and the energy/capacity side-buffer floats.
    ///
    /// Composition: 32 (`PlotSample`) + 8 (`(u32, u32)` raw ADC) + 8 (two `f32`
    /// side-buffers) = 48 bytes, matching the pre-split `size_of::<Sample>() + 8`.
    pub const BYTES_PER_SAMPLE: usize = std::mem::size_of::<PlotSample>()
        + std::mem::size_of::<(u32, u32)>()
        + 2 * std::mem::size_of::<f32>();

    fn visible_x_bounds(plot_ui: &egui_plot::PlotUi) -> Option<(f64, f64)> {
        if plot_ui.auto_bounds().x {
            return None;
        }

        let bounds = plot_ui.plot_bounds();
        bounds
            .is_finite_x()
            .then(|| (bounds.min()[0], bounds.max()[0]))
    }

    pub fn memory_bytes(&self) -> usize {
        self.all_samples.len() * Self::BYTES_PER_SAMPLE
    }

    /// Rebuild full protocol [`Sample`]s for export by zipping the hot buffer
    /// back together with the parallel raw-ADC side-buffer. Cold path — only
    /// reached from the export dialog, so the allocation is fine.
    pub fn export_samples(&self) -> Vec<Sample> {
        self.all_samples
            .iter()
            .zip(&self.raw_adc)
            .map(|(s, &(raw_voltage, raw_current))| Sample {
                timestamp_ms: s.timestamp_ms,
                voltage_v: s.voltage_v,
                current_a: s.current_a,
                power_w: s.power_w,
                dp_v: s.dp_v,
                dn_v: s.dn_v,
                temp_c: s.temp_c,
                raw_voltage,
                raw_current,
            })
            .collect()
    }

    /// Draw all enabled plots in a responsive grid layout.
    pub fn show(&self, ui: &mut egui::Ui, config: PlotConfig, lod_enabled: bool) {
        let active = [
            (config.voltage, "voltage_plot", "Voltage", "V"),
            (config.current, "current_plot", "Current", "A"),
            (config.power, "power_plot", "Power", "W"),
            (config.temperature, "temperature_plot", "Temperature", "°C"),
            (config.energy, "energy_plot", "Energy", "Wh"),
            (config.capacity, "capacity_plot", "Capacity", "mAh"),
        ];

        let active_count = active.iter().filter(|x| x.0).count();
        if active_count == 0 {
            return;
        }

        let available = ui.available_size();
        let cols = if active_count > 2 { 2 } else { 1 };
        let rows = active_count.div_ceil(cols);

        let spacing = ui.spacing().item_spacing;
        let plot_width =
            (spacing.x.mul_add(-((cols - 1) as f32), available.x) / cols as f32).max(100.0);
        let plot_height =
            (spacing.y.mul_add(-((rows - 1) as f32), available.y) / rows as f32).max(100.0);
        let size = [plot_width, plot_height];
        // Max points for min-max decimation (bigger = more detail, slower)
        let max_points = (plot_width * POINTS_PER_PIXEL) as usize;

        ui.vertical(|ui| {
            let mut current_col = 0;
            ui.horizontal_wrapped(|ui| {
                if config.voltage {
                    self.show_plot(
                        ui,
                        "voltage_plot",
                        "Voltage",
                        "V",
                        size,
                        |plot_ui| {
                            let visible_x = lod_enabled
                                .then(|| Self::visible_x_bounds(plot_ui))
                                .flatten();
                            if config.d_lines {
                                // One fused decimation pass yields the V / D+ / D− lanes.
                                let [v, dp, dn] = self.points_from_samples_multi(
                                    |s| {
                                        [
                                            f64::from(s.voltage_v),
                                            f64::from(s.dp_v),
                                            f64::from(s.dn_v),
                                        ]
                                    },
                                    max_points,
                                    visible_x,
                                    lod_enabled,
                                );
                                plot_ui.line(
                                    Line::new("V", PlotPoints::new(v))
                                        .color(egui::Color32::from_rgb(100, 180, 255))
                                        .width(1.5)
                                        .name("V"),
                                );
                                plot_ui.line(
                                    Line::new("D+", PlotPoints::new(dp))
                                        .color(egui::Color32::from_rgb(100, 255, 100))
                                        .width(1.5)
                                        .name("D+"),
                                );
                                plot_ui.line(
                                    Line::new("D-", PlotPoints::new(dn))
                                        .color(egui::Color32::from_rgb(100, 255, 255))
                                        .width(1.5)
                                        .name("D−"),
                                );
                            } else {
                                let [v] = self.points_from_samples_multi(
                                    |s| [f64::from(s.voltage_v)],
                                    max_points,
                                    visible_x,
                                    lod_enabled,
                                );
                                plot_ui.line(
                                    Line::new("V", PlotPoints::new(v))
                                        .color(egui::Color32::from_rgb(100, 180, 255))
                                        .width(1.5)
                                        .name("V"),
                                );
                            }
                        },
                        self.latest_voltage,
                        egui::Color32::from_rgb(100, 180, 255),
                        |x| self.value_from_samples(|s| f64::from(s.voltage_v), x),
                    );
                    current_col += 1;
                    if current_col % cols == 0 && current_col < active_count {
                        ui.end_row();
                    }
                }

                if config.current {
                    self.show_plot(
                        ui,
                        "current_plot",
                        "Current",
                        "A",
                        size,
                        |plot_ui| {
                            let visible_x = lod_enabled
                                .then(|| Self::visible_x_bounds(plot_ui))
                                .flatten();
                            let [a] = self.points_from_samples_multi(
                                |s| [f64::from(s.current_a)],
                                max_points,
                                visible_x,
                                lod_enabled,
                            );
                            plot_ui.line(
                                Line::new("A", PlotPoints::new(a))
                                    .color(egui::Color32::from_rgb(255, 100, 100))
                                    .width(1.5)
                                    .name("A"),
                            );
                        },
                        self.latest_current,
                        egui::Color32::from_rgb(255, 100, 100),
                        |x| self.value_from_samples(|s| f64::from(s.current_a), x),
                    );
                    current_col += 1;
                    if current_col % cols == 0 && current_col < active_count {
                        ui.end_row();
                    }
                }

                if config.power {
                    self.show_plot(
                        ui,
                        "power_plot",
                        "Power",
                        "W",
                        size,
                        |plot_ui| {
                            let visible_x = lod_enabled
                                .then(|| Self::visible_x_bounds(plot_ui))
                                .flatten();
                            let [w] = self.points_from_samples_multi(
                                |s| [f64::from(s.power_w)],
                                max_points,
                                visible_x,
                                lod_enabled,
                            );
                            plot_ui.line(
                                Line::new("W", PlotPoints::new(w))
                                    .color(egui::Color32::from_rgb(255, 180, 80))
                                    .width(1.5)
                                    .name("W"),
                            );
                        },
                        self.latest_power,
                        egui::Color32::from_rgb(255, 180, 80),
                        |x| self.value_from_samples(|s| f64::from(s.power_w), x),
                    );
                    current_col += 1;
                    if current_col % cols == 0 && current_col < active_count {
                        ui.end_row();
                    }
                }

                if config.temperature {
                    self.show_plot(
                        ui,
                        "temperature_plot",
                        "Temperature",
                        "°C",
                        size,
                        |plot_ui| {
                            let visible_x = lod_enabled
                                .then(|| Self::visible_x_bounds(plot_ui))
                                .flatten();
                            let [c] = self.points_from_samples_multi(
                                |s| [f64::from(s.temp_c)],
                                max_points,
                                visible_x,
                                lod_enabled,
                            );
                            plot_ui.line(
                                Line::new("C", PlotPoints::new(c))
                                    .color(egui::Color32::from_rgb(100, 220, 100))
                                    .width(1.5)
                                    .name("°C"),
                            );
                        },
                        self.latest_temp,
                        egui::Color32::from_rgb(100, 220, 100),
                        |x| self.value_from_samples(|s| f64::from(s.temp_c), x),
                    );
                    current_col += 1;
                    if current_col % cols == 0 && current_col < active_count {
                        ui.end_row();
                    }
                }

                if config.energy {
                    self.show_plot(
                        ui,
                        "energy_plot",
                        "Energy",
                        "Wh",
                        size,
                        |plot_ui| {
                            let visible_x = lod_enabled
                                .then(|| Self::visible_x_bounds(plot_ui))
                                .flatten();
                            plot_ui.line(
                                Line::new(
                                    "Wh",
                                    self.points_from_deque(
                                        &self.energy_wh,
                                        max_points,
                                        visible_x,
                                        lod_enabled,
                                    ),
                                )
                                .color(egui::Color32::from_rgb(220, 220, 100))
                                .width(1.5)
                                .name("Wh"),
                            );
                        },
                        self.latest_energy,
                        egui::Color32::from_rgb(220, 220, 100),
                        |x| self.value_from_vec(&self.energy_wh, x),
                    );
                    current_col += 1;
                    if current_col % cols == 0 && current_col < active_count {
                        ui.end_row();
                    }
                }

                if config.capacity {
                    self.show_plot(
                        ui,
                        "capacity_plot",
                        "Capacity",
                        "mAh",
                        size,
                        |plot_ui| {
                            let visible_x = lod_enabled
                                .then(|| Self::visible_x_bounds(plot_ui))
                                .flatten();
                            plot_ui.line(
                                Line::new(
                                    "mAh",
                                    self.points_from_deque(
                                        &self.capacity_mah,
                                        max_points,
                                        visible_x,
                                        lod_enabled,
                                    ),
                                )
                                .color(egui::Color32::from_rgb(200, 100, 220))
                                .width(1.5)
                                .name("mAh"),
                            );
                        },
                        self.latest_capacity,
                        egui::Color32::from_rgb(200, 100, 220),
                        |x| self.value_from_vec(&self.capacity_mah, x),
                    );
                    current_col += 1;
                    if current_col % cols == 0 && current_col < active_count {
                        ui.end_row();
                    }
                }
            });
        });
    }

    /// Compute the active index range for min-max decimation.
    ///
    /// When `visible_x` is `Some` **and** the visible time span is less than 90 % of the total
    /// data span, the range is narrowed to the visible sub-slice via binary search (zoom-aware
    /// LOD). Otherwise the full buffer is used so `egui_plot`'s auto-bounds can re-fit correctly
    /// — restricting data while auto-bounds is active causes a feedback loop where each frame
    /// the viewport shrinks further.
    fn visible_range(&self, visible_x: Option<(f64, f64)>) -> (usize, usize) {
        let count = self.all_samples.len();
        let Some((x_min, x_max)) = visible_x else {
            return (0, count);
        };
        let Some(first) = self.all_samples.front() else {
            return (0, 0);
        };
        let Some(last) = self.all_samples.back() else {
            return (0, 0);
        };
        let data_t_min = first.timestamp_ms as f64 / 1000.0;
        let data_t_max = last.timestamp_ms as f64 / 1000.0;
        let data_range = data_t_max - data_t_min;
        let vis_range = x_max - x_min;

        // Only restrict to the visible sub-slice when clearly zoomed in (< 90 % of total
        // range). At that point egui_plot has already disabled auto-bounds due to user
        // interaction, so restricting the data won't cause a feedback loop.
        if data_range <= 0.0 || vis_range >= data_range * 0.9 {
            return (0, count);
        }

        let min_ms = (x_min * 1000.0) as u64;
        let max_ms = (x_max * 1000.0) as u64;
        // `partition_point` has exact lower/upper-bound semantics (unlike `binary_search_by_key`,
        // whose `Ok(i)` is an unspecified match among duplicate timestamps — pause/resume
        // sequences can collide despite the app.rs NaN-sentinel +1ms nudge). Where no exact
        // match exists this is bit-for-bit the same index the old `Err(i)` case produced; where
        // duplicates exist it now deterministically resolves to the first (`start`) / last
        // (`end`) matching sample instead of an arbitrary one.
        let start = self
            .all_samples
            .partition_point(|s| s.timestamp_ms < min_ms)
            .saturating_sub(1);
        let end = (self
            .all_samples
            .partition_point(|s| s.timestamp_ms <= max_ms)
            + 1)
        .min(count);
        (start, end)
    }

    /// Timestamp (seconds) of the bucket's middle sample — the x at which
    /// [`emit_bucket`] places the bucket's extremes.
    fn bucket_center_x(&self, abs_start: usize, abs_end: usize) -> f64 {
        self.all_samples[usize::midpoint(abs_start, abs_end)].timestamp_ms as f64 / 1000.0
    }

    /// Build one plot-point series per lane in a single fused pass over the sample buffer.
    ///
    /// `extract` maps each sample to `N` independent lane values (e.g. voltage / D+ / D−),
    /// so the zoom-aware min-max decimation runs once — one [`Self::visible_range`] call and
    /// one walk of the active slice, reading each [`PlotSample`] once for all lanes. Each
    /// lane folds only a `lo`/`hi` pair via `f64::min`/`f64::max`, which ignore NaN operands,
    /// so there is no per-sample NaN branch and no argmin/argmax index bookkeeping — the
    /// accumulators stay independent, letting the compiler unroll the fold. An all-NaN
    /// bucket leaves `lo == +INFINITY`, which [`emit_bucket`] turns into a gap sentinel.
    ///
    /// Both extremes are emitted at the bucket-center timestamp (`lo` then `hi`) rather
    /// than at their true sample positions. This moves each extreme by at most half a
    /// bucket, which at `POINTS_PER_PIXEL = 8` (2 points per bucket) is ~1/8 px — visually
    /// indistinguishable. The non-LOD path still emits every sample at its true timestamp.
    fn points_from_samples_multi<const N: usize>(
        &self,
        extract: impl Fn(&PlotSample) -> [f64; N],
        max_points: usize,
        visible_x: Option<(f64, f64)>,
        lod_enabled: bool,
    ) -> [Vec<[f64; 2]>; N] {
        let count = self.all_samples.len();
        if count == 0 {
            return std::array::from_fn(|_| Vec::new());
        }

        if !lod_enabled {
            let mut pts: [Vec<[f64; 2]>; N] = std::array::from_fn(|_| Vec::with_capacity(count));
            for s in &self.all_samples {
                let t = s.timestamp_ms as f64 / 1000.0;
                let vals = extract(s);
                for (dst, v) in pts.iter_mut().zip(vals) {
                    dst.push([t, v]);
                }
            }
            return pts;
        }

        let (start_idx, end_idx) = self.visible_range(visible_x);
        let slice_len = end_idx - start_idx;

        if max_points == 0 || slice_len <= max_points {
            let mut pts: [Vec<[f64; 2]>; N] =
                std::array::from_fn(|_| Vec::with_capacity(slice_len));
            for i in start_idx..end_idx {
                let s = &self.all_samples[i];
                let t = s.timestamp_ms as f64 / 1000.0;
                let vals = extract(s);
                for (dst, v) in pts.iter_mut().zip(vals) {
                    dst.push([t, v]);
                }
            }
            return pts;
        }

        // Min-max bucket decimation over the active slice, fused across all N lanes.
        let buckets = (max_points / 2).max(1);
        let mut pts: [Vec<[f64; 2]>; N] =
            std::array::from_fn(|_| Vec::with_capacity(buckets * 2));

        for b in 0..buckets {
            let (local_start, local_end) = bucket_bounds(b, slice_len, buckets);
            if local_start >= local_end {
                continue;
            }
            let abs_start = start_idx + local_start;
            let abs_end = start_idx + local_end;

            let mut lo = [f64::INFINITY; N];
            let mut hi = [f64::NEG_INFINITY; N];
            for i in abs_start..abs_end {
                let vals = extract(&self.all_samples[i]);
                for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(vals) {
                    *l = l.min(v);
                    *h = h.max(v);
                }
            }

            let x = self.bucket_center_x(abs_start, abs_end);
            for (dst, (l, h)) in pts.iter_mut().zip(lo.into_iter().zip(hi)) {
                emit_bucket(dst, x, l, h);
            }
        }

        pts
    }

    /// Build plot points from an index-synchronized `VecDeque<f32>` (e.g. energy/capacity),
    /// with zoom-aware min-max decimation.
    ///
    /// `values` is kept the same length and index order as `all_samples`, so `values[i]`
    /// pairs with `all_samples[i].timestamp_ms`. Sub-ranges are taken with `VecDeque::range`,
    /// which positions in O(1) via the ring buffer — no per-bucket prefix walk — so each bucket
    /// costs O(bucket) rather than the O(offset) a re-skipping iterator would.
    ///
    /// Decimation uses the same branchless `lo`/`hi` fold and bucket-center [`emit_bucket`]
    /// emission as [`Self::points_from_samples_multi`], so energy/capacity decimate
    /// identically to the sample-backed lines.
    fn points_from_deque(
        &self,
        values: &VecDeque<f32>,
        max_points: usize,
        visible_x: Option<(f64, f64)>,
        lod_enabled: bool,
    ) -> PlotPoints<'static> {
        let count = self.all_samples.len();
        if count == 0 {
            return PlotPoints::new(vec![]);
        }
        debug_assert_eq!(values.len(), self.all_samples.len());

        if !lod_enabled {
            let mut pts = Vec::with_capacity(count);
            for (s, &v) in self.all_samples.iter().zip(values.iter()) {
                pts.push([s.timestamp_ms as f64 / 1000.0, f64::from(v)]);
            }
            return PlotPoints::new(pts);
        }

        let (start_idx, end_idx) = self.visible_range(visible_x);
        let slice_len = end_idx - start_idx;

        if max_points == 0 || slice_len <= max_points {
            let mut pts = Vec::with_capacity(slice_len);
            let samples = self.all_samples.range(start_idx..end_idx);
            let vals = values.range(start_idx..end_idx);
            for (s, &v) in samples.zip(vals) {
                pts.push([s.timestamp_ms as f64 / 1000.0, f64::from(v)]);
            }
            return PlotPoints::new(pts);
        }

        // Min-max bucket decimation over the active slice.
        let buckets = (max_points / 2).max(1);
        let mut pts = Vec::with_capacity(buckets * 2);

        for b in 0..buckets {
            let (local_start, local_end) = bucket_bounds(b, slice_len, buckets);
            if local_start >= local_end {
                continue;
            }
            let abs_start = start_idx + local_start;
            let abs_end = start_idx + local_end;

            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &raw in values.range(abs_start..abs_end) {
                let v = f64::from(raw);
                lo = lo.min(v);
                hi = hi.max(v);
            }

            emit_bucket(&mut pts, self.bucket_center_x(abs_start, abs_end), lo, hi);
        }

        PlotPoints::new(pts)
    }

    /// Look up the sample value nearest to the given X (seconds) coordinate.
    fn value_from_samples(&self, extract: impl Fn(&PlotSample) -> f64, x: f64) -> Option<(f64, f64)> {
        if self.all_samples.is_empty() || x < 0.0 {
            return None;
        }
        let target_ms = (x * 1000.0).round() as u64;
        let last_ms = self.all_samples.back().unwrap().timestamp_ms;
        let first_ms = self.all_samples.front().unwrap().timestamp_ms;

        // Ignore hover when the cursor is far outside the active data range.
        if target_ms > last_ms + 1000 || target_ms < first_ms.saturating_sub(1000) {
            return None;
        }

        // First index with timestamp_ms >= target_ms: identical to the old `Err(e)` insertion
        // point when no exact match exists; on an exact match among duplicate timestamps this
        // deterministically picks the first occurrence instead of `binary_search_by_key`'s
        // unspecified one.
        let idx = self
            .all_samples
            .partition_point(|s| s.timestamp_ms < target_ms);

        let idx = idx.min(self.all_samples.len().saturating_sub(1));
        self.all_samples
            .get(idx)
            .map(|s| (s.timestamp_ms as f64 / 1000.0, extract(s)))
    }

    /// Same as [`value_from_samples`] but reads from a separate `VecDeque<f32>`.
    fn value_from_vec(&self, vec: &VecDeque<f32>, x: f64) -> Option<(f64, f64)> {
        if self.all_samples.is_empty() || vec.is_empty() || x < 0.0 {
            return None;
        }
        let target_ms = (x * 1000.0).round() as u64;
        let last_ms = self.all_samples.back().unwrap().timestamp_ms;
        let first_ms = self.all_samples.front().unwrap().timestamp_ms;

        if target_ms > last_ms + 1000 || target_ms < first_ms.saturating_sub(1000) {
            return None;
        }

        // See `value_from_samples`: deterministic first-occurrence instead of an unspecified
        // duplicate index.
        let idx = self
            .all_samples
            .partition_point(|s| s.timestamp_ms < target_ms);

        let idx = idx.min(vec.len().saturating_sub(1));
        if let (Some(s), Some(&v)) = (self.all_samples.get(idx), vec.get(idx)) {
            Some((s.timestamp_ms as f64 / 1000.0, f64::from(v)))
        } else {
            None
        }
    }

    /// Render a single plot panel with title overlay and hover tooltip.
    fn show_plot(
        &self,
        ui: &mut egui::Ui,
        id: &str,
        label: &str,
        unit: &str,
        size: [f32; 2],
        add_lines: impl FnOnce(&mut egui_plot::PlotUi),
        latest_val: Option<f64>,
        primary_color: egui::Color32,
        value_at: impl Fn(f64) -> Option<(f64, f64)>,
    ) {
        let all_samples_empty = self.all_samples.is_empty();

        let response = Plot::new((id, self.generation))
            .height(size[1])
            .width(size[0])
            .include_y(0.0)
            .include_y(0.1)
            .y_axis_label(unit)
            .show_axes([true, true])
            .set_margin_fraction(egui::Vec2::ZERO)
            .label_formatter({
                let value_at = &value_at;
                move |_, value| {
                    if all_samples_empty {
                        "No data".to_string()
                    } else if let Some((x, y)) = value_at(value.x) {
                        format!("{label}: {y:.3} {unit}\nTime: {x:.3} s")
                    } else {
                        String::new()
                    }
                }
            })
            .show(ui, |plot_ui| {
                add_lines(plot_ui);

                let pointer_pos = plot_ui.ctx().input(|i| i.pointer.hover_pos());
                let is_hovered = pointer_pos.is_some_and(|p| plot_ui.response().rect.contains(p));

                if is_hovered
                    && let Some(pointer) = plot_ui.pointer_coordinate()
                    && let Some(val) = value_at(pointer.x)
                {
                    plot_ui.points(
                        egui_plot::Points::new("hover", vec![val.into()])
                            .radius(4.0)
                            .color(egui::Color32::WHITE)
                            .shape(egui_plot::MarkerShape::Circle),
                    );
                }
            });

        // Overlay title with live value
        let title_rect = response.response.rect;
        let painter = ui.painter();
        let title_text = latest_val.map_or_else(
            || format!("{label}: No data"),
            |v| format!("{label}: {v:.3} {unit}"),
        );
        painter.text(
            egui::pos2(title_rect.right() - 5.0, title_rect.top() + 2.0),
            egui::Align2::RIGHT_TOP,
            title_text,
            egui::FontId::proportional(13.0),
            primary_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(timestamp_ms: u64) -> Sample {
        Sample {
            timestamp_ms,
            voltage_v: 5.0,
            current_a: 1.0,
            power_w: 5.0,
            dp_v: 0.0,
            dn_v: 0.0,
            temp_c: 0.0,
            raw_voltage: 0,
            raw_current: 0,
        }
    }

    #[test]
    fn reserve_then_bulk_import_preserves_lengths_and_logical_capacity() {
        let mut plots = PlotState::new(16);
        let n = 1_000;
        plots.reserve(n);
        for i in 0..n as u64 {
            plots.push_unlimited(&sample(i), 0.0, 0.0);
        }
        assert_eq!(plots.sample_count(), n);
        assert_eq!(plots.all_samples.len(), n);
        // `reserve` must extend the raw-ADC side-buffer too, keeping it in lockstep.
        assert_eq!(plots.raw_adc.len(), n);
        // `reserve` only pre-allocates storage; the logical sample_capacity
        // (the circular-buffer limit used by `push`) must be unchanged.
        assert_eq!(plots.capacity(), 16);
    }
}

#[cfg(test)]
// All float comparisons below are against exact values by construction (timestamps built from
// whole milliseconds, voltages chosen to be exactly representable in f32), so strict equality
// is the correct check, not an approximation smell.
#[allow(clippy::float_cmp)]
mod partition_point_tests {
    use super::*;

    fn sample(timestamp_ms: u64, voltage_v: f32) -> Sample {
        Sample {
            timestamp_ms,
            voltage_v,
            current_a: 0.0,
            power_w: 0.0,
            dp_v: 0.0,
            dn_v: 0.0,
            temp_c: 0.0,
            raw_voltage: 0,
            raw_current: 0,
        }
    }

    /// Fixture with duplicate timestamps at 3000ms (indices 1-3) and 8000ms (indices 8-9),
    /// as can occur when pause/resume sequences collide despite the app.rs NaN-sentinel
    /// +1ms nudge. Per-sample voltages are distinct so tests can tell which duplicate index
    /// was actually selected.
    fn duplicate_fixture() -> PlotState {
        let mut state = PlotState::new(20);
        let voltages = [
            (2_000, 2.0),
            (3_000, 3.1),
            (3_000, 3.2),
            (3_000, 3.3),
            (4_000, 4.0),
            (5_000, 5.0),
            (6_000, 6.0),
            (7_000, 7.0),
            (8_000, 8.1),
            (8_000, 8.2),
            (10_000, 10.0),
        ];
        for (ts, v) in voltages {
            state.push_unlimited(&sample(ts, v), 0.0, 0.0);
        }
        state
    }

    #[test]
    fn visible_range_full_when_not_zoomed() {
        let state = duplicate_fixture();
        assert_eq!(state.visible_range(None), (0, 11));
    }

    #[test]
    fn visible_range_min_on_duplicate_timestamp_is_first_occurrence_minus_one() {
        // min_ms = 3000 hits the three-way duplicate at indices 1-3; documented deterministic
        // behavior is first-geq (index 1), so start = 0. max_ms = 3500 has no exact match, so
        // it exercises the same insertion-point path the old `Err(i)` branch did.
        let state = duplicate_fixture();
        assert_eq!(state.visible_range(Some((3.0, 3.5))), (0, 5));
    }

    #[test]
    fn visible_range_max_on_duplicate_timestamp_is_last_occurrence_plus_one() {
        // max_ms = 8000 hits the duplicate at indices 8-9; documented deterministic behavior is
        // last-leq, so end lands one past index 9 (plus the existing lead-out `+ 1`). min_ms =
        // 7500 has no exact match (regression check against the old `Err(i)` path).
        let state = duplicate_fixture();
        assert_eq!(state.visible_range(Some((7.5, 8.0))), (7, 11));
    }

    #[test]
    fn value_from_samples_exact_hit_on_duplicate_picks_first_occurrence() {
        // Old `binary_search_by_key` would return an unspecified index among the three 3000ms
        // duplicates; `partition_point` deterministically picks the first one (index 1, v=3.1).
        let state = duplicate_fixture();
        let (t, v) = state.value_from_samples(|s| f64::from(s.voltage_v), 3.0).unwrap();
        assert_eq!(t, 3.0);
        // Compare via the same f32->f64 widening the code under test uses: 3.1_f32 is not
        // exactly representable, so a bare `3.1` f64 literal would never match.
        assert_eq!(v, f64::from(3.1_f32));
    }

    #[test]
    fn value_from_samples_between_samples_matches_old_insertion_point() {
        // No exact match at 3500ms: both old (`Err(i)`) and new (`partition_point`) resolve to
        // the next sample at 4000ms.
        let state = duplicate_fixture();
        let (t, v) = state.value_from_samples(|s| f64::from(s.voltage_v), 3.5).unwrap();
        assert_eq!(t, 4.0);
        assert_eq!(v, 4.0);
    }

    #[test]
    fn value_from_samples_before_first_clamps_to_first() {
        // 1500ms is before the first sample (2000ms) but within the 1000ms hover tolerance;
        // clamps to index 0, identical to the old code's `Err(0)` case.
        let state = duplicate_fixture();
        let (t, v) = state.value_from_samples(|s| f64::from(s.voltage_v), 1.5).unwrap();
        assert_eq!(t, 2.0);
        assert_eq!(v, 2.0);
    }

    #[test]
    fn value_from_samples_after_last_clamps_to_last() {
        // 10500ms is after the last sample (10000ms) but within the 1000ms hover tolerance;
        // clamps to the last index, identical to the old code's `Err(len)` case.
        let state = duplicate_fixture();
        let (t, v) = state.value_from_samples(|s| f64::from(s.voltage_v), 10.5).unwrap();
        assert_eq!(t, 10.0);
        assert_eq!(v, 10.0);
    }

    #[test]
    fn value_from_samples_exact_hit_on_unique_timestamp_is_unchanged() {
        // No duplicates at the last timestamp, so this is deterministic both old and new.
        let state = duplicate_fixture();
        let (t, v) = state.value_from_samples(|s| f64::from(s.voltage_v), 10.0).unwrap();
        assert_eq!(t, 10.0);
        assert_eq!(v, 10.0);
    }

    #[test]
    fn value_from_vec_exact_hit_on_duplicate_picks_first_occurrence() {
        let state = duplicate_fixture();
        let vec: VecDeque<f32> = (0..11).map(|i| 100.0 + i as f32).collect();
        let (t, v) = state.value_from_vec(&vec, 3.0).unwrap();
        assert_eq!(t, 3.0);
        assert_eq!(v, 101.0);
    }

    #[test]
    fn value_from_vec_between_samples_matches_old_insertion_point() {
        let state = duplicate_fixture();
        let vec: VecDeque<f32> = (0..11).map(|i| 100.0 + i as f32).collect();
        let (t, v) = state.value_from_vec(&vec, 3.5).unwrap();
        assert_eq!(t, 4.0);
        assert_eq!(v, 104.0);
    }
}

#[cfg(test)]
mod bucket_bounds_tests {
    use super::bucket_bounds;

    /// Cases spanning slice_len < buckets, exact division, and non-exact division.
    fn cases() -> Vec<(usize, usize)> {
        vec![
            (0, 1),
            (1, 1),
            (10, 4),   // exact: slice_len % buckets == 0
            (10, 3),   // inexact: remainder distributed across leading buckets
            (3, 10),   // slice_len < buckets: most buckets are empty
            (1, 10),
            (1000, 7),
            (2_000_003, 4096),
        ]
    }

    #[test]
    fn covers_exactly_zero_to_slice_len_with_no_overlap() {
        for (slice_len, buckets) in cases() {
            let mut expected_next_start = 0;
            for b in 0..buckets {
                let (start, end) = bucket_bounds(b, slice_len, buckets);
                assert_eq!(
                    start, expected_next_start,
                    "bucket {b} start should immediately follow previous bucket's end \
                     (slice_len={slice_len}, buckets={buckets})"
                );
                assert!(
                    end <= slice_len,
                    "bucket {b} end {end} exceeds slice_len {slice_len} (buckets={buckets})"
                );
                expected_next_start = end;
            }
            assert_eq!(
                expected_next_start, slice_len,
                "last bucket should end exactly at slice_len (slice_len={slice_len}, buckets={buckets})"
            );
        }
    }

    #[test]
    fn bounds_are_monotonically_nondecreasing() {
        for (slice_len, buckets) in cases() {
            let mut prev_end = 0;
            for b in 0..buckets {
                let (start, end) = bucket_bounds(b, slice_len, buckets);
                assert!(start <= end, "start {start} > end {end} for bucket {b}");
                assert!(
                    start >= prev_end,
                    "bucket {b} start {start} regressed before previous end {prev_end}"
                );
                prev_end = end;
            }
        }
    }

    #[test]
    fn empty_buckets_are_skippable_when_buckets_exceed_slice_len() {
        // slice_len < buckets: some buckets must be empty (start == end), and the
        // `if local_start >= local_end { continue; }` guard at the call sites relies on this.
        let (slice_len, buckets) = (3, 10);
        let empty_count = (0..buckets)
            .filter(|&b| {
                let (start, end) = bucket_bounds(b, slice_len, buckets);
                start == end
            })
            .count();
        assert_eq!(empty_count, buckets - slice_len);
    }

    #[test]
    fn matches_previous_float_based_formula_when_inexact() {
        // Confirms behavior is unchanged from the old `(b as f64 * bucket_size) as usize`
        // formula for slice_len % buckets != 0, where float rounding could plausibly differ.
        let (slice_len, buckets) = (17, 5);
        let bucket_size = slice_len as f64 / buckets as f64;
        for b in 0..buckets {
            let old_start = (b as f64 * bucket_size) as usize;
            let old_end = (((b + 1) as f64 * bucket_size) as usize).min(slice_len);
            let (new_start, new_end) = bucket_bounds(b, slice_len, buckets);
            assert_eq!(old_start, new_start, "start mismatch at bucket {b}");
            assert_eq!(old_end, new_end, "end mismatch at bucket {b}");
        }
    }
}

#[cfg(test)]
mod points_from_deque_tests {
    use super::*;

    const fn mk_sample(timestamp_ms: u64) -> Sample {
        Sample {
            timestamp_ms,
            voltage_v: 0.0,
            current_a: 0.0,
            power_w: 0.0,
            dp_v: 0.0,
            dn_v: 0.0,
            temp_c: 0.0,
            raw_voltage: 0,
            raw_current: 0,
        }
    }

    fn push_energy(ps: &mut PlotState, timestamp_ms: u64, energy_wh: f64) {
        // capacity_mah mirrors a distinct-but-synchronized series.
        ps.push(&mk_sample(timestamp_ms), energy_wh, energy_wh * 2.0);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn empty_state_returns_no_points() {
        let ps = PlotState::new(16);
        // Early return fires before the length debug_assert, in both LOD modes.
        assert!(
            ps.points_from_deque(&ps.energy_wh, 1000, None, true)
                .points()
                .is_empty()
        );
        assert!(
            ps.points_from_deque(&ps.capacity_mah, 0, None, false)
                .points()
                .is_empty()
        );
    }

    #[test]
    fn no_lod_returns_every_sample_in_order() {
        let mut ps = PlotState::new(32);
        for i in 0..8u64 {
            push_energy(&mut ps, i * 1000, i as f64);
        }
        let pts = ps.points_from_deque(&ps.energy_wh, 1000, None, false);
        let points = pts.points();
        assert_eq!(points.len(), 8);
        for (i, p) in points.iter().enumerate() {
            assert_close(p.x, i as f64); // timestamp_ms / 1000
            assert_close(p.y, i as f64); // energy value at the same index
        }
    }

    #[test]
    fn lod_decimates_to_min_max_per_bucket() {
        let mut ps = PlotState::new(2048);
        let n = 1000usize;
        for i in 0..n {
            push_energy(&mut ps, i as u64 * 10, i as f64);
        }
        // slice_len = 1000 > max_points = 10 -> buckets = 5 of 200 samples; every
        // bucket emits its lo/hi pair at the bucket-center timestamp.
        let pts = ps.points_from_deque(&ps.energy_wh, 10, None, true);
        let points = pts.points();
        assert_eq!(points.len(), 10);
        // Bucket 0 covers indices 0..200 (min 0, max 199); both extremes land at the
        // center sample (index 100, t = 1.0 s), lo first.
        assert_close(points[0].x, 1.0);
        assert_close(points[0].y, 0.0);
        assert_close(points[1].x, 1.0);
        assert_close(points[1].y, 199.0);
        assert!(points.len() < n);
    }

    #[test]
    fn all_nan_bucket_emits_gap_sentinel() {
        let mut ps = PlotState::new(2048);
        // First 200 samples carry real increasing energy; the next 200 are NaN.
        for i in 0..200u64 {
            push_energy(&mut ps, i * 10, i as f64);
        }
        for i in 200..400u64 {
            push_energy(&mut ps, i * 10, f64::NAN);
        }
        // slice_len = 400 > max_points = 4 -> buckets = 2, bucket_size = 200.
        // Bucket 0 (real) emits 2 points; bucket 1 (all NaN) emits 1 gap.
        let pts = ps.points_from_deque(&ps.energy_wh, 4, None, true);
        let points = pts.points();
        assert_eq!(points.len(), 3);
        assert!(points[0].y.is_finite());
        assert!(points[1].y.is_finite());
        assert!(points[2].y.is_nan(), "all-NaN bucket must emit a NaN gap");
        // The sentinel sits at the bucket-center sample (index 300 of 200..400).
        assert_close(points[2].x, 3.0);
    }

    #[test]
    fn public_api_keeps_series_length_synced() {
        // Every public mutator keeps energy_wh / capacity_mah index-synchronized with
        // all_samples, so the debug_assert inside points_from_deque can never trip.
        let mut ps = PlotState::new(5);
        for i in 0..20u64 {
            push_energy(&mut ps, i * 10, i as f64); // overflows capacity -> pop_front
        }
        assert_eq!(ps.energy_wh.len(), ps.all_samples.len());
        assert_eq!(ps.capacity_mah.len(), ps.all_samples.len());

        ps.set_capacity(3);
        assert_eq!(ps.energy_wh.len(), ps.all_samples.len());
        assert_eq!(ps.capacity_mah.len(), ps.all_samples.len());

        ps.clear();
        assert_eq!(ps.energy_wh.len(), ps.all_samples.len());
        assert_eq!(ps.capacity_mah.len(), ps.all_samples.len());

        for i in 0..7u64 {
            ps.push_unlimited(&mk_sample(i * 10), i as f64, i as f64);
        }
        assert_eq!(ps.energy_wh.len(), ps.all_samples.len());
        assert_eq!(ps.capacity_mah.len(), ps.all_samples.len());

        // Exercising the path proves the assert holds for both public series.
        assert_eq!(
            ps.points_from_deque(&ps.energy_wh, 8, None, true)
                .points()
                .len(),
            7
        );
        assert_eq!(
            ps.points_from_deque(&ps.capacity_mah, 8, None, true)
                .points()
                .len(),
            7
        );
    }

    #[test]
    #[ignore = "manual benchmark; run with --release -- --ignored --nocapture"]
    fn bench_points_from_deque() {
        let n = 200_000usize;
        let mut ps = PlotState::new(n);
        for i in 0..n {
            push_energy(&mut ps, i as u64 * 10, i as f64 * 0.001);
        }
        let iters = 200usize;

        // New path: read the VecDeque in place, no per-frame source allocation.
        let start = std::time::Instant::now();
        let mut sink = 0usize;
        for _ in 0..iters {
            let pts = ps.points_from_deque(&ps.energy_wh, 4000, None, true);
            sink += pts.points().len();
        }
        let new_ms = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        // The work the old `points_from_iter` did every frame before any decimation:
        // widen the whole f32 buffer into a fresh `Vec<f64>`. This is exactly what the
        // new path removes; time it in isolation as the eliminated per-frame cost.
        let start = std::time::Instant::now();
        let mut alloc_sink = 0usize;
        for _ in 0..iters {
            // black_box keeps the allocation from being optimized away (this is the cost
            // being measured) and prevents clippy folding it into a non-allocating count.
            let widened = std::hint::black_box(
                ps.energy_wh.iter().map(|&v| f64::from(v)).collect::<Vec<f64>>(),
            );
            alloc_sink += widened.len();
        }
        let widen_ms = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        println!(
            "points_from_deque: {n} samples x {iters} iters -> {new_ms:.4} ms/frame (sink={sink}); \
             eliminated per-frame Vec<f64> widen: {widen_ms:.4} ms/frame (sink={alloc_sink})"
        );
        assert!(sink > 0 && alloc_sink > 0);
    }
}

#[cfg(test)]
mod raw_adc_split_tests {
    use super::*;

    /// Build a sample whose every field is distinct so round-trips can catch
    /// any field being dropped or crossed with another.
    fn sample(ts: u64, base: f32, raw_voltage: u32, raw_current: u32) -> Sample {
        Sample {
            timestamp_ms: ts,
            voltage_v: base,
            current_a: base + 1.0,
            power_w: base + 2.0,
            dp_v: base + 3.0,
            dn_v: base + 4.0,
            temp_c: base + 5.0,
            raw_voltage,
            raw_current,
        }
    }

    #[test]
    fn plot_sample_is_32_bytes() {
        assert_eq!(std::mem::size_of::<PlotSample>(), 32);
    }

    #[test]
    fn export_round_trips_every_field_including_raw_adc() {
        let mut state = PlotState::new(16);
        let inputs = [
            sample(0, 1.0, 510_000, 120_000),
            sample(100, 2.5, 900_000, 200_000),
            sample(200, 3.25, 1_234_567, 7_654_321),
        ];
        for s in &inputs {
            state.push(s, 0.0, 0.0);
        }

        let exported = state.export_samples();
        assert_eq!(exported.len(), inputs.len());
        for (out, inp) in exported.iter().zip(&inputs) {
            assert_eq!(out.timestamp_ms, inp.timestamp_ms);
            // Compare bit patterns to keep clippy's float_cmp happy and stay exact.
            assert_eq!(out.voltage_v.to_bits(), inp.voltage_v.to_bits());
            assert_eq!(out.current_a.to_bits(), inp.current_a.to_bits());
            assert_eq!(out.power_w.to_bits(), inp.power_w.to_bits());
            assert_eq!(out.dp_v.to_bits(), inp.dp_v.to_bits());
            assert_eq!(out.dn_v.to_bits(), inp.dn_v.to_bits());
            assert_eq!(out.temp_c.to_bits(), inp.temp_c.to_bits());
            assert_eq!(out.raw_voltage, inp.raw_voltage);
            assert_eq!(out.raw_current, inp.raw_current);
        }
    }

    #[test]
    fn capacity_eviction_keeps_buffers_in_lockstep() {
        let mut state = PlotState::new(3);
        for i in 0..10u64 {
            state.push(&sample(i * 10, i as f32, i as u32, (i * 2) as u32), 0.0, 0.0);
        }
        assert_eq!(state.sample_count(), 3);
        assert_eq!(state.all_samples.len(), state.raw_adc.len());
        assert_eq!(state.all_samples.len(), state.energy_wh.len());
        assert_eq!(state.all_samples.len(), state.capacity_mah.len());

        // Only the three newest survive, and their raw ADC stays aligned.
        let exported = state.export_samples();
        assert_eq!(exported[0].timestamp_ms, 70);
        assert_eq!(exported[0].raw_voltage, 7);
        assert_eq!(exported[0].raw_current, 14);
        assert_eq!(exported[2].raw_voltage, 9);
        assert_eq!(exported[2].raw_current, 18);
    }

    #[test]
    fn set_capacity_drains_all_buffers_in_lockstep() {
        let mut state = PlotState::new(100);
        for i in 0..10u64 {
            state.push_unlimited(&sample(i * 10, i as f32, i as u32, (i * 2) as u32), 0.0, 0.0);
        }
        state.set_capacity(4);
        assert_eq!(state.sample_count(), 4);
        assert_eq!(state.all_samples.len(), state.raw_adc.len());
        assert_eq!(state.raw_adc.len(), state.energy_wh.len());
        assert_eq!(state.energy_wh.len(), state.capacity_mah.len());

        let exported = state.export_samples();
        assert_eq!(exported.first().unwrap().timestamp_ms, 60);
        assert_eq!(exported.first().unwrap().raw_voltage, 6);
        assert_eq!(exported.last().unwrap().raw_voltage, 9);
    }

    #[test]
    fn clear_empties_all_buffers() {
        let mut state = PlotState::new(8);
        state.push(&sample(0, 1.0, 5, 6), 1.0, 2.0);
        state.clear();
        assert!(state.is_empty());
        assert!(state.raw_adc.is_empty());
        assert!(state.export_samples().is_empty());
    }
}

#[cfg(test)]
mod fused_decimation_tests {
    use super::*;

    fn mk(ts_ms: u64, v: f32, dp: f32, dn: f32) -> Sample {
        Sample {
            timestamp_ms: ts_ms,
            voltage_v: v,
            current_a: 0.0,
            power_w: 0.0,
            dp_v: dp,
            dn_v: dn,
            temp_c: 0.0,
            raw_voltage: 0,
            raw_current: 0,
        }
    }

    fn state_from(samples: &[Sample]) -> PlotState {
        let mut st = PlotState::new(samples.len().max(1));
        for s in samples {
            st.push_unlimited(s, 0.0, 0.0);
        }
        st
    }

    /// Timestamp (seconds) exactly as the decimator computes it from milliseconds.
    fn t(ms: u64) -> f64 {
        ms as f64 / 1000.0
    }

    /// Bitwise coordinate equality that also treats NaN as equal to NaN.
    fn coord_eq(x: f64, y: f64) -> bool {
        if x.is_nan() || y.is_nan() {
            x.is_nan() && y.is_nan()
        } else {
            x.to_bits() == y.to_bits()
        }
    }

    fn points_eq(a: &[[f64; 2]], b: &[[f64; 2]]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(p, q)| coord_eq(p[0], q[0]) && coord_eq(p[1], q[1]))
    }

    #[test]
    fn n1_ramp_emits_extremes_at_bucket_centers() {
        // 10-sample ramp, max_points = 4 → 2 buckets of 5 samples each.
        let samples: Vec<Sample> = (0..10).map(|i| mk(i, i as f32, 0.0, 0.0)).collect();
        let st = state_from(&samples);

        let [lane] = st.points_from_samples_multi(|s| [f64::from(s.voltage_v)], 4, None, true);

        // Bucket 0 (idx 0..5): lo 0 / hi 4 at center idx 2. Bucket 1 (idx 5..10):
        // lo 5 / hi 9 at center idx 7.
        let expected = vec![[t(2), 0.0], [t(2), 4.0], [t(7), 5.0], [t(7), 9.0]];
        assert!(points_eq(&lane, &expected), "got {lane:?}");
    }

    #[test]
    fn n3_lanes_fold_independently_at_shared_bucket_center() {
        // Single bucket (max_points = 2) over 5 samples. Each lane's extremes come from
        // different sample indices, yet all land at the shared bucket-center x (idx 2),
        // lo before hi regardless of which sample produced which extreme.
        let v = [0.0, 1.0, 2.0, 3.0, 4.0]; // ascending: min@0, max@4
        let dp = [7.0, 7.0, 7.0, 7.0, 7.0]; // flat: lo == hi
        let dn = [4.0, 3.0, 2.0, 1.0, 0.0]; // descending: min@4, max@0
        let samples: Vec<Sample> = (0..5)
            .map(|i| mk(i, v[i as usize], dp[i as usize], dn[i as usize]))
            .collect();
        let st = state_from(&samples);

        let [lv, ldp, ldn] = st.points_from_samples_multi(
            |s| [f64::from(s.voltage_v), f64::from(s.dp_v), f64::from(s.dn_v)],
            2,
            None,
            true,
        );

        assert!(points_eq(&lv, &[[t(2), 0.0], [t(2), 4.0]]), "v: {lv:?}");
        assert!(points_eq(&ldp, &[[t(2), 7.0], [t(2), 7.0]]), "dp: {ldp:?}");
        assert!(points_eq(&ldn, &[[t(2), 0.0], [t(2), 4.0]]), "dn: {ldn:?}");
    }

    #[test]
    fn nan_operands_ignored_and_all_nan_bucket_emits_gap_sentinel() {
        // 6 samples, max_points = 4 → 2 buckets of 3. Bucket 0 has a leading NaN that the
        // min/max fold must ignore (without an explicit is_nan branch); bucket 1 is all
        // NaN → one gap sentinel at its bucket-center timestamp.
        let vals = [f32::NAN, 1.0, 2.0, f32::NAN, f32::NAN, f32::NAN];
        let samples: Vec<Sample> = (0..6).map(|i| mk(i, vals[i as usize], 0.0, 0.0)).collect();
        let st = state_from(&samples);

        let [lane] = st.points_from_samples_multi(|s| [f64::from(s.voltage_v)], 4, None, true);

        assert_eq!(lane.len(), 3);
        // Bucket 0 (idx 0..3, center idx 1): NaN ignored → lo 1 / hi 2.
        assert!(points_eq(&lane[..2], &[[t(1), 1.0], [t(1), 2.0]]), "{lane:?}");
        // Bucket 1 (idx 3..6, center idx 4): all NaN → sentinel with NaN y.
        assert!(coord_eq(lane[2][0], t(4)), "{lane:?}");
        assert!(lane[2][1].is_nan(), "{lane:?}");
    }

    #[test]
    fn fused_matches_separate() {
        // The fused N=3 pass must produce, lane for lane, exactly what three N=1 passes
        // produce, across the non-LOD path, the pass-through path, and several
        // decimation ratios.
        let n = 1500usize;
        let samples: Vec<Sample> = (0..n)
            .map(|i| {
                let v = if i % 71 == 0 { f32::NAN } else { (i % 97) as f32 };
                let dp = if i % 50 == 0 { f32::NAN } else { (i % 53) as f32 };
                let dn = (i % 31) as f32;
                mk(i as u64, v, dp, dn)
            })
            .collect();
        let st = state_from(&samples);

        for &mp in &[0usize, 8, 64, 500, 4000] {
            for &lod in &[true, false] {
                let [fv, fdp, fdn] = st.points_from_samples_multi(
                    |s| [f64::from(s.voltage_v), f64::from(s.dp_v), f64::from(s.dn_v)],
                    mp,
                    None,
                    lod,
                );
                let [sv] =
                    st.points_from_samples_multi(|s| [f64::from(s.voltage_v)], mp, None, lod);
                let [sdp] = st.points_from_samples_multi(|s| [f64::from(s.dp_v)], mp, None, lod);
                let [sdn] = st.points_from_samples_multi(|s| [f64::from(s.dn_v)], mp, None, lod);
                assert!(points_eq(&fv, &sv), "voltage mismatch mp={mp} lod={lod}");
                assert!(points_eq(&fdp, &sdp), "dp mismatch mp={mp} lod={lod}");
                assert!(points_eq(&fdn, &sdn), "dn mismatch mp={mp} lod={lod}");
            }
        }
    }

    #[test]
    #[ignore = "timing benchmark; run with: cargo test --release -- --ignored --nocapture"]
    fn timing_fused_vs_separate() {
        let n = 200_000usize;
        let samples: Vec<Sample> = (0..n)
            .map(|i| mk(i as u64, (i % 97) as f32, (i % 53) as f32, (i % 31) as f32))
            .collect();
        let st = state_from(&samples);
        let max_points = 4000usize;
        let iters = 50u32;

        let sep_start = std::time::Instant::now();
        for _ in 0..iters {
            let a =
                st.points_from_samples_multi(|s| [f64::from(s.voltage_v)], max_points, None, true);
            let b = st.points_from_samples_multi(|s| [f64::from(s.dp_v)], max_points, None, true);
            let c = st.points_from_samples_multi(|s| [f64::from(s.dn_v)], max_points, None, true);
            std::hint::black_box((a, b, c));
        }
        let sep = sep_start.elapsed();

        let fused_start = std::time::Instant::now();
        for _ in 0..iters {
            let lanes = st.points_from_samples_multi(
                |s| [f64::from(s.voltage_v), f64::from(s.dp_v), f64::from(s.dn_v)],
                max_points,
                None,
                true,
            );
            std::hint::black_box(lanes);
        }
        let fused = fused_start.elapsed();

        eprintln!(
            "3x N=1: {sep:?} ({:?}/iter)  vs  1x N=3: {fused:?} ({:?}/iter)  [{n} samples, {iters} iters]",
            sep / iters,
            fused / iters,
        );
    }
}

#[cfg(test)]
mod decimation_parity_tests {
    use super::*;

    fn build_state(values: &[f32]) -> PlotState {
        let mut st = PlotState::new(values.len().max(1));
        for (i, &v) in values.iter().enumerate() {
            let s = Sample {
                timestamp_ms: i as u64 * 10,
                voltage_v: v,
                current_a: 0.0,
                power_w: 0.0,
                dp_v: 0.0,
                dn_v: 0.0,
                temp_c: 0.0,
                raw_voltage: 0,
                raw_current: 0,
            };
            st.push_unlimited(&s, 0.0, 0.0);
        }
        st
    }

    /// One bucket's outcome from the pre-change loop: extreme values plus the true
    /// timestamps the old code emitted them at.
    struct OldBucket {
        abs_start: usize,
        abs_end: usize,
        min_val: f64, // +INFINITY when the bucket is all-NaN
        max_val: f64,
        min_x: f64,
        max_x: f64,
    }

    /// ORIGINAL per-lane bucket loop (argmin/argmax, extremes at true timestamps),
    /// preserved so the parity and timing tests have a reference implementation.
    fn old_decimate(
        st: &PlotState,
        extract: impl Fn(&PlotSample) -> f64,
        start_idx: usize,
        end_idx: usize,
        buckets: usize,
    ) -> Vec<OldBucket> {
        let slice_len = end_idx - start_idx;
        let mut out = Vec::with_capacity(buckets);
        for b in 0..buckets {
            let (local_start, local_end) = bucket_bounds(b, slice_len, buckets);
            if local_start >= local_end {
                continue;
            }
            let abs_start = start_idx + local_start;
            let abs_end = start_idx + local_end;

            let mut min_val = f64::INFINITY;
            let mut max_val = f64::NEG_INFINITY;
            let mut min_idx = abs_start;
            let mut max_idx = abs_start;
            for i in abs_start..abs_end {
                let v = extract(&st.all_samples[i]);
                if v.is_nan() {
                    continue;
                }
                if v < min_val {
                    min_val = v;
                    min_idx = i;
                }
                if v > max_val {
                    max_val = v;
                    max_idx = i;
                }
            }

            let ts = |i: usize| st.all_samples[i].timestamp_ms as f64 / 1000.0;
            out.push(OldBucket {
                abs_start,
                abs_end,
                min_val,
                max_val,
                min_x: ts(min_idx),
                max_x: ts(max_idx),
            });
        }
        out
    }

    /// Seeded LCG (no external rand crate) producing values in `[0, 1)`.
    struct Lcg(u64);
    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            // Numerical Recipes constants.
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
        fn next_unit(&mut self) -> f64 {
            // Top 53 bits → uniform f64 in [0, 1).
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn noisy_values(n: usize, seed: u64, nan_every: usize) -> Vec<f32> {
        let mut rng = Lcg(seed);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // Slow drift + high-frequency noise, occasional NaN runs. All finite values
            // stay positive, so the min/max fold never sees a ±0.0 ambiguity.
            if nan_every != 0 && (i / nan_every).is_multiple_of(17) {
                out.push(f32::NAN);
                continue;
            }
            let drift = (i as f64 / 500.0).sin().mul_add(2.0, 5.0);
            let noise = (rng.next_unit() - 0.5) * 0.8;
            out.push((drift + noise) as f32);
        }
        out
    }

    #[test]
    fn bucket_center_fold_matches_argminmax_extremes() {
        // Include constant runs and NaN runs alongside the noisy data.
        let mut values = noisy_values(4000, 0x1234_5678, 250);
        for v in &mut values[500..600] {
            *v = 3.3; // constant bucket region: old code emitted a single point here
        }
        for v in &mut values[700..720] {
            *v = f32::NAN; // all-NaN bucket region
        }
        let st = build_state(&values);
        let extract = |s: &PlotSample| f64::from(s.voltage_v);
        let ts = |idx: usize| st.all_samples[idx].timestamp_ms as f64 / 1000.0;

        // Full view plus zoomed windows; visible_range narrows exactly as production does.
        for &visible_x in &[None, Some((5.0, 12.0)), Some((0.9, 7.31))] {
            let (start_idx, end_idx) = st.visible_range(visible_x);
            for &max_points in &[64usize, 512, 3998] {
                if end_idx - start_idx <= max_points {
                    continue; // pass-through: no decimation to compare
                }
                let buckets = (max_points / 2).max(1);
                let old = old_decimate(&st, extract, start_idx, end_idx, buckets);
                let [new] = st.points_from_samples_multi(
                    |s| [f64::from(s.voltage_v)],
                    max_points,
                    visible_x,
                    true,
                );

                let mut i = 0usize;
                for ob in &old {
                    // Half the bucket's width in seconds (10 ms sample period), plus a
                    // hair of float slack for the exactly-half-a-bucket edge case.
                    let tol = (ob.abs_end - ob.abs_start) as f64 * 0.010 / 2.0 + 1e-9;
                    if ob.min_val.is_infinite() {
                        // All-NaN bucket → a single gap sentinel in both implementations.
                        let [x, y] = new[i];
                        assert!(y.is_nan(), "expected NaN sentinel, got {y}");
                        assert!(
                            (x - ts(ob.abs_start)).abs() <= tol,
                            "sentinel moved more than half a bucket"
                        );
                        i += 1;
                    } else {
                        let [x_lo, y_lo] = new[i];
                        let [x_hi, y_hi] = new[i + 1];
                        // Per-bucket {min, max} value multiset is identical, bit for bit:
                        // both folds reduce the same finite f64 set and the fixture has
                        // no ±0.0 ambiguity.
                        assert_eq!(y_lo.to_bits(), ob.min_val.to_bits(), "min differs");
                        assert_eq!(y_hi.to_bits(), ob.max_val.to_bits(), "max differs");
                        // Both extremes share the bucket-center x, and each sits within
                        // half a bucket of where the old code emitted it.
                        assert_eq!(x_lo.to_bits(), x_hi.to_bits());
                        assert!(
                            (x_lo - ob.min_x).abs() <= tol && (x_hi - ob.max_x).abs() <= tol,
                            "extreme moved more than half a bucket"
                        );
                        i += 2;
                    }
                }
                assert_eq!(i, new.len(), "point count mismatch");
            }
        }
    }

    /// Flat emitter mirroring the OLD production loop end to end (argmin/argmax at true
    /// timestamps), used only by the ignored timing comparison below.
    fn old_flat(
        st: &PlotState,
        extract: impl Fn(&PlotSample) -> f64,
        slice_len: usize,
        buckets: usize,
    ) -> Vec<[f64; 2]> {
        let mut pts = Vec::with_capacity(buckets * 2);
        for b in 0..buckets {
            let (abs_start, abs_end) = bucket_bounds(b, slice_len, buckets);
            if abs_start >= abs_end {
                continue;
            }
            let mut min_val = f64::INFINITY;
            let mut max_val = f64::NEG_INFINITY;
            let mut min_idx = abs_start;
            let mut max_idx = abs_start;
            for i in abs_start..abs_end {
                let v = extract(&st.all_samples[i]);
                if v.is_nan() {
                    continue;
                }
                if v < min_val {
                    min_val = v;
                    min_idx = i;
                }
                if v > max_val {
                    max_val = v;
                    max_idx = i;
                }
            }
            if min_val.is_infinite() {
                pts.push([
                    st.all_samples[abs_start].timestamp_ms as f64 / 1000.0,
                    f64::NAN,
                ]);
            } else if min_idx <= max_idx {
                pts.push([st.all_samples[min_idx].timestamp_ms as f64 / 1000.0, min_val]);
                if min_idx != max_idx {
                    pts.push([st.all_samples[max_idx].timestamp_ms as f64 / 1000.0, max_val]);
                }
            } else {
                pts.push([st.all_samples[max_idx].timestamp_ms as f64 / 1000.0, max_val]);
                pts.push([st.all_samples[min_idx].timestamp_ms as f64 / 1000.0, min_val]);
            }
        }
        pts
    }

    #[test]
    #[ignore = "timing benchmark; run with: cargo test --release -- --ignored --nocapture"]
    fn timing_old_vs_new() {
        use std::hint::black_box;
        use std::time::Instant;

        let n = 500_000;
        let values = noisy_values(n, 0xDEAD_BEEF, 5000);
        let st = build_state(&values);
        let extract = |s: &PlotSample| f64::from(s.voltage_v);
        // Realistic full-view decimation: ~2000 buckets over 500k samples.
        let buckets = 2000usize;
        let max_points = buckets * 2;
        let iters: u128 = 200;

        // Warm up + defeat DCE.
        black_box(old_flat(&st, extract, n, buckets).len());
        black_box(st.points_from_samples_multi(|s| [f64::from(s.voltage_v)], max_points, None, true));

        let t0 = Instant::now();
        for _ in 0..iters {
            black_box(old_flat(&st, black_box(extract), n, buckets));
        }
        let old_ns = t0.elapsed().as_nanos() / iters;

        let t1 = Instant::now();
        for _ in 0..iters {
            black_box(st.points_from_samples_multi(
                |s| [f64::from(s.voltage_v)],
                black_box(max_points),
                None,
                true,
            ));
        }
        let new_ns = t1.elapsed().as_nanos() / iters;

        let speedup = old_ns as f64 / new_ns as f64;
        println!(
            "min-max decimation over {n} samples, {buckets} buckets, {iters} iters:\n  \
             old (argmin/argmax, true x):     {old_ns:>8} ns/call\n  \
             new (branchless bucket-center):  {new_ns:>8} ns/call\n  \
             speedup: {speedup:.3}x"
        );
    }
}
