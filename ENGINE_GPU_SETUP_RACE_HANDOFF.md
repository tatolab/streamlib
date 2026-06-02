# Engine concurrent-GPU-setup race — investigation handoff

> **Transient working doc** for a fresh max-effort agent session. The
> plugin-SDK extraction that *preceded* this is DONE and banked (see
> "What's already settled"); this doc is only about the **residual
> engine-layer crash** it uncovered. Delete when the race is fixed.
> This is engine-core / RHI work: **diagnose → write an architecture
> plan → get sign-off → implement.** Do not start editing engine code
> before the plan is approved (project rule for RHI/engine/IPC work).

## The one-line problem

The drone-racer pipeline SIGSEGVs deep in `libnvidia-glcore` via
`vkCreateComputePipelines` during the **concurrent multi-processor
`setup()` fan-out**, on NVIDIA Linux (driver 595.71.05, RTX 3090). It is
a **timing race**, not a deterministic bug — the crash site moves with
overhead and disappears entirely under heavy logging/validation.

## Reproduce

```bash
cd ~/Repositories/tatolab/drone-racer/racer/runner
source ~/Repositories/tatolab/streamlib/scripts/gitea/registry-token.local.sh
cargo build                      # host links streamlib 0.4.35-dev.10
ulimit -c 0
RUST_LOG=warn timeout --kill-after=5 25 ./target/debug/racer-runner; echo "exit=$?"
```
- `exit=139` + `timeout: the monitored command dumped core` = the crash.
- It reproduces on essentially every bare run, AND under `gdb` (so a
  post-mortem/live `bt` is fair game — see below). It does **NOT**
  reproduce under `RUST_LOG=info` + `STREAMLIB_VULKAN_VALIDATION=1`
  (the overhead serializes the threads enough to dodge the race) —
  that path instead exits 1 on a *separate* iceoryx2 issue (see
  "Secondary finding").
- Packages are already built into `racer/.streamlib/cache/packages/`
  (engine-free, dev.10). If you suspect a stale build, clear
  `racer/.streamlib/cache/*` and `~/.streamlib/resolver-cache/*` and
  re-run (first run rebuilds the 5 packages; slow).
- The pipeline is `UdpSource(:5600)→VadrVisionDepayloader→JpegDecoder`
  + `UdpSource(:14550)→MavlinkDecoder` → `RacerPilot` →
  `MavlinkEncoder→UdpSink`. **No UDP feed is needed** — the crash is in
  setup, before any data flows. The GPU processor is **JpegDecoder**
  (the `@tatolab/jpeg` package wrapping `vulkan-jpeg`'s Vulkan-compute
  `SimpleJpegDecoder`); its `setup()` builds a compute kernel, and that
  is where the crash lands.

## The crash — exact stack (Thread 44, the GPU/jpeg thread)

```
#0  libnvidia-glcore.so.595.71.05   (vkCreateComputePipelines internals)
#9  vulkanalia ... DeviceV1_0::create_compute_pipelines        vulkanalia-0.35.0/src/vk/versions.rs:1385
#10 streamlib_engine ... create_compute_pipeline_with_cache    vulkan/rhi/vulkan_compute_kernel.rs:2066
#11 VulkanComputeKernelInner::new_inner                        vulkan/rhi/vulkan_compute_kernel.rs:223
#13 VulkanComputeKernel::new                                   vulkan/rhi/vulkan_compute_kernel.rs:1049
#14 GpuContext::create_compute_kernel                          core/context/gpu_context.rs:1389
#15 host_gpu_full_create_compute_kernel  (host side of the plugin ABI)  core/plugin/host_services/gpu_context/full/kernel_construction.rs:64
#16 with_decoded_compute_kernel_descriptor                     core/rhi/plugin_abi_bridge.rs:187
#19 escalate_scope_registry::with_scope                        core/context/escalate_scope_registry.rs:134  (escalate gate held)
#28 host_gpu_full_create_compute_kernel  (extern "C")
#29 streamlib_plugin_sdk::context::GpuContextFullAccess::create_compute_kernel   (the engine-free jpeg plugin)
```

This is the signature documented in
[`docs/learnings/concurrent-vkdevicewaitidle-threading.md`](docs/learnings/concurrent-vkdevicewaitidle-threading.md):
an NVIDIA driver SIGSEGV with a clean stack landing in `vkCreate*Pipelines`
is "almost always an external-synchronization (threading) violation the
driver tolerated until it corrupted its own state." The pipeline-create
is the **victim**, not necessarily the cause.

## What the all-threads backtrace shows at crash time

Captured with `gdb -batch -ex run -ex 'thread apply all bt 12'` (48
threads total):

- **Thread 44** — the crashing thread, in the jpeg plugin's
  escalate-gated `create_compute_kernel` → `vkCreateComputePipelines`.
  It is the **only** thread in `create_compute_pipeline_with_cache`
  (an earlier read mis-counted it as two — that was the same thread
  dumped twice by two gdb commands; there is **not** a second
  concurrent gated pipeline-create).
- **7 threads** blocked in
  `EscalateGate::enter → Condvar::wait<GateState>` — i.e. the
  FullAccess/escalate GPU work **is being correctly serialized** by the
  escalate gate. The race is therefore **not** among the escalate-gated
  ops.
- **Thread 1 (the main thread)** is blocked on a **`pthread_mutex`
  deep inside `libnvidia-glcore` / `libGLX_nvidia`** (frames:
  `futex_wait → __lll_lock_wait → pthread_mutex_lock → libnvidia-glcore
  → libGLX_nvidia`). On NVIDIA the Vulkan ICD and GL share the
  `libnvidia-glcore` blob, so this is the **NVIDIA driver's internal
  lock being contended** between the main thread and Thread 44's Vulkan
  pipeline compilation. The streamlib frames that drove the main thread
  into the driver are below gdb's symbolization cut-off — **getting a
  deeper/better-symbolized main-thread stack is a key first task** (it
  names what host-side work is contending the driver with the plugin's
  pipeline-create).

### Concurrency timeline (from `RUST_LOG=info`)

```
... HostVulkanDevice pipeline-compiler pre-warmed
... HostVulkanDevice graphics-pipeline compiler pre-warmed       <- pre-warm finishes
... [Pxxxx] Calling setup (thread id=ThreadId(35), runtime=Rust)  } ~8 processor setup()
... [Pxxxx] Calling setup (thread id=ThreadId(36..43))            } calls fan out
...                                                                } CONCURRENTLY,
                                                                     same millisecond
```
So: the engine pre-warms the compute + graphics pipeline compiler at
`HostVulkanDevice::new()`, then the runtime dispatches every processor's
`setup()` concurrently on its own thread. JpegDecoder's `setup()` builds
its compute kernel on one of those threads while the main thread is
inside the driver — and the driver's internal state corrupts.

## What's already RULED OUT (do not re-investigate)

1. **Duplicate-engine / second `streamlib-engine` copy** — the original
   `cdylib-device-panic` cause. FIXED and verified: every loaded plugin
   `.so` (jpeg, mavlink, network, vadr-vision, racer-pilot) has **zero
   `streamlib_engine` symbols** (`nm -D` and `nm`), all built against
   `streamlib-plugin-sdk-0.4.35-dev.10`, host is
   `streamlib-engine-0.4.35-dev.10`.
2. **Version skew (load-vs-build / schema codegen)** — refuted. All five
   plugin `.so` embed `streamlib-plugin-sdk-0.4.35-dev.10`; the host
   embeds `streamlib-engine-0.4.35-dev.10`; schema axis coherent
   (`@tatolab/core ^1.0.0`). No `add_module`-vs-codegen mismatch.
3. **The `ComputeKernelDescriptor` plugin-ABI marshaling** — refuted by a
   thorough multi-agent trace. `ComputeKernelDescriptorRepr` is defined
   **once** in `streamlib-plugin-abi` (`src/repr/compute.rs`), imported
   identically by SDK and engine, `#[repr(C)]`, layout-locked (size 56 /
   align 8 / all 8 offsets asserted). The SPIR-V is `'static
   include_bytes!` rodata in the dlopen'd `.so` (never dangles) and is
   copied into a host-owned `Vec<u32>` *before* the driver sees it;
   bindings decode into a host-owned `Vec` inside a callback-scoped
   lifetime; discriminants agree. The descriptor arrives at the driver
   intact.
4. **Pipeline-cache blob corruption** — refuted: crashes with an empty
   pipeline cache (cleared `racer/.streamlib/cache/pipeline-cache/*`).
5. **Two concurrent gated `vkCreateComputePipelines`** — refuted (the
   "×2" was a gdb double-dump artifact, see above).

## Leading hypothesis + fix directions (to validate, NOT prescribe)

The race is between **Thread 44's Vulkan pipeline compilation** (gated)
and **something the main thread / another non-gated path is doing in the
shared NVIDIA `glcore` driver blob** during the concurrent setup
fan-out. The escalate gate serializes escalate ops but does **not**
serialize whatever the main thread is doing in the driver, nor
necessarily the compiler pre-warm vs. the first plugin kernel build.

Candidate angles for the new agent (verify each against current code):

- **Why is the main thread inside `libGLX_nvidia` at all?** This is a
  pure-Vulkan pipeline (no display/GL processor). Is it the NVIDIA
  Vulkan ICD's own GLX-backed init, a winit/event-loop context, an
  adapter, or driver-internal threading? Get the deeper main-thread
  stack first. If host-side code is doing GL/driver work concurrently
  with plugin GPU setup, serialize or sequence it.
- **`VkPipelineCache` external synchronization.** `create_compute_pipeline_with_cache`
  uses a shared cache (`VkPipelineCache` is externally-synchronized per
  spec). Even with one pipeline-create thread, concurrent driver
  activity touching the cache/compiler may need a device-wide
  serialization (a mutex around all pipeline creation, or around the
  whole "build a kernel" critical section), not just the escalate gate.
- **Compiler pre-warm vs. first plugin kernel.** `#1203` added compute +
  graphics pipeline-compiler pre-warm at `HostVulkanDevice::new()`.
  Confirm the pre-warm fully completes (and its driver threads quiesce)
  before any concurrent `setup()` touches the compiler. The pre-warm
  "decouple from the jpeg layout" commit suggests this area is live.
- **`vkDeviceWaitIdle` routing.** `70012bd7` routed waits through
  `HostVulkanDevice::wait_idle` (holds all queue mutexes) and
  `xtask check-device-wait-idle` lints raw calls. Verify the lint is
  green AND that no path the plugins/Drop-impls pull in does a raw or
  insufficiently-guarded device/queue wait during concurrent setup.
- **Serialize the setup fan-out's GPU work.** The bluntest fix: don't
  let `setup()` run GPU-touching work concurrently — gate all
  device-touching setup behind one lock, or run GPU `setup()` serially.
  Weigh against the perf goal (the fan-out exists for fast startup).

The **prescribed diagnostic** (per the learning doc) is the Khronos
validation layer's threading check — but note the trap below.

## Diagnostic gotchas

- **Validation hides the race.** `STREAMLIB_VULKAN_VALIDATION=1` enables
  `VK_LAYER_KHRONOS_validation` (confirmed: the layer is installed and
  the engine logs `VK_LAYER_KHRONOS_validation enabled` at `info`), but
  its overhead changes timing enough that the crash does **not** fire,
  so it cannot currently name the op. Worse, **the engine registers no
  `VkDebugUtilsMessengerEXT`**, so validation messages have no callback
  and don't reliably surface. To use the threading/synchronization
  validation check, the agent likely needs to (a) register a debug
  messenger that routes to `tracing`, and (b) find a way to keep the
  race live under validation (e.g. synchronization-validation only,
  thread-priority pinning, or a targeted unit/integration repro of
  concurrent pipeline creation + a concurrent driver op).
- **gdb does NOT hide this crash** (reproduces 2/2 under gdb), so
  all-threads backtraces are reliable here even though gdb hid the
  *original* (duplicate-engine) intermittent race.
- `RUST_LOG=info` also perturbs timing enough to avoid the crash — use
  `RUST_LOG=warn` for the bare repro.

## Secondary finding (separate issue — flag, don't conflate)

When the SIGSEGV does **not** fire (under validation/info overhead), the
run instead reaches the WIRE phase and exits 1 on:
```
streamlib_engine::core::logging::stdio_interceptor — Error:
  Runtime("Failed to open/create service:
  PublishSubscribeOpenError(DoesNotSupportRequestedMinBufferSize)")
```
This is an **iceoryx2 service buffer-size** problem (a service's
requested min buffer size isn't supported), distinct from the GPU race.
It surfaced in the very first warmup run too. It may bite once the GPU
race is fixed (large decoded-video-frame payloads on the
`JpegDecoder.video_out → RacerPilot.video_in` link, or the
stdio-interceptor log service). Treat as a follow-up after the race.

## What's already settled (context, not work)

- The plugin-SDK extraction is **complete and banked** on branch
  `fix/vulkan-jpeg-cdylib-device-panic` (streamlib repo, 14 commits;
  drone-racer `racer-pilot` migrated). All drone-racer pipeline packages
  are engine-free. The original duplicate-engine crash mode is gone.
- A coherent `0.4.35-dev.10` is published to the local Gitea
  (`localhost:3300`): cargo crates via
  `STREAMLIB_PUBLISH_ALL_LIBS=1 scripts/gitea/publish-crates.sh --dev 10`,
  and the four engine-free `.slpkg`s (network 1.0.1, vadr-vision 1.0.1,
  jpeg 1.0.6, mavlink 1.1.2) via `scripts/gitea/publish-packages.sh`.
- The branch name's "device-panic" had **two** causes: the
  duplicate-engine one (fixed by the extraction) and **this** engine
  concurrent-GPU-setup race (pre-existing, the `#1203` / `70012bd7`
  area, still open).

## Key files

- `libs/streamlib-engine/src/vulkan/rhi/vulkan_compute_kernel.rs` —
  `create_compute_pipeline_with_cache` (~2066), `new_inner` (~223), the
  shared `VkPipelineCache`.
- `libs/streamlib-engine/src/vulkan/rhi/vulkan_device.rs` —
  `HostVulkanDevice::new` (pre-warm at ~the prewarm block; validation
  knob ~515), `HostVulkanDevice::wait_idle` (the guarded wait).
- `libs/streamlib-engine/src/core/context/escalate_gate.rs` +
  `escalate_scope_registry.rs` — the gate that serializes escalate ops.
- `libs/streamlib-engine/src/core/compiler/compiler_ops/spawn_processor_op.rs`
  — the concurrent `setup()` fan-out ("Calling setup (thread id=...)").
- `xtask/src/check_device_wait_idle.rs` — the raw-wait_idle lint.
- `docs/learnings/concurrent-vkdevicewaitidle-threading.md` — the
  reference learning for this exact signature.

## Success criterion

`racer-runner` reaches and **survives** the concurrent GPU-setup
fan-out (no SIGSEGV / no core) across multiple cold `RUST_LOG=warn`
runs (run it 5–10×, un-wrapped — no gdb/validation, since those mask
the race). Then chase the secondary iceoryx2 issue if it surfaces.
