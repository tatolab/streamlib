# Hardware-integration test tier

streamlib's test suite is split into two tiers, with the boundary
enforced by a Cargo feature so the split can't drift:

| Tier | Triggered by | What it covers | Parallel-safe? |
|---|---|---|---|
| **1 — Unit** | `cargo test` (default) | Pure logic, parsers, state machines, serialization round-trips, mock-backed integration. | Yes — by construction. |
| **2 — Hardware integration** | `cargo test --features streamlib/hardware-tests,streamlib-media-builtins/hardware-tests` | Tests that construct a real `HostVulkanDevice`, allocate GPU memory, exercise the swapchain, etc. | No — must run with `--test-threads=1`. |

Tier 1 is parallel-safe by construction — no test inside the tier-1 set
is allowed to require a GPU device or any other exclusive system
resource. Tier 1 should always pass cleanly in parallel; if it doesn't,
the offending test is mis-classified.

The **CI** tier-1 gate is the minimal per-crate `--lib` run defined by
`.github/workflows/test.yml` (the CI config is the source of truth for
what CI enforces). The broader **local** tier-1 baseline is the whole
workspace:

```bash
cargo test --workspace
```

Every binary and every `Doc-tests` block should print `test result: ok.`
with zero failures — that, not any particular total, is the pass bar.

Tier 2 is the gate that runs when a change is hardware-relevant — Vulkan
RHI work, encoder/decoder, display, anything in `vulkan/rhi/`. The
canonical command:

```bash
cargo test \
    --features streamlib/hardware-tests,streamlib-media-builtins/hardware-tests \
    --workspace --no-fail-fast \
    -- --test-threads=1
```

Both features are named because a `pkg/feature` flag enables that package's
feature and nothing else: `streamlib/hardware-tests` forwards to
`streamlib-engine`, but it does **not** reach
`streamlib-media-builtins/hardware-tests`. A crate left off this line does not
fail the sweep — its tier-2 tests report as `ignored`, which reads exactly like
having none. Any crate that declares its own `hardware-tests` feature belongs
here on the same day it declares it.

The `--test-threads=1` is mandatory: tier-2 tests serialize on the GPU
device. Running them in parallel deadlocks (most often inside the
NVIDIA Vulkan driver's per-process kernel state, see
[`docs/learnings/nvidia-dma-buf-after-swapchain.md`](learnings/nvidia-dma-buf-after-swapchain.md)).

## Vulkan validation over a tier-2 run

Tier 2 is the only tier that constructs a real Vulkan device, so it is the
only place the Khronos validation layer has anything to say. Three env vars
drive it. Each is independent — setting one never turns on another's
behaviour; all it implies is the layer they have in common. Every one of them
is a no-op where that layer is not installed (a warning, never a failure,
which is why CI is unaffected):

| Env var | Effect |
|---|---|
| `STREAMLIB_VULKAN_VALIDATION=1` | Load the layer, forward `ERROR` and `WARNING` findings into `tracing`, count them per device. |
| `STREAMLIB_VULKAN_SYNC_VALIDATION=1` | Load the layer and add synchronization validation. |
| `STREAMLIB_VULKAN_VALIDATION_ABORT_ON_ERROR=1` | Load the layer, and let the first error kill the process, naming its VUID. |

In particular the whole-sweep gate below sets only the third, so it runs
*without* synchronization validation; combine the second and third to gate on
both.

Registering a messenger silences the layer's own stdout printing, so with a
plain `STREAMLIB_VULKAN_VALIDATION=1` run a finding reaches a `cargo test`
binary — which installs no `tracing` subscriber — only where a test reads
`HostVulkanDevice::validation_layer_message_counts()`. Abort-on-error is
therefore the whole-sweep gate:

```bash
STREAMLIB_VULKAN_VALIDATION_ABORT_ON_ERROR=1 cargo test \
    --features streamlib/hardware-tests,streamlib-media-builtins/hardware-tests \
    --workspace --no-fail-fast \
    -- --test-threads=1
```

A binary that dies with `SIGABRT` raised a validation error; the panic
message immediately above it names the VUID and quotes the spec. That sweep
runs clean, and is the standing rig gate for hardware-relevant work: a change
that reddens it is a regression to fix, not a new baseline to record.

Warning-severity findings never trip abort mode. One is known and accepted:
`vulkan_graphics_kernel::tests::constructs_kernel_with_vertex_input_buffers`
declares vertex input attributes at locations 1 and 2 that the blit shader
never reads, which the layer reports as `WARNING-Shader-OutputNotConsumed`.
Unused vertex input declarations are spec-legal, and that a pipeline can be
created with them is exactly what the test locks.

Abort-on-error is also the one mode in which
`a_deliberately_invalid_vulkan_call_moves_the_validation_error_count` skips,
since it raises a finding on purpose. That test is the only thing standing
between a green sweep and a sweep that is green because the layer went
silent, so run it alongside:

```bash
STREAMLIB_VULKAN_VALIDATION=1 cargo test \
    --features streamlib/hardware-tests -p streamlib-engine --lib \
    vulkan_validation_messenger -- --test-threads=1 --nocapture
```

`--nocapture` is what makes that check meaningful: both hardware tests skip
by returning early, and libtest swallows a passing test's output, so without
it a skipped run and a real one print the same `ok`. With it, a run that
proves nothing says `Skipping` and why.

A test that wants to hold one GPU path at zero reads the counter around it
rather than relying on the sweep:

```rust
let before = device.validation_layer_message_counts();
// ... the path under test ...
assert_eq!(device.validation_layer_message_counts(), before);
```

`None` means no messenger is installed — validation off, or layer absent. It
is never the same as zero, and a test must skip rather than pass on it.

Leave validation off when reproducing a driver-race symptom: it shifts
timing.

## Why Cargo features instead of `#[ignore]`

The structural defense is `#[cfg_attr(not(feature = "hardware-tests"),
ignore = "...")]`, not plain `#[ignore]`. The reasoning:

- A plain `#[ignore]` is a single-purpose mute switch. It can drift
  from "this test belongs to a different tier" to "this test is flaky
  so I muted it" without anyone noticing — exactly the failure mode
  the tier separation exists to prevent.
- A feature-gated ignore is a structural commitment: the test is
  ignored *only* in tier 1, and runs unconditionally in tier 2. The
  feature flag makes the tier intent explicit at the call site.
- Future agents reading the code see "if the `hardware-tests` feature
  is on, this test runs" rather than just "ignored." That conveys
  intent, not a band-aid.

If a hardware test is flaky, the right answer is to fix it, not to add
a plain `#[ignore]` next to its `#[cfg_attr]` line.

## What goes in tier 2

A test belongs in tier 2 if its body, or any helper it transitively
calls, constructs a real GPU device or otherwise depends on a
system-exclusive resource. Concretely, today:

- Anything calling `HostVulkanDevice::new()` directly or through a
  helper like `try_vulkan_device()`, `setup_device()`,
  `create_test_device()`.
- Tests in `vulkan/rhi/` that exercise GPU memory, swapchains,
  pipelines, sync primitives.
- Audio device probes: anything opening a stream through the audio
  device seam against a real backend, which needs an audio device rather
  than a GPU — a reachable session for the PipeWire arm, `/dev/snd` and an
  openable capture PCM for the ALSA arm.
- Future: V4L2 camera capture, display swapchains, anything that holds a
  kernel-level exclusive lock.

Pure-logic tests in the same file (e.g. cache-path string formatting,
SPIR-V reflection validators that operate on byte arrays without ever
constructing a device) stay in tier 1.

## Adding a new hardware test

1. Place the test next to its production code (`vulkan/rhi/foo.rs::tests`).
2. Tag it with `#[cfg_attr(not(feature = "hardware-tests"), ignore =
   "hardware integration — set --features streamlib/hardware-tests +
   run with --test-threads=1. See docs/testing-hardware.md")]`
   immediately above `#[test]`. Name the feature flag that actually reaches
   the crate the test lives in: `streamlib/hardware-tests` forwards to
   `streamlib-engine` only, so a test in any other crate names its own
   (`streamlib-media-builtins/hardware-tests`) — and that crate joins the
   sweep line above in the same PR.
3. Use a shared `try_vulkan_device()` helper (or equivalent) that
   gracefully skips when no GPU is available — keeps the test
   well-behaved when the feature is on but the runner has no GPU.
4. Don't reach for `#[serial]` from the `serial_test` crate; the
   `--test-threads=1` invocation in tier 2 already serializes
   everything.

## CI

Tier 1 runs on every PR via `.github/workflows/test.yml`. A tier-2 CI
workflow is **future work** — it requires a GPU runner that isn't wired
yet.

Until the GPU runner lands, run tier 2 locally before merging any
PR that touches `vulkan/rhi/`, encoders/decoders, or display code.
The PR template should call this out explicitly when it's relevant.
