# Mechanical-sympathy ideas

Ideas for making this codebase work *with* the CPU — cache-friendly layout, branchless
arithmetic, allocation-free steady state — in 100% safe Rust. The app is fast enough for
normal use; most of these are craft, and each one says so honestly. Every idea below was
adversarially verified against the real code (several by compiling candidate patterns with
`rustc -O` and comparing assembly); the refuted ones are kept at the bottom because *why
they fail* is half the lesson.

Sources: Bakhvalov, *Performance Analysis and Tuning on Modern CPUs* (2nd ed.);
[Algorithmica HPC](https://en.algorithmica.org/hpc/); [perf-ninja-rs](https://github.com/grahamking/perf-ninja-rs);
[Branchless binary search](https://pythonspeed.com/articles/branchless-binary-search/).

## Where the time actually goes

**Traffic = bytes touched × frequency.** The frequency map of this app:

| Path | Rate | Traffic |
|---|---|---|
| Plot decimation (`plots.rs::points_from_samples`) | per frame (20 fps) × per visible line | **dominant** — full-buffer walk when not zoomed: ~8.3 MB × 4 lines × 20 fps ≈ 660 MB/s at the default 10 MB preset; seconds/frame at 1 GB |
| Energy/capacity plotting (`points_from_iter`) | per frame when enabled | full-buffer `Vec<f64>` collect **before** decimation |
| Sample ingest (`app.rs::process_messages`) | ~100 Hz USB / ~10 Hz BLE | tiny |
| Packet decode + CRC (`protocol.rs`, `crc.rs`) | ~25 packets/s | negligible |
| Import/export (`csv_utils.rs`, `cfn.rs`) | per user action | cold — one-shot, but O(file) |
| Device scan/connect (`usb.rs`, `ble.rs`) | per user action | cold |

The one real cliff: with LOD on but not zoomed, `visible_range` returns the full buffer, so
**every frame re-walks the entire sample history once per visible plot line**. Everything in
Tier A attacks that path.

## Tier A — the per-frame plot path (real, if modest, wins)

### A1. Stop materializing the energy/capacity series into a `Vec<f64>` each frame
[plots.rs:606](crates/fnirsi-gui/src/plots.rs:606) — `let values: Vec<f64> = extract.take(count).collect();`
runs unconditionally before any decimation: a full-length heap allocation plus f32→f64 widen
of the whole buffer, per frame, per enabled energy/capacity plot (~2 MB/frame at the default
preset). The source deques (`energy_wh`, `capacity_mah`) are already dense, index-synchronized
`VecDeque<f32>` — pass `&VecDeque<f32>` directly and index it with the same indices used for
`all_samples`. Replaces `points_from_iter` with a `points_from_deque(&self, values: &VecDeque<f32>, …)`
mirroring `points_from_samples`. *Impact: low. Complexity: small–medium. Teaching: high
(allocation-free steady state; the collect was only ever there to enable indexing).*

### A2. Fuse the voltage/D+/D− walks into one pass
[plots.rs:218-260](crates/fnirsi-gui/src/plots.rs:218) — the voltage panel calls
`points_from_samples` three times (V, D+, D−), each independently binary-searching the
visible range (6 searches) and re-streaming the same cache lines (3 full passes). Replace
`points_from_samples` with a generic `points_from_samples_multi<const N: usize>` taking
`extract: impl Fn(&Sample) -> [f64; N]`; the other plots use `N = 1` so there is exactly one
decimation path. Per-lane min/max/idx state; one `VecDeque` index + one cache-line touch
feeds all lanes. *Impact: low (≈3×→1× bandwidth on that panel). Complexity: medium.
Teaching: high (loop fusion — touch each cache line once).*

### A3. Hot/cold split: keep export-only ADC fields out of the scanned buffer
[plots.rs:23](crates/fnirsi-gui/src/plots.rs:23) — `raw_voltage`/`raw_current` are read
*nowhere* in the GUI (verified by grep); they ride along in every 40-byte `Sample` the
decimation scan streams. A GUI-local 32-byte `PlotSample` (same field names → extract
closures and binary searches compile unchanged) plus a parallel `raw_adc: VecDeque<(u32,u32)>`
(the lockstep pattern `energy_wh`/`capacity_mah` already establish) cuts scanned footprint
20%. Export reconstructs `Sample`s by zipping — cold path. Do **not** touch the protocol
`Sample`. Memory-neutral overall (32+8+8 = 48 B/sample, same as today); update
`SIZEOF_SAMPLE` ([app.rs:83](crates/fnirsi-gui/src/app.rs:83)) and `memory_bytes`
([plots.rs:168](crates/fnirsi-gui/src/plots.rs:168)) so they don't drift. *Impact: low
(~20% of the dominant traffic). Complexity: medium. Teaching: high (hot/cold structure
splitting — the pragmatic stopping point short of full SoA, which was refuted, below).*

## Tier B — ingest and import (cheap wins)

### B1. `reserve` the plot deques before a bulk import
[app.rs:363](crates/fnirsi-gui/src/app.rs:363) — the import loop `push_back`s
`samples.len()` elements into three deques through the doubling-growth cascade. Add
`PlotState::reserve(&mut self, additional: usize)` calling `reserve_exact` on all three,
invoke it once before the loop. *Impact: low. Complexity: trivial.*

### B2. Countdown decimator instead of `is_multiple_of` per sample
[app.rs:476](crates/fnirsi-gui/src/app.rs:476) — the downsample gate divides by a
runtime-selected divider on every sample (`%`/`is_multiple_of` with a non-constant divisor
is a real `div` instruction). A countdown counter (`keep = counter == 0; counter = if keep
{ divider - 1 } else { counter - 1 }`) removes the division and reads as intent ("keep 1 in
N"). *Impact: negligible (100 Hz). Complexity: small. Teaching: high (strength reduction —
division is the one arithmetic op that stayed expensive).*

### B3. A `DeviceMessage::Sample(Sample)` variant for BLE
[app.rs:1165](crates/fnirsi-gui/src/app.rs:1165) — `vec![sample]` heap-allocates per BLE
notification purely to satisfy the batch-shaped `Samples(Vec<Sample>)` variant; LLVM cannot
elide it because the Vec crosses the channel. `Sample` is `Copy` and 40 B; the enum is
already ~104 B wide because of `Connected(DeviceInfo)`, so an inline variant widens nothing.
(The USB side's `samples.to_vec()` at [app.rs:1077](crates/fnirsi-gui/src/app.rs:1077) could
similarly become a `SamplePacket([Sample; 4])` variant.) *Impact: negligible (10 Hz).
Complexity: small. Teaching: high (don't heap-wrap one Copy value to fit a batch API).*

## Tier C — cold paths and pure craft

### C1. Multiply by reciprocal constants in `decode_sample`
[protocol.rs:103-113](crates/fnirsi-protocol/src/protocol.rs:103) — five f32 divisions by
constants per sample. **Verified empirically: LLVM does *not* fold `x / C` into `x * (1/C)`
without fast-math** (the results can differ by 1 ulp), so this is a real codegen change —
`divss` ~11 cycles vs `mulss` ~4. Write purpose-named reciprocal consts so units stay
readable: `const VI_SCALE: f32 = 1.0 / 100_000.0;` (raw LSB = 10 µV / 10 µA → V/A).
*Impact: negligible at 400 samples/s. Complexity: trivial. Teaching: high — the flagship
"check what the compiler actually can't do" example.*

### C2. Integer bucket boundaries in the decimation loops
[plots.rs:541](crates/fnirsi-gui/src/plots.rs:541) (and the duplicate at :641) — bucket
edges via `(b as f64 * bucket_size) as usize` pay a float multiply plus a saturating
float→int cast per bucket. Pure integer `b * slice_len / buckets` is exact, and the
`.min(slice_len)` clamp becomes provably redundant. *Impact: negligible. Complexity:
trivial. Teaching: high (saturating casts aren't free; integer index math).*

### C3. Resolve XLSX column indices once, not 9 hash lookups per row
[csv_utils.rs:172-179](crates/fnirsi-protocol/src/csv_utils.rs:172) — `get_num` re-hashes
constant header names against a SipHash `HashMap` nine times per row. Resolve each column's
`Option<usize>` once before the row loop (the Parquet reader at :336-344 already does this —
make the two readers consistent). *Impact: low (import path). Complexity: small. Teaching:
high (hoist loop-invariant lookups).*

### C4. Hoist Arrow type dispatch out of the Parquet row loop
[csv_utils.rs:299-320](crates/fnirsi-protocol/src/csv_utils.rs:299) — `numeric_value` runs a
6-way `downcast_ref` chain per cell (9 cells/row); the column's concrete type is invariant
per batch. Resolve each column to a typed accessor once per batch (keep all six numeric
variants — they exist to read foreign parquet files). *Impact: low. Complexity: medium.
Teaching: high (loop unswitching).*

### C5. Serialize JSONL rows straight into the writer
[csv_utils.rs:59](crates/fnirsi-protocol/src/csv_utils.rs:59) — `serde_json::to_string`
allocates a `String` per row that is immediately copied into the `BufWriter`.
`serde_json::to_writer(&mut wtr, s)?` then `wtr.write_all(b"\n")?` — one transient
allocation per row removed. *Impact: low. Complexity: trivial.*

### C6. Delete the dead `index` field
[plots.rs:27](crates/fnirsi-gui/src/plots.rs:27) — written on every push, reset in clear,
**never read** (`generation` is the counter actually consumed). Not a perf change — the
verifier reframed it as hygiene: misleadingly-named dead state. *Trivial.*

## Promising but not yet adversarially verified

- **Bucket-center min/max decimation (branchless).** Buckets are ~¼ pixel wide
  (`max_points = width × 8`, two points per bucket), so emitting both extremes at the
  bucket-center timestamp is visually identical to tracking argmin/argmax — and dropping
  the index tracking turns the inner loop into plain `min`/`max` folds (branchless
  `minss`/`maxss`; `f32::min/max` also skip NaN handling branches — an all-NaN bucket
  simply leaves `INFINITY` behind, which is the existing sentinel test). The verifier that
  refuted the `as_slices` idea independently pointed at exactly this as "the correct
  target". Pairs naturally with A2. *Needs a visual diff check.*
- **`partition_point` instead of `binary_search_by_key`**
  ([plots.rs:490](crates/fnirsi-gui/src/plots.rs:490), :495, :711, :735). On duplicate
  timestamps `binary_search`'s returned index is unspecified; `partition_point` has clean
  semantics and compiles to the cmov-style branchless probe from the branchless-binary-search
  article. Semantics cleanup first, perf second.
- **`[profile.release]`: `lto = "thin"`, `codegen-units = 1`.** There is no release tuning
  at all today; this enables cross-crate inlining (protocol decode into the GUI reader loop)
  and better code layout for a pure build-file change. Perf-ninja lists LTO/PGO as its
  closing labs. Cost: slower release builds — measure both.
- **CLI stdout: lock once + `BufWriter`** ([main.rs:379-397](crates/fnirsi-cli/src/main.rs:379)) —
  `println!` per sample at 100 Hz re-locks stdout per call and, on a tty, flushes per line.
  Syscalls are mechanical sympathy too.
- **Incremental decimation cache** — the only idea that changes the *algorithm*: steady-state
  appends only touch the tail bucket, so cache decimated points keyed on
  (generation, zoom range) and recompute only the tail. Turns the O(n)-per-frame walk into
  O(new samples). Biggest possible win at large buffers; medium-large complexity and real
  invalidation risk (eviction shifts indices). Only worth it if the Tier A items prove
  insufficient at the 100 MB–1 GB presets.

## Refuted — and why that's the interesting part

Each of these looked plausible and was killed by verification. The lesson each teaches is
worth more than the change would have been:

- **Full AoS→SoA for the plot buffers.** The bandwidth argument is real, but the
  autovectorization claim dies on `VecDeque` ring indexing, and the export path
  (`samples() -> &VecDeque<Sample>`) forces a 7-deque zip/reconstruction ripple. Large
  complexity, self-rated low impact. *Lesson: the hot/cold split (A3) captures most of the
  win at a fraction of the cost — know where to stop.*
- **`as_slices()` instead of indexed `VecDeque` scanning.** The mechanism was simply wrong:
  a sequential scan of a ring buffer is two ascending unit-stride runs — exactly what the
  stream prefetcher is built for — and the wrap branch predicts perfectly (one miss per
  scan). `as_slices` yields the *same* two physical regions. *Lesson: a "hidden branch" is
  only a cost if the predictor can't learn it; prefetchers watch address streams, not your
  types.*
- **`array::from_fn` instead of zero-init-then-overwrite** (packet decode). Compiled both:
  byte-identical assembly — LLVM's dead-store elimination already removes the zeroing.
  Worth doing only as a readability cleanup, never as a perf claim. *Lesson: the compiler
  got there first — check the assembly before claiming a win.*
- **Unswitching the `paused` check out of the ingest loop.** A loop-invariant bool branch is
  perfectly predicted — effectively free. *Lesson: count mispredictions, not branches.*
- **Single-pass Parquet column transpose.** Nine per-column passes are nine
  one-read-one-write streaming loops; fusing them creates one read stream fanning out to
  nine concurrent write streams — not obviously better, on a cold path. *Lesson: count
  write streams too; streaming-friendly ≠ fewest passes.*
- **Coalescing BLE samples into batched channel messages.** The BLE producer only ever
  delivers one sample per notification — there is no burst to coalesce. *Lesson: know the
  actual arrival shape before batching.*

## Already sympathetic — don't regress these

Fixed `[u8; 64]` packets on the stack end-to-end; `[Sample; 4]` decode with no heap;
`request_repaint_after` throttling (50 ms live / 200 ms paused); min/max LOD decimation
capped at points-per-pixel; cached `latest_*` values instead of O(n) reverse scans;
the `energy_wh`/`capacity_mah` side-buffers are already SoA; table-driven CRC-8 (256 B
table, 4 cache lines); `BufWriter` on JSONL; pre-sized `Vec::with_capacity` in the CFN
reader.
