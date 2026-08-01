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
| `subprocess-python-baseline` | Today's model: same callback, own process, own venv, through the python-native cdylib. The arm the pivot is measured against. |

The floor arm exists because gate 5 does not discriminate: PyO3 callback overhead passes its
0.5ms budget by ~39x naive and ~3300x anchored, while the engine's `read_raw` allocates a fresh
64 KiB `Vec`, then a fresh full-size `Vec`, then memcpys, **per read**
(`plugin-abi/src/vtables/input_mailboxes.rs:156-157`, allocation inside the retry loop). Without a
floor arm a large absolute latency is unattributable.

Numbers from this branch (720p60, passthrough, 30s cells, surface references — not protocol runs;
see "Status"):

```
arm                          p50       p99     p99.9       max
rust-passthrough-floor   0.071ms   0.107ms   0.120ms   0.124ms
in-process-python        0.089ms   0.120ms   0.133ms   0.144ms
subprocess-python-baseline 0.180ms 0.238ms   0.265ms   1.141ms
```

In-process beats the baseline 2.0x at p50 and 2.0x at p99. PyO3 costs ~18µs at p50 over the
pure-Rust floor.

## What crosses the wire

`--wire-payload-mode surface-reference` (the default) puts a fixed-width surface reference on the
link, matching what a real `@tatolab/core/VideoFrame` weighs — it references its GPU surface by id
(`packages/core/schemas/video_frame.yaml`). The pixels the callback views are resolved
process-locally, standing in for `GpuContext::resolve_pixel_buffer_by_surface_id`
(`gpu_context.rs:681`), which an in-process processor can already reach with no engine change.

`--wire-payload-mode full-pixel-payload` pushes whole uncompressed pictures instead. It exists only
to reproduce the payload sweep that retracted an earlier ~27ms 1080p "floor", and the summarizer
refuses to let such a cell back any gate. At 1080p60 the two modes differ by 175x (0.091ms vs
15.917ms) — the earlier figure was the harness's transport choice, not an engine property.

The reference body is padded to a fixed 192 bytes on purpose: a bare encoding varies with the
decimal digit count of the geometry it describes, which would make the resolution leg of the matrix
vary transport width as well as pixel work.

## Startup settle

`--startup-settle-seconds` (default 2.0) is a quiet period before the source's first frame, held
identical across arms. `Runner::start()` returns once the graph compiles, but a Python subprocess
needs ~0.66s more to spawn, import, and reach its poll loop; frames emitted into that window queue
up, and under `every_sample` the backlog is never dropped. It drains only as fast as the consumer
runs ahead of the source, so it outlives the warmup exclusion, which excludes by *time*.

Before the settle existed, the baseline arm reported p50 183ms over a 20s cell and 84ms over a 40s
cell against 0.089ms in-process. All of it was startup transient. The recorder now also compares its
first measured decile against its last and reports `backlog_drain_fraction`; a cell that shed a
fifth of its latency across its own life is refused rather than reported.

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

`python/runner.py` invokes the harness once per cell over an A/B/A schedule; one cell per process
keeps interpreter, GC, and allocator state from leaking between cells.

### The subprocess baseline arm needs provisioning first

```
python3 python/provision_subprocess_baseline_package.py
```

Idempotent, and required before any `--mode subprocess` cell. Three prerequisites the harness
cannot satisfy from inside a measurement run, each of which fails in a way that reads as a slow or
absent baseline rather than a broken one:

- `libstreamlib_python_native.so` is absent from `target/`. It is built and pinned by absolute path
  via `STREAMLIB_PYTHON_NATIVE_LIB` so a stale or foreign artifact cannot win — an unpinned
  subprocess resolves a different cdylib whose iceoryx2 service constants disagree with the host's,
  fails to open its own input channel with `DoesNotSupportRequestedAmountOfPublishers`, and the cell
  reports an arm that produced no frames.
- The package's `streamlib` dependency resolves from no index. A `[tool.uv.sources]` path override
  at this checkout's Python SDK is injected into a staged copy — the same rewrite
  `streamlib link --engine` performs (`python_venv.rs:360-393`), scoped to the package so a
  measurement run neither depends on nor disturbs global link state.
- `streamlib install` skips linked entries by design and the module loader refuses to cold-build an
  installed slot, so nothing in between provisions a linked Python package's venv.

`STREAMLIB_MODULES_DIR` points the module slot, the link, and the lock file at the spike crate, so
provisioning writes nothing outside `spikes/`.

### Warm-restart battery (gate 6)

```
PYTHONPATH=python python3 python/warm_restart_battery.py \
  --harness-binary ./target/release/tier_a_harness \
  --warm-run-count 10 --extra-import torch --output-dir ./artifacts/restart
```

1 cold + N warm restarts, each a fresh process, measuring exec-to-first-frame from a pre-spawn
`CLOCK_MONOTONIC` stamp to the sink's first-frame stamp. On this branch: warm median **0.869s**
against a 1.5s threshold, cold 0.919s.

### Evaluating the gates

```
python3 python/summarize_measurement_matrix.py ./artifacts
```

Built around one rule: never report a verdict a cell cannot support, because a silently skipped gate
reads as a passed one. Cells are refused — loudly, with the reason — when they were built with
stamping compiled out, carry the full-pixel payload, resolved a delivery profile other than
`every_sample`, received no frames, disagreed on clocks, saturated the histogram, or drained a
startup backlog.

Two gates stay NOT EVALUATED until the owner states a number. Owner decision 3 made the
floor-vs-PyO3 delta the gate and decision 4 added an absolute p99.9 ceiling; neither names a
threshold, and #1702 states thresholds are evaluated verbatim and never decided inline. Both
quantities are computed and printed; `--floor-delta-gate-ms` and `--absolute-p99-9-ceiling-ms` turn
them into verdicts.

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
| `summary.json` | p50/p99/p99.9/max, drop count, the anomaly counters, `backlog_drain_fraction`, and the sink's first-frame stamp (gate 6's endpoint). Never a headline mean — the distribution is heavily tailed. |
| `gc-collections-embedded-interpreter.jsonl` | Every CPython collection in the interpreter that ran the callback, monotonic-stamped so a latency tail spike can be attributed to a GC. In-process arm only. |

An empty GC record file is a real result, not a broken recorder: the per-frame numpy view is
released as soon as the callback returns, so gen0's tracked-container count does not climb and no
generational pass is triggered during a short cell. The recorder is asserted functional
independently (`python/test_spike_harness_contract.py` forces collections and asserts a nonzero
count).

Three signals invalidate a cell rather than degrading it, and the harness logs each at `error`:
`negative_latency_anomaly_count` (sink stamp before emit stamp ⇒ the arms' clocks disagree),
`histogram_range_saturation_count` (percentiles are clipped), and `backlog_drain_fraction` past
0.20 (the cell was draining a startup queue, so its percentiles describe occupancy and vary with
how long it ran).

## Which arrangement Tier A measures

A Rust `main` that calls `Python::initialize()` and embeds CPython — **not** the `PyApp`
`#[pyclass]` that CPython imports, which is what #1702's design sketches. The crate is
`crate-type = ["rlib"]` only; there is no `#[pymodule]`.

The per-frame path is identical either way (the processor thread is a foreign thread to CPython in
both arrangements), so `source_emit_to_sink_receive` transfers. What does NOT transfer is main-thread
ownership, SIGINT ownership, and interpreter-init ordering against `GpuContext` — which is exactly
what the warm-restart battery measures. Posted to #1702 as an owner decision.

## Status

**Delivered here: all three arms, the warm-restart battery, and the gate evaluator. Not delivered:
the protocol matrix or the verdict.** No 10-minute cells, no soak, no GC-tuned cells, no Tier B —
those are the next PR in the stack.

Gate 6 is measured and passes (0.869s warm median against 1.5s). Gates 1, 2, 4 and 5 pass on
exploratory 720p60 cells. Gates 3 and the floor-delta gate cannot be evaluated at all until the
owner states their thresholds.

Payload is capped at 1080p: subprocess links are `UntrustedSession` (16 MiB ceiling), in-process
host-to-host links are `Trusted` (64 MiB). 4K BGRA fits the in-process arm and is refused on the
subprocess arm, so the two are structurally incomparable above 16 MiB — though under
`surface-reference` the wire body no longer tracks geometry, so the cap binds only the full-pixel
sweep.
