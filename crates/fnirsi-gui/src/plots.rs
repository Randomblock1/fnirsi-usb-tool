//! Real-time measurement plots.

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use fnirsi_protocol::Sample;
use std::collections::VecDeque;

/// Circular-buffered storage for the real-time measurement plots.
pub struct PlotState {
    all_samples: VecDeque<Sample>,
    energy_wh: VecDeque<f32>,
    capacity_mah: VecDeque<f32>,
    sample_capacity: usize,
    index: usize,
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
            energy_wh: VecDeque::with_capacity(capacity),
            capacity_mah: VecDeque::with_capacity(capacity),
            sample_capacity: capacity,
            index: 0,
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
            self.energy_wh.pop_front();
            self.capacity_mah.pop_front();
        }
        self.all_samples.push_back(*sample);
        self.energy_wh.push_back(energy_wh as f32);
        self.capacity_mah.push_back(capacity_mah as f32);
        self.index += 1;

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
        self.energy_wh.clear();
        self.capacity_mah.clear();
        self.index = 0;
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

    pub fn memory_bytes(&self) -> usize {
        self.all_samples.len() * (std::mem::size_of::<Sample>() + 8)
    }

    pub const fn samples(&self) -> &VecDeque<Sample> {
        &self.all_samples
    }

    /// Draw all enabled plots in a responsive grid layout.
    pub fn show(
        &self,
        ui: &mut egui::Ui,
        show_v: bool,
        show_d: bool,
        show_i: bool,
        show_p: bool,
        show_t: bool,
        show_e: bool,
        show_c: bool,
    ) {
        let active = [
            (show_v, "voltage_plot", "Voltage", "V"),
            (show_i, "current_plot", "Current", "A"),
            (show_p, "power_plot", "Power", "W"),
            (show_t, "temperature_plot", "Temperature", "°C"),
            (show_e, "energy_plot", "Energy", "Wh"),
            (show_c, "capacity_plot", "Capacity", "mAh"),
        ];

        let active_count = active.iter().filter(|x| x.0).count();
        if active_count == 0 {
            return;
        }

        let available = ui.available_size();
        let cols = if active_count > 2 { 2 } else { 1 };
        let rows = active_count.div_ceil(cols);

        let spacing = ui.spacing().item_spacing;
        let plot_width = (spacing.x.mul_add(-((cols - 1) as f32), available.x) / cols as f32).max(100.0);
        let plot_height = (spacing.y.mul_add(-((rows - 1) as f32), available.y) / rows as f32).max(100.0);
        let size = [plot_width, plot_height];
        // Max points for min-max decimation: 4 points per pixel gives good spike fidelity.
        let max_points = (plot_width * 4.0) as usize;

        ui.vertical(|ui| {
            let mut current_col = 0;
            ui.horizontal_wrapped(|ui| {
                if show_v {
                    self.show_plot(
                        ui,
                        "voltage_plot",
                        "Voltage",
                        "V",
                        size,
                        |plot_ui| {
                            plot_ui.line(
                                Line::new(self.points_from_samples(|s| f64::from(s.voltage_v), max_points))
                                    .color(egui::Color32::from_rgb(100, 180, 255))
                                    .width(1.5)
                                    .name("V"),
                            );
                            if show_d {
                                plot_ui.line(
                                    Line::new(self.points_from_samples(|s| f64::from(s.dp_v), max_points))
                                        .color(egui::Color32::from_rgb(100, 255, 100))
                                        .width(1.5)
                                        .name("D+"),
                                );
                                plot_ui.line(
                                    Line::new(self.points_from_samples(|s| f64::from(s.dn_v), max_points))
                                        .color(egui::Color32::from_rgb(100, 255, 255))
                                        .width(1.5)
                                        .name("D−"),
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

                if show_i {
                    self.show_plot(
                        ui,
                        "current_plot",
                        "Current",
                        "A",
                        size,
                        |plot_ui| {
                            plot_ui.line(
                                Line::new(self.points_from_samples(|s| f64::from(s.current_a), max_points))
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

                if show_p {
                    self.show_plot(
                        ui,
                        "power_plot",
                        "Power",
                        "W",
                        size,
                        |plot_ui| {
                            plot_ui.line(
                                Line::new(self.points_from_samples(|s| f64::from(s.power_w), max_points))
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

                if show_t {
                    self.show_plot(
                        ui,
                        "temperature_plot",
                        "Temperature",
                        "°C",
                        size,
                        |plot_ui| {
                            plot_ui.line(
                                Line::new(self.points_from_samples(|s| f64::from(s.temp_c), max_points))
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

                if show_e {
                    self.show_plot(
                        ui,
                        "energy_plot",
                        "Energy",
                        "Wh",
                        size,
                        |plot_ui| {
                            plot_ui.line(
                                Line::new(
                                    self.points_from_iter(self.energy_wh.iter().map(|&v| f64::from(v)), max_points),
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

                if show_c {
                    self.show_plot(
                        ui,
                        "capacity_plot",
                        "Capacity",
                        "mAh",
                        size,
                        |plot_ui| {
                            plot_ui.line(
                                Line::new(
                                    self.points_from_iter(
                                        self.capacity_mah.iter().map(|&v| f64::from(v)),
                                        max_points,
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

    /// Build plot points by extracting a value from each sample, with min-max decimation.
    ///
    /// When the sample count exceeds `max_points`, the data is divided into `max_points/2`
    /// buckets and each bucket emits two points (min and max Y in temporal order), preserving
    /// all visual spikes. When below the threshold, all points are returned directly.
    fn points_from_samples(
        &self,
        extract: impl Fn(&Sample) -> f64,
        max_points: usize,
    ) -> PlotPoints<'static> {
        let count = self.all_samples.len();
        if count == 0 {
            return PlotPoints::new(vec![]);
        }
        if max_points == 0 || count <= max_points {
            let mut pts = Vec::with_capacity(count);
            for s in &self.all_samples {
                pts.push([s.timestamp_ms as f64 / 1000.0, extract(s)]);
            }
            return PlotPoints::new(pts);
        }

        // Min-max bucket decimation.
        let buckets = (max_points / 2).max(1);
        let mut pts = Vec::with_capacity(buckets * 2);
        let bucket_size = count as f64 / buckets as f64;

        for b in 0..buckets {
            let start = (b as f64 * bucket_size) as usize;
            let end = (((b + 1) as f64 * bucket_size) as usize).min(count);
            if start >= end {
                continue;
            }

            let mut min_val = f64::INFINITY;
            let mut max_val = f64::NEG_INFINITY;
            let mut min_idx = start;
            let mut max_idx = start;

            for i in start..end {
                let s = &self.all_samples[i];
                let v = extract(s);
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
                // All NaN bucket — emit a NaN sentinel to create a gap.
                let s = &self.all_samples[start];
                pts.push([s.timestamp_ms as f64 / 1000.0, f64::NAN]);
            } else if min_idx <= max_idx {
                let s_min = &self.all_samples[min_idx];
                let s_max = &self.all_samples[max_idx];
                pts.push([s_min.timestamp_ms as f64 / 1000.0, min_val]);
                if min_idx != max_idx {
                    pts.push([s_max.timestamp_ms as f64 / 1000.0, max_val]);
                }
            } else {
                let s_max = &self.all_samples[max_idx];
                let s_min = &self.all_samples[min_idx];
                pts.push([s_max.timestamp_ms as f64 / 1000.0, max_val]);
                pts.push([s_min.timestamp_ms as f64 / 1000.0, min_val]);
            }
        }

        PlotPoints::new(pts)
    }

    /// Build plot points from an external iterator (e.g. energy/capacity), with min-max decimation.
    fn points_from_iter(
        &self,
        extract: impl Iterator<Item = f64>,
        max_points: usize,
    ) -> PlotPoints<'static> {
        let count = self.all_samples.len();
        if count == 0 {
            return PlotPoints::new(vec![]);
        }

        // Collect into a temporary vec so we can apply decimation.
        let values: Vec<f64> = extract.take(count).collect();
        let actual = values.len();

        if max_points == 0 || actual <= max_points {
            let mut pts = Vec::with_capacity(actual);
            for (i, v) in values.into_iter().enumerate() {
                if let Some(s) = self.all_samples.get(i) {
                    pts.push([s.timestamp_ms as f64 / 1000.0, v]);
                }
            }
            return PlotPoints::new(pts);
        }

        let buckets = (max_points / 2).max(1);
        let mut pts = Vec::with_capacity(buckets * 2);
        let bucket_size = actual as f64 / buckets as f64;

        for b in 0..buckets {
            let start = (b as f64 * bucket_size) as usize;
            let end = (((b + 1) as f64 * bucket_size) as usize).min(actual);
            if start >= end {
                continue;
            }

            let mut min_val = f64::INFINITY;
            let mut max_val = f64::NEG_INFINITY;
            let mut min_idx = start;
            let mut max_idx = start;

            for i in start..end {
                let v = values[i];
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
                if let Some(s) = self.all_samples.get(start) {
                    pts.push([s.timestamp_ms as f64 / 1000.0, f64::NAN]);
                }
            } else if min_idx <= max_idx {
                if let Some(s) = self.all_samples.get(min_idx) {
                    pts.push([s.timestamp_ms as f64 / 1000.0, min_val]);
                }
                if min_idx != max_idx {
                    if let Some(s) = self.all_samples.get(max_idx) {
                        pts.push([s.timestamp_ms as f64 / 1000.0, max_val]);
                    }
                }
            } else {
                if let Some(s) = self.all_samples.get(max_idx) {
                    pts.push([s.timestamp_ms as f64 / 1000.0, max_val]);
                }
                if let Some(s) = self.all_samples.get(min_idx) {
                    pts.push([s.timestamp_ms as f64 / 1000.0, min_val]);
                }
            }
        }

        PlotPoints::new(pts)
    }

    /// Look up the sample value nearest to the given X (seconds) coordinate.
    fn value_from_samples(&self, extract: impl Fn(&Sample) -> f64, x: f64) -> Option<(f64, f64)> {
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

        let idx = self
            .all_samples
            .binary_search_by_key(&target_ms, |s| s.timestamp_ms)
            .unwrap_or_else(|e| e);

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

        let idx = self
            .all_samples
            .binary_search_by_key(&target_ms, |s| s.timestamp_ms)
            .unwrap_or_else(|e| e);

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
        let latest_str = latest_val.map_or_else(|| "—".to_string(), |v| format!("{v:.3}"));

        let mut hovered_point = None;

        let response = Plot::new(id)
            .height(size[1])
            .width(size[0])
            .include_y(0.0)
            .include_y(0.1)
            .y_axis_label(unit)
            .show_axes([true, true])
            .set_margin_fraction(egui::Vec2::ZERO)
            .label_formatter(|_name, _value| String::new())
            .show(ui, |plot_ui| {
                add_lines(plot_ui);

                let pointer_pos = plot_ui.ctx().input(|i| i.pointer.hover_pos());
                let is_hovered = pointer_pos.is_some_and(|p| plot_ui.response().rect.contains(p));

                if is_hovered
                    && let Some(pointer) = plot_ui.pointer_coordinate()
                        && let Some((x, y)) = value_at(pointer.x) {
                            plot_ui.points(
                                egui_plot::Points::new(vec![[x, y]])
                                    .radius(4.0)
                                    .color(egui::Color32::WHITE)
                                    .shape(egui_plot::MarkerShape::Circle),
                            );
                            hovered_point = Some((x, y));
                        }
            });

        if response.response.hovered()
            && let Some((x, y)) = hovered_point {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    egui::Id::new(id).with("tooltip"),
                    |ui| {
                        ui.label(
                            egui::RichText::new(format!("{label}: {y:.3} {unit}\ntime: {x:.3} s"))
                                .size(14.0)
                                .strong(),
                        );
                    },
                );
            }

        // Overlay title with live value
        let title_rect = response.response.rect;
        let painter = ui.painter();
        let title_text = format!("{label}: {latest_str} {unit}");
        painter.text(
            egui::pos2(title_rect.left() + 5.0, title_rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            title_text,
            egui::FontId::proportional(13.0),
            primary_color,
        );
    }
}
