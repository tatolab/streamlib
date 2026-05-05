# Hardware-integration test tier

streamlib's test suite is split into two tiers, with the boundary
enforced by a Cargo feature so the split can't drift:

| Tier | Triggered by | What it covers | Parallel-safe? |
|---|---|---|---|
| **1 — Unit** | `cargo test` (default) | Pure logic, parsers, state machines, serialization round-trips, mock-backed integration. | Yes — by construction. |
| **2 — Hardware integration** | `cargo test --features streamlib/hardware-tests` | Tests that construct a real `HostVulkanDevice`, allocate GPU memory, exercise the swapchain, etc. | No — must run with `--test-threads=1`. |

Tier 1 is the gate that runs on every PR via the [canonical workspace
baseline](testing-baseline.md). It's parallel-safe by construction — no
test inside the tier-1 set is allowed to require a GPU device or any
other exclusive system resource. Tier 1 should always pass cleanly in
parallel; if it doesn't, the offending test is mis-classified.

Tier 2 is the gate that runs when a change is hardware-relevant — Vulkan
RHI work, encoder/decoder, display, anything in `vulkan/rhi/`. The
canonical command:

```bash
cargo test --features streamlib/hardware-tests --workspace \
    --exclude api-server-demo \
    --exclude camera-deno-subprocess \
    --exclude camera-python-subprocess \
    --exclude camera-rust-plugin \
    --exclude webrtc-cloudflare-stream \
    --no-fail-fast \
    -- --test-threads=1
```

The `--test-threads=1` is mandatory: tier-2 tests serialize on the GPU
device. Running them in parallel deadlocks (most often inside the
NVIDIA Vulkan driver's per-process kernel state, see
[`docs/learnings/nvidia-dma-buf-after-swapchain.md`](learnings/nvidia-dma-buf-after-swapchain.md)).

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
- Future: V4L2 camera capture, audio device probes, display
  swapchains, anything that holds a kernel-level exclusive lock.

Pure-logic tests in the same file (e.g. cache-path string formatting,
SPIR-V reflection validators that operate on byte arrays without ever
constructing a device) stay in tier 1.

## Adding a new hardware test

1. Place the test next to its production code (`vulkan/rhi/foo.rs::tests`).
2. Tag it with `#[cfg_attr(not(feature = "hardware-tests"), ignore =
   "hardware integration — set --features streamlib/hardware-tests +
   run with --test-threads=1. See docs/testing-hardware.md")]`
   immediately above `#[test]`.
3. Use a shared `try_vulkan_device()` helper (or equivalent) that
   gracefully skips when no GPU is available — keeps the test
   well-behaved when the feature is on but the runner has no GPU.
4. Don't reach for `#[serial]` from the `serial_test` crate; the
   `--test-threads=1` invocation in tier 2 already serializes
   everything.

## CI

Tier 1 runs on every PR via `.github/workflows/test.yml` (the canonical
parallel command). A tier-2 CI workflow is **future work** — it
requires a GPU runner, which the
[testing-baseline doc](testing-baseline.md#ci-gate-pending) tracks as
pending behind issue #343.

Until the GPU runner lands, run tier 2 locally before merging any
PR that touches `vulkan/rhi/`, encoders/decoders, or display code.
The PR template should call this out explicitly when it's relevant.
