# Mechanical-sympathy experiment results

Every non-refuted idea from [mechanical-sympathy-ideas.md](mechanical-sympathy-ideas.md)
was implemented in its own isolated git worktree (uncommitted — nothing merged), then
independently adversarially reviewed: diff read in full, tests and clippy re-run without
trusting the implementer, edge cases hunted. Worktrees live at
`.claude/worktrees/wf_8b1027a9-7c2-{1..17}` under the main repo; each holds its diff as
uncommitted working-tree changes.

**Scoreboard: 17/17 implemented — 12 adopt, 3 adopt-with-fixes, 1 rejected (real deadlock
found in review), 1 build-config experiment with mixed evidence.**

| # | Idea | Worktree | Review | Measured result |
|---|---|---|---|---|
| U5 | Incremental decimation pyramid | `-17` | with-fixes | **25–49× faster** full-range decimation @1M samples (reviewer reproduced 42.7×); +0.78 B/sample |
| U1 | Bucket-center branchless min/max | `-13` | yes | **~1.5–2.5×** on the decimation loop (reviewer: 1.45–2.5×, noisy but consistent) |
| A2 | Fuse V/D+/D− walks | `-2` | yes | **12–18%** faster fused vs 3 passes; 1 binary-search pair instead of 3 |
| A1 | Kill per-frame `Vec<f64>` collect | `-1` | yes | removes ~1.6 MB alloc/frame/plot; ~9–19% of per-plot cost |
| C3 | XLSX resolve columns once | `-9` | yes | 12.5–14× on lookup step; **1.65–2× end-to-end `read_xlsx`** |
| C4 | Parquet hoist type dispatch | `-10` | yes | ~4× on dispatch (micro); end-to-end I/O-bound |
| C1 | Reciprocal multiplies | `-7` | yes | ASM-verified: 21 `fdiv` → 0, bonus SIMD `fmul.4s` over the 4 samples |
| A3 | Hot/cold `PlotSample` split | `-3` | yes | 40→32 B scan stride (−20% bytes/scan); memory-neutral; size pinned by const assert |
| B1 | Reserve before import | `-4` | yes | no bench (cold path); one alloc per deque instead of ~log n doublings |
| B2 | Countdown decimator | `-5` | yes | unmeasurable at 100 Hz; div removed; resync nuance (below) |
| B3 | Channel message variants | `-6` | with-fixes | enum 104→168 B, but zero allocs on both reader paths (was 1 per send) |
| C2 | Integer bucket boundaries | `-8` | yes | frame-time-invisible; boundary math now exact (proofs in tests) |
| C5 | JSONL `to_writer` | `-11` | yes | one String alloc per row removed (protocol + CLI) |
| C6 | Delete dead `index` field | `-12` | yes | hygiene; −5 lines |
| U2 | `partition_point` | `-14` | with-fixes | determinism fix, not speed; one real boundary-semantics change (below) |
| U3 | `[profile.release]` thin LTO + cgu=1 | `-15` | yes | binary-size win solid; build-time "not slower" claim rests on n=2 runs |
| U4 | CLI stdout lock+buffer | `-16` | **no — broken** | review found a deterministic deadlock (below) |

## Headline findings

**The pyramid wins by doing less, not by touching faster.** U5 (one level of 256-sample
min/max summaries, maintained incrementally on push, evicted whole-bucket from the front,
raw-walk fallback when zoomed) turns the O(n)-per-frame decimation walk into O(n/256) —
measured 25–49× at 1M samples for 0.78 bytes/sample overhead. Every other plot-path idea
shaves constants off the walk; U5 removes the walk. If only one plot-path change ever
lands, it should be this one (after its scope-creep cleanup).

**U1's speedup has the "wrong" mechanism — and that's the lesson.** The old min/max loop
was *already branchless* (LLVM emits `fcsel`); the measured ~2× came from dropping
argmin/argmax index tracking, which freed LLVM to unroll with two independent accumulator
pairs — an **ILP win, not a branch win**. The ideas doc predicted "branchless + fewer
mispredicts"; the assembly said "shorter dependency chain". Prediction wrong, direction
right, only the disassembly could tell.

**U4 is the cautionary tale.** The implementation was clean, tested, clippy-green — and
the adversarial reviewer found a deterministic deadlock the implementer's same-thread
reentrancy analysis missed: `LogOutput` holds a session-long `StdoutLock` on the block_on
thread while `handle.await` waits for the BLE task, which logs to stdout via `tracing`
(default writer) from another thread → `fnirsi-cli log --ble` to stdout hangs on shutdown
and needs SIGKILL. Fix direction if revisited: route `tracing` to stderr (arguably correct
anyway), or scope the lock so it's dropped before the await. Until then: rejected.

## Fixes owed before any adoption

- **B3 (`-6`) and U5 (`-17`) scope creep**: both bundle baseline clippy fixes — including
  `+= a*b` → `mul_add` rewrites that *change numeric results* (single fused rounding) in
  energy/capacity accumulation. Strip those hunks; land them (if at all) as a separate,
  deliberate change. (`mul_add` without hardware FMA can also be *slower* — untested.)
- **U2 (`-14`)**: the new `visible_range` end bound genuinely differs from the old code on
  an *exact* `max_ms` match (off by one, now deterministic; old was unspecified only for
  duplicates). Decision needed: accept the new deterministic semantics (reasonable) and
  pin it with a test for the unique-exact-match case, which is currently untested.
- **B2 (`-5`)**: rate-change resync now happens within one *old*-divider period (was: one
  *new*-divider period) — switching 1 Hz → 100 Hz can delay the first kept sample ~1 s.
  Acceptable, but document or reset the counter on preset change.
- **U3 (`-15`)**: keep the (deterministic, bit-identical) binary-size result; re-run the
  build-time comparison ≥5× on a quiet machine before believing "never slower". Note its
  discovered measurement trap: reverting Cargo.toml can silently relink from a cached
  fingerprint (0.3 s "build") — always verify a profile change actually recompiled.

## Composition plan (when the experiment graduates)

Nine ideas touch `plots.rs`; they cannot merge blindly. Suggested order, each rebased on
the last:

1. **Independent, small, anywhere**: C6, B1, C1, C5, C3, C4, C2, B2, U2(+fix), B3(−scope
   creep), U3 (build file). These conflict with nothing structural.
2. **A1** (points_from_deque) — establishes the deque-direct pattern.
3. **A3** (PlotSample split) — changes the element type under everything that follows.
4. **A2 + U1 together** — fusion and bucket-center *simplify each other*: bucket-center
   deletes the per-lane argmin/argmax index arrays that make N-lane fusion state-heavy.
5. **U5 last** — the summary pyramid sits in front of whatever raw walk survives 2–4, and
   its aggregation should reuse U1's fold-style min/max.

A conservative alternative: land tier 1 + A1 + A3 only, and take U5 *instead of* A2+U1
(the pyramid makes the raw walk cold, so optimizing it further buys little).

## Operational lessons (for the skill / future runs)

- **A shared `CARGO_TARGET_DIR` across parallel worktrees is unsound for verification**:
  same crate name + version ⇒ identical `-C metadata` unit hashes ⇒ worktrees overwrite
  each other's test binaries and clippy caches. Several agents observed *another
  experiment's tests* running, or stale clippy lines. Every agent that noticed re-verified
  in a private target dir; treat shared-cache test/clippy output as untrusted. Build-cache
  sharing: fine. Verification: isolate.
- **The "warning-free tree" premise was false**: clippy 1.96 introduced ~9 warnings on
  untouched HEAD (`suboptimal_flops`, `explicit_counter_loop`, `manual_midpoint`).
  Baseline your lint state before telling agents to keep it clean, or they'll either
  false-fail or "helpfully" fix out-of-scope code (two did).
- **Adversarial review earns its cost**: 1 of 17 clean-looking implementations was broken
  in a way only cross-thread reasoning caught, and several measured claims were revised
  down (A1's 19% → ~10% on the review machine; U1's "stable 2.3×" → "1.45–2.5×, noisy").

## A/B benchmark: combined branch vs master

All 16 adoptable ideas were integrated into one branch (15 commits; U4 excluded as
broken; the out-of-scope `mul_add` hunks stripped; B2/U2 fixes applied during
integration). Measured with the in-tree `#[ignore]` A/B benches (identical bench code
compiled against both sides, release profile as-shipped — the branch includes its thin-LTO
profile change), Apple Silicon, quiet machine, 2 alternating rounds, `--test-threads=1`:

| Bench | master | branch | Δ |
|---|---|---|---|
| Whole-frame render, 7 lines, 1M samples (headless egui) | 13.7 / 14.4 ms/frame | 2.85 / 2.79 ms/frame | **~4.9× faster** |
| Whole-frame render, 200k samples | 2.96 / 2.97 ms/frame | 1.47 / 1.47 ms/frame | **~2.0× faster** |
| Packet decode (1M iters, CRC on) | 61–63 ns/packet | 62–78 ns/packet | parity (CRC-dominated; within noise) |
| XLSX import, 50k rows | 124–154 ms | 118–121 ms | ~5% (calamine parse dominates) |
| `fnirsi-cli` binary | 5.43 MB | 4.54 MB | −16% |
| `fnirsi-gui` binary | 10.20 MB | 8.90 MB | −13% |

The frame-render wins are end-to-end (including egui layout/tessellation overhead the
branch doesn't touch); the decimation-only speedups underneath are larger (integration
agents measured 3.1× for the bucket-center fold and ~40× for the summary-pyramid walk in
isolation). The decode row is the honesty check: the reciprocal-multiply change is real at
the instruction level (fdiv → fmul, ASM-verified) but invisible at wall-clock packet
rates — exactly as predicted in the ideas doc.
