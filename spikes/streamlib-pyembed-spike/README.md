<!--
Copyright (c) 2025 Jonathan Fontanez
SPDX-License-Identifier: BUSL-1.1
-->

# streamlib-pyembed-spike — #1702

Throwaway spike measuring in-process Python (CPython embedded via PyO3) against today's
subprocess-per-Python-processor model. **Nothing here is an API proposal.** The callback token
registry, the process-global measurement collection point, and the Python-facing shapes are
deliberate spike artifacts; #1702 defers API design until after the numbers exist.

The engine is untouched. Every processor enters through the existing `App::add_local` path.

## Not a workspace member

This crate self-roots its own `[workspace]` table, the same convention `examples/*` and most
`packages/*` follow (root `Cargo.toml:44-56`). It builds with `cd spikes/streamlib-pyembed-spike
&& cargo build` and never enters the engine's release closure or CI gate surface. Cold dependency
build measured at **63s** on the reference machine.

Its `Cargo.lock` is **not committed** — the repo ignores every non-root lockfile
(`.gitignore:51-52`) and this crate follows that convention rather than overriding it. The cost is
real for a benchmark: `pyo3`/`numpy`/`hdrhistogram` are caret ranges, so a rerun can resolve
different patch versions than the run that produced a given number. Until that is addressed, the
resolved versions of a run are recoverable only from the machine that produced it. Raised as a note
for the owner rather than decided here.

## What is measured, and what is not

The metric is **`source_emit_to_sink_receive`**, stamped raw `CLOCK_MONOTONIC` on both sides.

It is **not** capture-to-present. Tier A has no camera and no display, and present time is not
observable in *either* tier without an engine change this spike forbids: the only present-path
instrumentation is `#[tracing::instrument(level = "trace")]` on `VulkanPresentTarget::end_frame`
(`vulkan_present_target.rs:630`), and `Cargo.toml:93` pins tracing with `release_max_level_debug`,
compiling trace out of every release build. `ProcessorMetrics` is declared and read but written by
nothing in `runtime/ sdk/ tools/`. Owner decision on #1702: report the observable quantity under
its own name rather than an unmeasured one under the frozen name.

## The three arms

| `--arm` | What it isolates |
|---|---|
| `rust-passthrough-floor` | The engine's own wire-hop cost, no interpreter. Run this first at every cell. |
| `in-process-python` | The same graph with a Python callable on the processor thread. |
| subprocess baseline | Today's model. **Not in this PR** — Tier B, see #1702. |

The floor arm exists because gate 5 does not discriminate: PyO3 callback overhead passes its
0.5ms budget by ~39x naive and ~3300x anchored, while the engine's `read_raw` allocates a fresh
64 KiB `Vec`, then a fresh full-size `Vec`, then memcpys, **per read**
(`plugin-abi/src/vtables/input_mailboxes.rs:156-157`, allocation inside the retry loop). Without a
floor arm a large absolute latency is unattributable.

Smoke numbers from this branch (10s cells, not protocol runs — see "Status" below):

```
720p30  floor  emit->sink p50 9.63ms   p99 11.09ms
720p30  python emit->sink p50 9.61ms   p99 11.08ms   stage callback p50 13.8µs
1080p30 python emit->sink p50 27.16ms  p99 29.38ms   stage callback p50 4.18ms (realistic)
```

The Python arm is indistinguishable from the pure-Rust floor at the same geometry. The wire hop
dominates by three orders of magnitude over the PyO3 callback.

**The floor scales with payload, and that bounds the protocol.** Floor-arm p50 is 1.44ms at
640x480, 9.63ms at 720p, ~27ms at 1080p — the engine's `read_raw` allocates a fresh 64 KiB `Vec`,
then a fresh full-size `Vec`, then memcpys, per read per hop. At 1080p the two-hop service time
exceeds the 60fps frame period (16.6ms), so a 1080p60 cell runs saturated and its percentiles would
describe queue occupancy rather than latency. Posted to #1702; 1080p is a 30fps-only geometry until
the owner rules.

## The GIL attachment anchor

`Python::attach` on a foreign thread maps to `PyGILState_Ensure()`, which builds a thread state on
entry and destroys it on exit. CPython 3.12 virtual-allocates the frame datastack chunk on first
Python frame push and frees it on thread-state delete — one `mmap` + one `munmap` per frame
(measured: 1041 mmap / 1005 munmap per 1000 calls). The anchor holds one unreleased
`PyGILState_Ensure` parked with `PyEval_SaveThread`, dropping per-call cost from p50 6.3µs to
p50 110ns. Public `pyo3::ffi` only.

Whether the real SDK should anchor processor threads is a pivot design question these numbers do
not settle: anchoring trades a resident thread state per processor thread for the syscall pair.
`--disable-gil-anchor` is the control condition.

## Delivery profile

Every input port declares `delivery_profile = "every_sample"`. Owner decision on #1702: latency
percentiles are the primary signal; drop counts are reported, not gated. Under `latest`
(SkipToLatest) the sink drains to the newest sample, pinning latency near one frame period and
making the percentile gates near-vacuous. `delivery_profile` is a compile error on an `output(...)`
(`grammar.rs:432-448`) — it is consumer-side only.

## Running a cell

```
cargo build --release
PYTHONPATH=python ./target/release/tier_a_harness \
  --arm in-process-python --fps 30 --frame-width 1920 --frame-height 1080 \
  --duration-seconds 600 --warmup-exclusion-seconds 60 \
  --stage-callback-module spike_stage_callbacks \
  --stage-callback-attribute realistic_stage \
  --output-directory ./artifacts
```

`python/runner.py` invokes the harness once per cell; one cell per process keeps interpreter, GC,
and allocator state from leaking between cells.

Add `--require-locked-measurement-state` for gated cells. The harness **verifies** the machine is
locked and fails fast; it never invokes `sudo` (`sudo -n` fails on the reference box and a password
prompt would wedge an unattended multi-hour run). `machine_specification_probe` emits the exact
owner checklist of privileged commands as data.

## Artifact directory, per cell

| File | Contents |
|---|---|
| `cell-spec.json` | Every resolved parameter, including the delivery profile and metric name. The summarizer refuses to evaluate a gate the recorded configuration cannot support. |
| `machine-spec.json` | CPU/governor/boost, kernel + preemption, glibc, GPU + driver, Python/numpy, scheduling class, loadavg. Unknown knobs carry an explicit reason, never a silent default. |
| `per-frame-measurements.jsonl` | One object per frame, warmup-excluded frames included (raw data is raw). |
| `source-emit-to-sink-receive.histogram` | Mergeable HDR histogram export. |
| `summary.json` | p50/p99/p99.9/max, drop count, and the anomaly counters. Never a headline mean — the distribution is heavily tailed. |
| `gc-collections-embedded-interpreter.jsonl` | Every CPython collection in the interpreter that ran the callback, monotonic-stamped so a latency tail spike can be attributed to a GC. In-process arm only. |

An empty GC record file is a real result, not a broken recorder: the per-frame numpy view is
released as soon as the callback returns, so gen0's tracked-container count does not climb and no
generational pass is triggered during a short cell. The recorder is asserted functional
independently (`python/test_spike_harness_contract.py` forces collections and asserts a nonzero
count).

Two counters invalidate a cell rather than degrading it, and the harness logs both at `error`:
`negative_latency_anomaly_count` (sink stamp before emit stamp ⇒ the arms' clocks disagree) and
`histogram_range_saturation_count` (percentiles are clipped).

## Which arrangement Tier A measures

A Rust `main` that calls `Python::initialize()` and embeds CPython — **not** the `PyApp`
`#[pyclass]` that CPython imports, which is what #1702's design sketches. The crate is
`crate-type = ["rlib"]` only; there is no `#[pymodule]`.

The per-frame path is identical either way (the processor thread is a foreign thread to CPython in
both arrangements), so `source_emit_to_sink_receive` transfers. What does NOT transfer is main-thread
ownership, SIGINT ownership, and interpreter-init ordering against `GpuContext` — which is exactly
what the warm-restart battery measures. Posted to #1702 as an owner decision.

## Status

**This PR delivers the Tier A harness and proves it runs end-to-end. It does not deliver the
protocol numbers.** No 10-minute cell, no A/B/A matrix, no soak, no GC-tuned cells. Those and the
subprocess baseline are Tier B (PR 2), and two protocol questions remain open with the owner:
SCHED_OTHER vs SCHED_FIFO for the primary numbers, and whether to add an absolute p99.9 ceiling
(tail risk sits beyond p99 — measured 611µs p99.9 and 12.8ms max under GIL contention, where only
a relative-to-baseline delta is gated today).

Payload is capped at 1080p: subprocess links are `UntrustedSession` (16 MiB ceiling), in-process
host-to-host links are `Trusted` (64 MiB). 4K BGRA fits the in-process arm and is refused on the
subprocess arm, so the two are structurally incomparable above 16 MiB.
