//! Incremental min/max aggregation for plot decimation.
//!
//! One level of pre-aggregation over the raw sample buffer: consecutive
//! samples are grouped into buckets of [`BUCKET_SAMPLES`], each holding
//! min/max summaries for every [`Channel`]. Appends fold into the trailing
//! bucket in O(1); front evictions drop whole buckets once all of their
//! samples are gone. A full-range min-max decimation then walks
//! O(n / [`BUCKET_SAMPLES`]) summaries instead of every sample, emitting in
//! the same bucket-center lo-then-hi grammar as the raw fused walk in
//! `plots.rs` (both go through [`emit_bucket`]).

use crate::plots::PlotSample;
use std::collections::VecDeque;

/// Samples covered by one sealed bucket.
///
/// The render path only uses summaries when a render bucket spans at least
/// this many samples; zoomed in past that granularity it falls back to
/// walking the raw samples.
pub const BUCKET_SAMPLES: usize = 256;

/// Per-sample channels the pyramid summarizes (the ones
/// `PlotState::points_from_samples_multi` renders).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Voltage,
    Current,
    Power,
    Temp,
    Dp,
    Dn,
}

impl Channel {
    const COUNT: usize = 6;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Voltage,
        Self::Current,
        Self::Power,
        Self::Temp,
        Self::Dp,
        Self::Dn,
    ];

    /// Read this channel's value from a sample.
    #[must_use]
    pub const fn extract(self, sample: &PlotSample) -> f32 {
        match self {
            Self::Voltage => sample.voltage_v,
            Self::Current => sample.current_a,
            Self::Power => sample.power_w,
            Self::Temp => sample.temp_c,
            Self::Dp => sample.dp_v,
            Self::Dn => sample.dn_v,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Min/max of a set of samples, with the timestamps where they occur.
///
/// Ties keep the first occurrence (strict `<` / `>`), so results are
/// deterministic and exactly comparable with a front-to-back brute-force
/// scan. Rendering emits at bucket centers and only reads `min` / `max` /
/// `any`; the timestamps pin down that determinism in the property tests and
/// keep the summaries reusable for true-timestamp emission.
#[derive(Clone, Copy, Debug)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
    pub min_ts_ms: u64,
    pub max_ts_ms: u64,
    /// `false` while every sample seen was NaN (a gap in the data).
    pub any: bool,
}

impl MinMax {
    pub const EMPTY: Self = Self {
        min: 0.0,
        max: 0.0,
        min_ts_ms: 0,
        max_ts_ms: 0,
        any: false,
    };

    const fn add(&mut self, value: f32, ts_ms: u64) {
        if value.is_nan() {
            return;
        }
        if self.any {
            if value < self.min {
                self.min = value;
                self.min_ts_ms = ts_ms;
            }
            if value > self.max {
                self.max = value;
                self.max_ts_ms = ts_ms;
            }
        } else {
            *self = Self {
                min: value,
                max: value,
                min_ts_ms: ts_ms,
                max_ts_ms: ts_ms,
                any: true,
            };
        }
    }

    const fn merge(&mut self, other: &Self) {
        if !other.any {
            return;
        }
        if !self.any {
            *self = *other;
            return;
        }
        if other.min < self.min {
            self.min = other.min;
            self.min_ts_ms = other.min_ts_ms;
        }
        if other.max > self.max {
            self.max = other.max;
            self.max_ts_ms = other.max_ts_ms;
        }
    }

    /// Widened `(lo, hi)` pair in the shape [`emit_bucket`] expects: a fold
    /// that never saw a finite value leaves `lo == +INFINITY`, which signals
    /// the gap sentinel.
    fn fold_pair(&self) -> (f64, f64) {
        if self.any {
            (f64::from(self.min), f64::from(self.max))
        } else {
            (f64::INFINITY, f64::NEG_INFINITY)
        }
    }
}

/// Summary of up to [`BUCKET_SAMPLES`] consecutive samples, all channels.
#[derive(Debug)]
struct Bucket {
    /// Samples folded into this bucket (== [`BUCKET_SAMPLES`] once sealed).
    len: usize,
    channels: [MinMax; Channel::COUNT],
}

impl Bucket {
    const EMPTY: Self = Self {
        len: 0,
        channels: [MinMax::EMPTY; Channel::COUNT],
    };
}

/// Emit one decimated bucket for a single lane: both extremes at the
/// bucket-center `x` (`lo` then `hi`), or a single NaN sentinel to open a gap
/// when the fold never saw a finite value (an all-NaN bucket leaves
/// `lo == +INFINITY`).
///
/// Equal-x points render as the vertical bar that min-max decimation already
/// draws. Shared by the raw fused walk in `plots.rs` and the summary path
/// below, so both produce the same visual grammar.
pub fn emit_bucket(pts: &mut Vec<[f64; 2]>, x: f64, lo: f64, hi: f64) {
    if lo.is_infinite() {
        pts.push([x, f64::NAN]);
    } else {
        pts.push([x, lo]);
        pts.push([x, hi]);
    }
}

/// One aggregation level maintained incrementally alongside the raw sample
/// deque. Deque index `i` corresponds to offset `front_skip + i` from the
/// start of the first bucket; every bucket except the trailing one is sealed
/// at exactly [`BUCKET_SAMPLES`] samples.
#[derive(Debug, Default)]
pub struct Pyramid {
    buckets: VecDeque<Bucket>,
    /// Samples already evicted from the deque but still folded into the first
    /// bucket's aggregates. While non-zero the first bucket's summaries are
    /// stale, so queries walk its remaining raw samples instead.
    front_skip: usize,
}

impl Pyramid {
    /// Number of live samples the summaries cover; always equals the length
    /// of the sample deque this pyramid shadows.
    #[must_use]
    pub fn covered_samples(&self) -> usize {
        self.buckets.iter().map(|b| b.len).sum::<usize>() - self.front_skip
    }

    /// Heap footprint of the summaries.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.buckets.len() * std::mem::size_of::<Bucket>()
    }

    /// Pre-allocate bucket storage for `additional` more samples ahead of a
    /// known-size bulk import.
    pub fn reserve(&mut self, additional: usize) {
        self.buckets.reserve(additional.div_ceil(BUCKET_SAMPLES));
    }

    /// Fold one appended sample into the trailing bucket. O(1).
    pub fn push(&mut self, sample: &PlotSample) {
        match self.buckets.back_mut() {
            Some(bucket) if bucket.len < BUCKET_SAMPLES => Self::fold_into(bucket, sample),
            _ => {
                let mut bucket = Bucket::EMPTY;
                Self::fold_into(&mut bucket, sample);
                self.buckets.push_back(bucket);
            }
        }
    }

    fn fold_into(bucket: &mut Bucket, sample: &PlotSample) {
        bucket.len += 1;
        for ch in Channel::ALL {
            bucket.channels[ch.index()].add(ch.extract(sample), sample.timestamp_ms);
        }
    }

    /// Record that the oldest `count` samples were removed from the deque.
    /// Summaries only drop as whole buckets; until every sample of the first
    /// bucket is gone it is retained and marked partially evicted.
    pub fn evict_front(&mut self, count: usize) {
        self.front_skip += count;
        while let Some(front) = self.buckets.front() {
            if self.front_skip < front.len {
                break;
            }
            self.front_skip -= front.len;
            self.buckets.pop_front();
        }
        if self.buckets.is_empty() {
            self.front_skip = 0;
        }
    }

    pub fn clear(&mut self) {
        self.buckets.clear();
        self.front_skip = 0;
    }

    /// Exact min/max of `channel` over deque index range `start..end`.
    ///
    /// Buckets fully covered by the range (and not partially evicted) fold
    /// their summaries in O(1); partial coverage at the edges walks the raw
    /// samples, so the result is identical to a brute-force scan of
    /// `samples[start..end]`.
    #[must_use]
    pub fn range_min_max(
        &self,
        samples: &VecDeque<PlotSample>,
        channel: Channel,
        start: usize,
        end: usize,
    ) -> MinMax {
        let mut agg = MinMax::EMPTY;
        if start >= end {
            return agg;
        }
        let abs_start = self.front_skip + start;
        let abs_end = self.front_skip + end;
        let first_bucket = abs_start / BUCKET_SAMPLES;
        let last_bucket = (abs_end - 1) / BUCKET_SAMPLES;
        for k in first_bucket..=last_bucket {
            let bucket = &self.buckets[k];
            let bucket_lo = k * BUCKET_SAMPLES;
            let lo = abs_start.max(bucket_lo);
            let hi = abs_end.min(bucket_lo + bucket.len);
            if lo == bucket_lo && hi == bucket_lo + bucket.len {
                agg.merge(&bucket.channels[channel.index()]);
            } else {
                for offset in lo..hi {
                    let s = &samples[offset - self.front_skip];
                    agg.add(channel.extract(s), s.timestamp_ms);
                }
            }
        }
        agg
    }

    /// Min-max decimation of `samples[start..end]` into at most
    /// `render_buckets` buckets per lane, walking summaries instead of
    /// samples — the fused multi-lane counterpart of the raw walk in
    /// `PlotState::points_from_samples_multi`.
    ///
    /// Render bucket boundaries are aligned to summary buckets (the raw path
    /// spreads them fractionally instead), so interior render buckets fold
    /// sealed summaries and only the two edge buckets can walk raw samples.
    /// Emission goes through the shared [`emit_bucket`]: both extremes at the
    /// bucket-center timestamp (`lo` then `hi`), or a NaN sentinel for an
    /// all-NaN gap — the same visual grammar as the raw walk, so crossing the
    /// summary threshold while zooming never changes how lines look.
    #[must_use]
    pub fn min_max_points_multi<const N: usize>(
        &self,
        samples: &VecDeque<PlotSample>,
        channels: [Channel; N],
        start: usize,
        end: usize,
        render_buckets: usize,
    ) -> [Vec<[f64; 2]>; N] {
        if start >= end || render_buckets == 0 {
            return std::array::from_fn(|_| Vec::new());
        }
        let abs_start = self.front_skip + start;
        let abs_end = self.front_skip + end;
        let first_bucket = abs_start / BUCKET_SAMPLES;
        let bucket_count = (abs_end - 1) / BUCKET_SAMPLES + 1 - first_bucket;
        let render_buckets = render_buckets.min(bucket_count);
        let mut pts: [Vec<[f64; 2]>; N] =
            std::array::from_fn(|_| Vec::with_capacity(render_buckets * 2));
        for r in 0..render_buckets {
            let ka = first_bucket + r * bucket_count / render_buckets;
            let kb = first_bucket + (r + 1) * bucket_count / render_buckets;
            let lo = (ka * BUCKET_SAMPLES).max(abs_start) - self.front_skip;
            let hi = (kb * BUCKET_SAMPLES).min(abs_end) - self.front_skip;
            let x = samples[usize::midpoint(lo, hi)].timestamp_ms as f64 / 1000.0;
            for (dst, ch) in pts.iter_mut().zip(channels) {
                let (fold_lo, fold_hi) = self.range_min_max(samples, ch, lo, hi).fold_pair();
                emit_bucket(dst, x, fold_lo, fold_hi);
            }
        }
        pts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic LCG so property tests are reproducible.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn next_f32(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32
        }

        fn next_usize(&mut self, bound: usize) -> usize {
            (self.next() >> 33) as usize % bound.max(1)
        }
    }

    /// Channels get distinct derived values so a mixed-up channel index shows
    /// up as a test failure. NaN input propagates to every channel.
    fn sample(ts_ms: u64, v: f32) -> PlotSample {
        PlotSample {
            timestamp_ms: ts_ms,
            voltage_v: v,
            current_a: -v,
            power_w: v + 1.0,
            dp_v: v * 0.5,
            dn_v: 3.0 - v,
            temp_c: v.abs(),
        }
    }

    /// Independent brute-force scan mirroring the first-occurrence tie-break.
    fn brute(samples: &VecDeque<PlotSample>, ch: Channel, start: usize, end: usize) -> MinMax {
        let mut agg = MinMax::EMPTY;
        for s in samples.iter().take(end).skip(start) {
            let v = ch.extract(s);
            if v.is_nan() {
                continue;
            }
            if agg.any {
                if v < agg.min {
                    agg.min = v;
                    agg.min_ts_ms = s.timestamp_ms;
                }
                if v > agg.max {
                    agg.max = v;
                    agg.max_ts_ms = s.timestamp_ms;
                }
            } else {
                agg = MinMax {
                    min: v,
                    max: v,
                    min_ts_ms: s.timestamp_ms,
                    max_ts_ms: s.timestamp_ms,
                    any: true,
                };
            }
        }
        agg
    }

    fn assert_minmax_eq(got: &MinMax, want: &MinMax, ctx: &str) {
        assert_eq!(got.any, want.any, "any mismatch: {ctx}");
        if want.any {
            assert_eq!(got.min.to_bits(), want.min.to_bits(), "min mismatch: {ctx}");
            assert_eq!(got.max.to_bits(), want.max.to_bits(), "max mismatch: {ctx}");
            assert_eq!(got.min_ts_ms, want.min_ts_ms, "min ts mismatch: {ctx}");
            assert_eq!(got.max_ts_ms, want.max_ts_ms, "max ts mismatch: {ctx}");
        }
    }

    /// Reference for `min_max_points_multi`: identical bucket alignment and
    /// bucket-center emission, but every range aggregated by brute force over
    /// the raw samples (independent of the production fold and emit).
    fn reference_points(
        pyr: &Pyramid,
        samples: &VecDeque<PlotSample>,
        ch: Channel,
        start: usize,
        end: usize,
        render_buckets: usize,
    ) -> Vec<[f64; 2]> {
        if start >= end || render_buckets == 0 {
            return Vec::new();
        }
        let abs_start = pyr.front_skip + start;
        let abs_end = pyr.front_skip + end;
        let first_bucket = abs_start / BUCKET_SAMPLES;
        let bucket_count = (abs_end - 1) / BUCKET_SAMPLES + 1 - first_bucket;
        let render_buckets = render_buckets.min(bucket_count);
        let mut pts = Vec::new();
        for r in 0..render_buckets {
            let ka = first_bucket + r * bucket_count / render_buckets;
            let kb = first_bucket + (r + 1) * bucket_count / render_buckets;
            let lo = (ka * BUCKET_SAMPLES).max(abs_start) - pyr.front_skip;
            let hi = (kb * BUCKET_SAMPLES).min(abs_end) - pyr.front_skip;
            let agg = brute(samples, ch, lo, hi);
            let x = samples[usize::midpoint(lo, hi)].timestamp_ms as f64 / 1000.0;
            if agg.any {
                pts.push([x, f64::from(agg.min)]);
                pts.push([x, f64::from(agg.max)]);
            } else {
                pts.push([x, f64::NAN]);
            }
        }
        pts
    }

    fn assert_points_eq(got: &[[f64; 2]], want: &[[f64; 2]], ctx: &str) {
        assert_eq!(got.len(), want.len(), "point count mismatch: {ctx}");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(g[0].to_bits(), w[0].to_bits(), "x mismatch at {i}: {ctx}");
            assert_eq!(g[1].to_bits(), w[1].to_bits(), "y mismatch at {i}: {ctx}");
        }
    }

    /// Circular buffer driver: push `total` samples through a deque of
    /// `capacity`, mirroring evictions into the pyramid, calling `checkpoint`
    /// periodically.
    fn drive(
        capacity: usize,
        total: usize,
        rng: &mut Lcg,
        mut checkpoint: impl FnMut(&Pyramid, &VecDeque<PlotSample>),
    ) {
        let mut samples: VecDeque<PlotSample> = VecDeque::new();
        let mut pyr = Pyramid::default();
        for i in 0..total {
            let v = if rng.next_usize(20) == 0 {
                f32::NAN
            } else {
                rng.next_f32().mul_add(10.0, -5.0)
            };
            let s = sample(i as u64 * 10, v);
            if samples.len() >= capacity {
                samples.pop_front();
                pyr.evict_front(1);
            }
            samples.push_back(s);
            pyr.push(&s);
            assert_eq!(pyr.covered_samples(), samples.len(), "desync at push {i}");
            if i % 97 == 0 {
                checkpoint(&pyr, &samples);
            }
        }
        checkpoint(&pyr, &samples);
    }

    #[test]
    fn bookkeeping_tracks_sample_count_through_evictions() {
        let mut rng = Lcg(7);
        let mut samples: VecDeque<PlotSample> = VecDeque::new();
        let mut pyr = Pyramid::default();
        let mut ts = 0u64;
        for round in 0..200 {
            let pushes = rng.next_usize(700) + 1;
            for _ in 0..pushes {
                ts += 10;
                let s = sample(ts, rng.next_f32());
                samples.push_back(s);
                pyr.push(&s);
            }
            // Bulk eviction as set_capacity performs via drain(..excess).
            let evict = rng.next_usize(samples.len() + 1);
            samples.drain(..evict);
            pyr.evict_front(evict);
            assert_eq!(pyr.covered_samples(), samples.len(), "round {round}");
            if let Some(front) = pyr.buckets.front() {
                assert!(pyr.front_skip < front.len, "front over-evicted");
            } else {
                assert_eq!(pyr.front_skip, 0);
                assert!(samples.is_empty());
            }
            if round % 50 == 0 {
                samples.clear();
                pyr.clear();
                assert_eq!(pyr.covered_samples(), 0);
            }
        }
    }

    #[test]
    fn range_min_max_matches_brute_force_across_eviction_cycles() {
        let mut rng = Lcg(0xDEAD_BEEF);
        let mut query_rng = Lcg(0x5EED);
        // Capacity deliberately not a multiple of BUCKET_SAMPLES; ~5 full
        // wraparounds keep the front bucket partially evicted most of the time.
        drive(1000, 5000, &mut rng, |pyr, samples| {
            let len = samples.len();
            for ch in Channel::ALL {
                let got = pyr.range_min_max(samples, ch, 0, len);
                let want = brute(samples, ch, 0, len);
                assert_minmax_eq(&got, &want, "full range");
            }
            for _ in 0..10 {
                let a = query_rng.next_usize(len + 1);
                let b = query_rng.next_usize(len + 1);
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                for ch in Channel::ALL {
                    let got = pyr.range_min_max(samples, ch, start, end);
                    let want = brute(samples, ch, start, end);
                    assert_minmax_eq(&got, &want, &format!("range {start}..{end}"));
                }
            }
        });
    }

    #[test]
    fn min_max_points_multi_matches_reference_decimation() {
        let mut rng = Lcg(0xCAFE);
        let mut query_rng = Lcg(0xF00D);
        drive(2000, 9000, &mut rng, |pyr, samples| {
            let len = samples.len();
            for &render_buckets in &[1usize, 3, 7, 50] {
                let [got] =
                    pyr.min_max_points_multi(samples, [Channel::Voltage], 0, len, render_buckets);
                let want = reference_points(pyr, samples, Channel::Voltage, 0, len, render_buckets);
                assert_points_eq(
                    &got,
                    &want,
                    &format!("full range, {render_buckets} buckets"),
                );
            }
            let a = query_rng.next_usize(len + 1);
            let b = query_rng.next_usize(len + 1);
            let (start, end) = if a <= b { (a, b) } else { (b, a) };
            // Fused lanes must each match their own single-channel reference.
            let [got_a, got_dn] =
                pyr.min_max_points_multi(samples, [Channel::Current, Channel::Dn], start, end, 5);
            let want_a = reference_points(pyr, samples, Channel::Current, start, end, 5);
            let want_dn = reference_points(pyr, samples, Channel::Dn, start, end, 5);
            assert_points_eq(&got_a, &want_a, &format!("current, range {start}..{end}"));
            assert_points_eq(&got_dn, &want_dn, &format!("dn, range {start}..{end}"));
        });
    }

    #[test]
    fn all_nan_ranges_propagate_gap_sentinel() {
        let mut samples: VecDeque<PlotSample> = VecDeque::new();
        let mut pyr = Pyramid::default();
        // 1024 real samples, then 512 NaN, then 512 real: the NaN run spans
        // two whole buckets.
        for i in 0..2048u64 {
            let v = if (1024..1536).contains(&i) {
                f32::NAN
            } else {
                1.0
            };
            let s = sample(i * 10, v);
            samples.push_back(s);
            pyr.push(&s);
        }
        let gap = pyr.range_min_max(&samples, Channel::Voltage, 1024, 1536);
        assert!(!gap.any, "all-NaN range must report no value");
        let mixed = pyr.range_min_max(&samples, Channel::Voltage, 1000, 1600);
        assert!(mixed.any);

        // 8 render buckets over 8 summary buckets: buckets 4 and 5 are pure
        // NaN and must emit exactly one sentinel each, at their bucket-center
        // sample (midpoints of 1024..1280 and 1280..1536).
        let [pts] = pyr.min_max_points_multi(&samples, [Channel::Voltage], 0, 2048, 8);
        let nan_xs: Vec<f64> = pts.iter().filter(|p| p[1].is_nan()).map(|p| p[0]).collect();
        let expected = [
            f64::from(1152u32 * 10) / 1000.0,
            f64::from(1408u32 * 10) / 1000.0,
        ];
        assert_eq!(nan_xs.len(), 2);
        assert!((nan_xs[0] - expected[0]).abs() < 1e-9);
        assert!((nan_xs[1] - expected[1]).abs() < 1e-9);
    }

    /// Raw min-max decimation mirroring the fallback walk in `plots.rs`
    /// (fractional bucket boundaries, branchless fold, bucket-center
    /// emission), used as the timing baseline.
    fn raw_decimate(
        samples: &VecDeque<PlotSample>,
        ch: Channel,
        start_idx: usize,
        end_idx: usize,
        buckets: usize,
    ) -> Vec<[f64; 2]> {
        let slice_len = end_idx - start_idx;
        let mut pts = Vec::with_capacity(buckets * 2);
        for b in 0..buckets {
            let local_start = b * slice_len / buckets;
            let local_end = (b + 1) * slice_len / buckets;
            if local_start >= local_end {
                continue;
            }
            let abs_start = start_idx + local_start;
            let abs_end = start_idx + local_end;
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for s in samples.range(abs_start..abs_end) {
                let v = f64::from(ch.extract(s));
                lo = lo.min(v);
                hi = hi.max(v);
            }
            let x = samples[usize::midpoint(abs_start, abs_end)].timestamp_ms as f64 / 1000.0;
            emit_bucket(&mut pts, x, lo, hi);
        }
        pts
    }

    #[test]
    #[ignore = "release-mode timing measurement; run with --release --ignored --nocapture"]
    fn timing_summary_vs_raw_decimation_1m() {
        use std::hint::black_box;
        use std::time::Instant;

        let n = 1_000_000usize;
        let mut rng = Lcg(42);
        let mut samples: VecDeque<PlotSample> = VecDeque::with_capacity(n);
        let mut pyr = Pyramid::default();
        // 200k extra pushes so the pyramid sits mid-eviction like steady state.
        for i in 0..n + 200_000 {
            let s = sample(i as u64 * 10, rng.next_f32());
            if samples.len() >= n {
                samples.pop_front();
                pyr.evict_front(1);
            }
            samples.push_back(s);
            pyr.push(&s);
        }
        // ~500 px wide plot at 8 points/px -> max_points 4000 -> 2000 buckets.
        let render_buckets = 2000;
        let iters = 200u32;

        let t0 = Instant::now();
        for _ in 0..iters {
            black_box(raw_decimate(
                black_box(&samples),
                Channel::Voltage,
                0,
                samples.len(),
                render_buckets,
            ));
        }
        let raw = t0.elapsed();

        let t1 = Instant::now();
        for _ in 0..iters {
            black_box(pyr.min_max_points_multi(
                black_box(&samples),
                [Channel::Voltage],
                0,
                samples.len(),
                render_buckets,
            ));
        }
        let summary = t1.elapsed();

        println!(
            "raw: {:?}/iter, summary: {:?}/iter, ratio: {:.1}x, \
             bucket bytes/sample: {:.3}",
            raw / iters,
            summary / iters,
            raw.as_secs_f64() / summary.as_secs_f64(),
            pyr.memory_bytes() as f64 / samples.len() as f64,
        );
    }
}
