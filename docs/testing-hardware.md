# Hardware-integration test tier

streamlib's test suite is split into two tiers, with the boundary
enforced by a Cargo feature so the split can't drift:

| Tier | Triggered by | What it covers | Parallel-safe? |
|---|---|---|---|
| **1 — Unit** | `cargo test` (default) | Pure logic, parsers, state machines, serialization round-trips, mock-backed integration. | Yes — by construction. |
| **2 — Hardware integration** | `cargo test --features streamlib/hardware-tests,streamlib-media-builtins/hardware-tests` | Tests that construct a real `HostVulkanDevice`, allocate GPU memory, exercise the swapchain, etc. | No — must run with `--test-threads=1`. |
| **Multi-process mesh end-to-end** | `cargo test -p streamlib-engine --features multi-process-mesh-e2e-tests --test runtime_mesh_two_processes --test cross_runtime_links_two_processes --test cross_runtime_link_requests_two_processes` | Real runtimes in separate processes over Zenoh on loopback: discovery, links, link requests. | Serial within each suite. |

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
  openable PCM for the ALSA arm, a Mac with a default device for the
  CoreAudio arm — and, for CoreAudio capture, microphone access already
  allowed for the terminal running it. Capture and playback are separate
  endpoints, and a test naming one says which in its ignore reason. The
  CoreAudio content test that hears its own playback through a muted process
  tap also needs System Audio Recording allowed for that terminal (System
  Settings › Privacy & Security › Screen & System Audio Recording › System
  Audio Recording Only); without it the tap returns digital zeros and the test
  fails naming the setting.
- Future: V4L2 camera capture, display swapchains, anything that holds a
  kernel-level exclusive lock.

Pure-logic tests in the same file (e.g. cache-path string formatting,
SPIR-V reflection validators that operate on byte arrays without ever
constructing a device) stay in tier 1.

### Audible tests are attended only

The `hardware-tests` sweep stays silent: playback tests there write zeros,
capture tests only listen, and one CoreAudio content test plays a tone only
into a muted process tap of itself
(`coreaudio_arm_hears_its_own_playback_through_a_muted_process_tap`). That test
first plays a pilot at about −120 dBFS, below hearing on any output, and plays
its 440 Hz tone at 0.5 only once the pilot has come back through the tap. A
tap macOS does not let read, with System Audio Recording not allowed, so fails
on the pilot with nothing audible played. That a reading tap keeps the tone
off the output rests on Core Audio's documented muted-tap behaviour and has
not yet been observed on a Mac, so run it attended, with the speakers
audible, until someone has heard it stay silent:

```bash
cargo test -p streamlib-engine --features hardware-tests \
  --test coreaudio_arm_hears_its_own_playback_through_a_muted_process_tap \
  -- --test-threads=1 --nocapture
```

A test that plays sound a person hears is gated on `audible-hardware-tests`
instead (it implies `hardware-tests`), and is run by someone at the machine:

```bash
cargo test -p streamlib-engine --features audible-hardware-tests \
  --test coreaudio_arm_hears_what_it_plays -- --test-threads=1 --nocapture
cargo test -p streamlib-media-builtins --features audible-hardware-tests \
  --test speaker_sink_matches_its_device -- --test-threads=1
```

The second is gated on a Mac only, where the CoreAudio arm plays it through the
default output; on Linux it is not gated.

### Microphone access for the CoreAudio capture tests

macOS asks on behalf of the application that launched the test — the terminal,
not `cargo`. A CoreAudio capture test checks access before it measures:

- Never asked: it asks, and fails with "allow <terminal> in the prompt, then
  re-run". Click Allow and run it again.
- Refused: it fails naming System Settings › Privacy & Security › Microphone.
  Turn the terminal on there and run it again.

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

### A window test on Apple

On Apple only the process's first thread can drive the window event pump, and
libtest never runs a test there. A window test is therefore its own
`harness = false` binary whose `main` drives the pump, declared with
`required-features = ["hardware-tests"]` so it is built only in tier 2, and with
an empty `main` off Apple. Two exist today:
`streamlib-engine`'s `processor_owned_window_on_the_first_thread` and
`streamlib-media-builtins`' `two_display_windows_on_the_macos_window_server`.
The second asserts against the window server, so it refuses to run while the
login session's screen is locked.

## Multi-process end-to-end tests are never a merge gate

A test that launches processes and waits on them to find each other over a
network depends on start-up timing and discovery, which no bound makes
deterministic. Such suites are local validations: run them when a change
touches the mesh, never as a PR gate. CI compiles them with `--no-run` so
they cannot rot. Engine logic they reach belongs in an in-process unit test
against a mocked seam, which is what gates a PR.

## CI

Tier 1 runs on every PR via `.github/workflows/test.yml`. A tier-2 CI
workflow is **future work** — it requires a GPU runner that isn't wired
yet.

Until the GPU runner lands, run tier 2 locally before merging any
PR that touches `vulkan/rhi/`, encoders/decoders, or display code.
The PR template should call this out explicitly when it's relevant.
