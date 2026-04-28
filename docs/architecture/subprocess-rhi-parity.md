# Subprocess RHI parity — escalate to host vs. ship per-language

> **Living document.** Validate, update, and critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation):
> use Opus, show your work, preserve disagreed-with content with
> reasoning rather than silently deleting. Treat this academically,
> not dogmatically. The conclusions below reflect the best
> understanding of the codebase as of 2026-04-27 (post-#549). Trade-offs
> may shift as new adapters arrive or as existing seams evolve —
> verify against current code before generalizing.

## Question

As polyglot subprocesses (`streamlib-python-native`, `streamlib-deno-native`,
future others) grow beyond one-shot DMA-BUF FD import, where do
host-RHI-equivalent patterns (compute-kernel dispatch, layout
transitions, queue management, frames-in-flight sizing, VMA pool
isolation, EGL DRM-modifier probe, validation/tracing instrumentation)
live? Escalated to the host on every call? Re-implemented per-language?
Some hybrid?

## Context

streamlib's host process owns a privileged Vulkan RHI in
[`libs/streamlib/src/vulkan/rhi/`](../../libs/streamlib/src/vulkan/rhi/):
single `VkInstance` + `VkDevice`, VMA pool isolation for DMA-BUF
export, per-queue submit mutex, `MAX_FRAMES_IN_FLIGHT = 2`, EGL DRM
modifier probe, [`VulkanComputeKernel`](compute-kernel.md) with SPIR-V
reflection, and the entire Vulkan-Video session/DPB. Every solved-on-host
bug class lives there once.

Subprocess customers reach the host through two IPC seams (see
[adapter-runtime-integration.md](adapter-runtime-integration.md) for
the full bucketing):

1. **Surface-share registry** — UDS + `SCM_RIGHTS` FD passing for
   one-shot DMA-BUF + optional timeline-semaphore handoff.
2. **Escalate IPC** — JSON-RPC over stdin/stdout, JTD-typed schemas,
   per-op handlers running on `GpuContextFullAccess`.

#549 (`feat(adapter-vulkan): subprocess VulkanContext runtime`) shipped
the third subprocess runtime, completing the trio after #529
(cpu-readback) and #530 (opengl). After that PR landed, the production
subprocess Vulkan slice is genuinely consumer-only:

| Subprocess Vulkan code path | Status (post-#549) |
| --- | --- |
| `mod vulkan` adapter delegation in both natives | ✓ Delegates every adapter op (`acquire_*`, `end_*_access`, layout transitions) to `streamlib-adapter-vulkan`'s `VulkanSurfaceAdapter`. No per-language reimplementation. |
| `mod opengl` adapter delegation in both natives | ✓ Same pattern, via `streamlib-adapter-opengl::EglRuntime`. |
| `streamlib::adapter_support` re-export (`libs/streamlib/src/lib.rs:180`) | ✓ Curated consumer surface: `VulkanDevice`, `VulkanTexture`, `VulkanPixelBuffer`, `VulkanTimelineSemaphore`. Convention-only — see #525-C. |
| `mod vulkan_compute_dispatch` quarantine | ✗ Raw vulkanalia — descriptor set, pipeline, command buffer, fence, ~200 lines × 2. Mandelbrot example consumes it. PR #549 explicitly tagged this issue (#525) to retire it. |
| `mod surface_share_vulkan_linux` legacy (#420 era) | ✗ Pre-#511 module: DMA-BUF → `VkBuffer` import, ~280 lines × 2. Serves only `examples/polyglot-dma-buf-consumer/`. |

Three host adapter crates (`vulkan`, `opengl`, `cpu-readback`)
duplicate the registry-lock + contention-counter pattern verbatim
(`try_begin_read` / `try_begin_write` ~50 lines × 3). Skia (#513) is
filed and would add a 4th copy if it lands first.

## Three options considered

### Option A — Escalate everything beyond consumer-only

Subprocess Vulkan code does FD import + bind + map + read/write,
nothing else. Every privileged primitive (allocation, kernel
construction, dispatch, modifier discovery, queue submit) goes through
escalate IPC to the host's `GpuContextFullAccess`.

Pros: zero pattern duplication; bug fixes land once; polyglot SDKs
stay tiny. Cons: schema bumps + 3-runtime regen for every new escalate
op; IPC roundtrip latency on the privileged path.

### Option B — Ship per-language Vulkan

Each subprocess native re-implements the patterns it needs;
conformance + golden-input tests catch divergence.

This option is a **phantom in streamlib's actual setup.** The
"subprocess natives" are Rust crates with FFI shims to Python (PyO3)
and Deno (`deno_core`). They're not "per-language" in the sense
PyTorch's bindings to CUDA are — they're per-runtime native shims that
already share a Rust language. The relevant question is whether the
*Rust crate(s) the natives share* own command-buffer recording, or
delegate to the host. Reframing kills B as a distinct option.

Per CLAUDE.md → "Production-grade by default": *"would a future host-side
bug fix need to land in N subprocesses?"* — yes, every solved-on-host
bug class would re-open. Conformance tests don't catch driver-version
bugs (the [`nvidia-dma-buf-after-swapchain`](../learnings/nvidia-dma-buf-after-swapchain.md)
and [`nvidia-dual-vulkan-device-crash`](../learnings/nvidia-dual-vulkan-device-crash.md)
learnings are receipts for that). Option B is the failure mode the
architecture explicitly prevents.

### Option C — Hybrid with a written rule

Bucket per-pattern: which escalate, which ship per-language? Once you
list them out, every privileged-by-nature pattern (allocation,
modifier choice, kernel construction, queue submit, fence management)
belongs on the host side. The "rule" Option C wants to write reduces
to *"everything escalates."* Option C and Option A are the same answer
under analysis.

## Recommendation

**Option A — escalate everything beyond a tightly-bounded import-side
carve-out — is the production-grade answer**, and it matches the model
every comparable system has converged on (Chromium GPU process / Dawn
Wire, Unreal RHI + Shader Compile Workers, VST3 plugin sandbox,
WebGPU/wgpu-core in browsers).

The carve-out is one pattern: **DMA-BUF FD import + bind + map** (and
the tiled-image equivalent via `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`).
Nothing else. The carve-out exists because a subprocess can't share a
host's `VkDevice` across processes; it must construct its own
consumer-only `VkDevice` to bind imported FDs. The carve-out is
small, stable, type-bounded by what `streamlib::adapter_support`
exposes, and consumer-only by construction (no allocator, no kernel
constructor, no queue mutex, no modifier probe).

Everything outside the carve-out goes through escalate IPC:

- Compute dispatch → `RegisterComputeKernel` + `RunComputeKernel`
  ops driving host's `VulkanComputeKernel` (filed as #550).
- Allocation, modifier choice, timeline construction → already
  done by host-side adapter setup (e.g.
  `acquire_render_target_dma_buf_image`).
- Layout transitions, timeline waits → already done by the host
  adapter at acquire/release boundaries (`transition_layout_sync`).
- Queue mutex, frames-in-flight, swapchain → host-only by
  construction (subprocess holds neither a queue nor a swapchain).

This gives streamlib the **Unreal-engine model**: a single RHI is the
source of truth; bug fixes land once and fan out to every consumer
(host adapter, host pipeline, subprocess via escalate IPC).

## Two layers of centralization

The mental model that closes the typical "subprocess RHI parity" gap:
**there are two layers of centralization, not one.**

1. **Host RHI** (privileged) — the engine. All wins live here.
2. **Subprocess consumer-only client** — a thin shim that has to live
   in subprocess address space because `VkDevice` can't cross
   processes. Centralized into `streamlib::adapter_support` (today)
   and graduating to a standalone `streamlib-consumer-rhi` crate
   (#552) so the boundary is type-system enforced.

Both layers have one source of truth.

## Diagram 1 — Today (post-#549)

```
┌──────────────────────────────────────────────────────────────────────┐
│ HOST PROCESS                                                         │
│                                                                      │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  streamlib RHI  (libs/streamlib/src/vulkan/rhi/)             ║    │
│  ║  ALL THE WINS — VulkanComputeKernel, VMA pools, queue mutex, ║    │
│  ║  modifier probe, frames-in-flight=2, dual-device guard       ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲                                                              │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │ streamlib::adapter_support  (curated re-export, #549)         │   │
│  │   pub use VulkanDevice, VulkanTexture, VulkanPixelBuffer,     │   │
│  │           VulkanTimelineSemaphore                             │   │
│  │ ⚠️  Convention only — cdylibs link the FULL streamlib crate,   │   │
│  │    could in theory reach FullAccess types                     │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲      ▲                                         │
│  ┌────┴──┬───┴──┬───┴──┬───┴────────┐                                │
│  │ vk-   │ gl-  │skia- │cpu-rb-     │  host adapter crates           │
│  │ adptr │adptr │adptr │adptr       │  (#513 skia not yet built)     │
│  │       │      │ TODO │            │                                │
│  └───────┴──────┴──────┴────────────┘                                │
│  ✗ Each adapter rolls its own try_begin_read/write registry-lock     │
│    pattern — verbatim ~50 lines × 3 (vk + gl + cpu-rb)               │
│  ✗ Skia (#513) about to add a 4th copy                               │
│       ▲                                                              │
│       │ surface-share + escalate IPC                                 │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│                      │         │                      │
│ python-native cdylib │         │ deno-native cdylib   │
│ ─────────────────────│         │ ─────────────────────│
│ ✓ mod vulkan      ──┼────────►│ ✓ mod vulkan         │ ← #549: adapter
│   delegates to       │         │   delegates to       │   delegation OK
│   adapter-vulkan     │         │   adapter-vulkan     │
│ ✓ mod opengl     ───┼────────►│ ✓ mod opengl         │ ← #530: same
│   delegates to       │         │   delegates to       │
│   adapter-opengl     │         │   adapter-opengl     │
│ ✗ vulkan_compute_   ◄┼─── × ──┼►✗ vulkan_compute_   │ ← #549 quarantine
│   dispatch           │         │   dispatch           │   ~200 lines × 2
│   raw vulkanalia:    │         │   raw vulkanalia:    │   raw desc-set,
│   desc-set, pipeline,│         │   desc-set, pipeline,│   pipeline,
│   cmd buffer, fence  │         │   cmd buffer, fence  │   fence — all
│   (Mandelbrot demo)  │         │   (Mandelbrot demo)  │   reimplemented
│ ✗ surface_share_    ◄┼─── × ──┼►✗ surface_share_    │ ← #420 era
│   vulkan_linux       │         │   vulkan_linux       │   legacy:
│   ~280 lines × 2     │         │   ~280 lines × 2     │   DMA-BUF →
│                      │         │                      │   VkBuffer for
│                      │         │                      │   polyglot-dma-
│                      │         │                      │   buf-consumer
│ ✗ Cargo.toml:        │         │ ✗ Cargo.toml:        │ ← cdylibs link
│   streamlib = full   │         │   streamlib = full   │   FULL streamlib
│   crate (not just    │         │   crate (not just    │   — capability
│   adapter_support)   │         │   adapter_support)   │   boundary not
│                      │         │                      │   type-enforced
└──────────────────────┘         └──────────────────────┘
```

## Diagram 2 — After all P0s land (target state)

```
┌──────────────────────────────────────────────────────────────────────┐
│ HOST PROCESS                                                         │
│                                                                      │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  streamlib RHI  (host-only, FullAccess)                      ║    │
│  ║  unchanged — single source of truth for every fix            ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲                                                              │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │ streamlib-consumer-rhi  (#552 — standalone crate)             │   │
│  │   pub use VulkanDevice, VulkanTexture, VulkanPixelBuffer,     │   │
│  │           VulkanTimelineSemaphore                             │   │
│  │ ✓ Capability boundary TYPE-SYSTEM ENFORCED — consumers        │   │
│  │   physically cannot construct FullAccess types                │   │
│  │ ✓ cdylibs depend on this — NOT on full streamlib crate        │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲      ▲                                         │
│  ┌────┴──┬───┴──┬───┴──┬───┴────────┐                                │
│  │ vk-   │ gl-  │skia- │cpu-rb-     │ host adapter crates            │
│  │ adptr │adptr │adptr │adptr       │ all 4 use shared Registry<T>   │
│  └───────┴──────┴──────┴────────────┘                                │
│       ▲     ▲     ▲     ▲                                            │
│  ┌────┴─────┴─────┴─────┴──────────────────────────────────────┐     │
│  │ streamlib-adapter-abi  (extended in #551)                   │     │
│  │   trait SurfaceRegistration { write_held, read_holders, …}  │     │
│  │   struct Registry<T> {                                      │     │
│  │     fn try_begin_read(&self, id) -> Result<…>               │     │
│  │     fn try_begin_write(&self, id) -> Result<…>              │     │
│  │   }                                                         │     │
│  │   trait SurfaceAdapter        (existing)                    │     │
│  │   error/guard typestates      (existing)                    │     │
│  └─────────────────────────────────────────────────────────────┘     │
│  ✓ ZERO duplicated try_begin_* code; Skia inherits the pattern       │
│                                                                      │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║ Escalate IPC — new ops (#550)                                ║    │
│  ║ ───────────────────────────────────────────────────────────  ║    │
│  ║   RegisterComputeKernel(spv, bindings) → kernel_id           ║    │
│  ║       ↪ host: GpuContext::create_compute_kernel              ║    │
│  ║         (SPIR-V reflection done ONCE, kernel cached)         ║    │
│  ║   RunComputeKernel(kernel_id, surface_uuid, push, dims)      ║    │
│  ║       ↪ host: kernel.dispatch(x, y, z) on host VkDevice      ║    │
│  ║       ↪ timeline-sync to subprocess                          ║    │
│  ║                                                              ║    │
│  ║   Mirrors Unreal RHI / wgpu-core / CUDA / Metal pattern:     ║    │
│  ║   register once, dispatch many                               ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲ surface-share + escalate IPC                                 │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│                      │         │                      │
│ python-native cdylib │         │ deno-native cdylib   │
│ ─────────────────────│         │ ─────────────────────│
│ Cargo.toml deps:     │         │ Cargo.toml deps:     │
│ ✓ streamlib-         │         │ ✓ streamlib-         │
│   consumer-rhi       │         │   consumer-rhi       │
│ ✓ streamlib-adapter- │         │ ✓ streamlib-adapter- │
│   {abi,vulkan,opengl}│         │   {abi,vulkan,opengl}│
│ ✗ NOT streamlib (the │         │ ✗ NOT streamlib (the │
│   FullAccess crate)  │         │   FullAccess crate)  │
│                      │         │                      │
│ ✓ mod vulkan         │ ────►   │ ✓ mod vulkan         │
│ ✓ mod opengl         │ ────►   │ ✓ mod opengl         │
│ ✓ dispatch_compute   │ ────►   │ ✓ dispatch_compute   │ ← #550:
│   THIN escalate-IPC  │         │   THIN escalate-IPC  │   raw vulkanalia
│   wrapper:           │         │   wrapper:           │   DELETED;
│   1. ensure          │         │   1. ensure          │   ~few lines
│      RegisterCompute │         │      RegisterCompute │   each
│      Kernel          │         │      Kernel          │
│   2. RunComputeKernel│         │   2. RunComputeKernel│
│ ⊘ surface_share_     │         │ ⊘ surface_share_     │ ← #553:
│   vulkan_linux       │         │   vulkan_linux       │   legacy retired
│   DELETED            │         │   DELETED            │
└──────────────────────┘         └──────────────────────┘
```

## How wins flow without duplication

The bug-fix-fan-out story — the answer to *"can we leverage the RHI
without duplication?"*:

| RHI win (lives in `vulkan/rhi/`) | How subprocess gets it |
| --- | --- |
| NVIDIA DMA-BUF allocation cap fix (VMA pool isolation) | Host allocates; subprocess only **imports** the FD |
| Render-target needs tiled DRM modifier (NVIDIA EGL trap) | Host **chooses** the modifier; subprocess receives it in the surface descriptor and imports tiled |
| Pre-swapchain allocation pattern | Host runs the pattern; subprocess never allocates |
| Per-queue submit mutex | Host owns the queue; subprocess submits nothing |
| Frames-in-flight=2 vs. swapchain image count | Host owns the swapchain; subprocess doesn't have one |
| `VulkanComputeKernel` SPIR-V reflection | Host runs all dispatches; subprocess `dispatch_compute` is an escalate-IPC call to `RunComputeKernel` (#550) |
| Single `VkDevice` per process (dual-device crash) | Host has its `FullAccess` device; subprocess has its consumer-only device, never submits — dual-device crash trigger (concurrent submission) doesn't apply |
| Layout transitions / timeline waits | Host adapter runs them at acquire/release boundary; subprocess waits on the imported timeline |

**Bug-fix fan-out is exactly 1.** Fix in `vulkan/rhi/` → host adapters
get it free → subprocesses get it indirectly because their inputs
(FDs, kernels, surfaces) and their privileged ops (escalate-IPC results)
come from the RHI. The only place "fix-twice" risk lives is the
import-side carve-out, and that's collapsed into a single shared crate.

## The Unreal parallel made literal

| Unreal | streamlib |
| --- | --- |
| RHI (one engine, all rendering wins live here) | `vulkan/rhi/` |
| Editor calls RHI directly (in-process) | Host adapters call RHI directly |
| Cooker / Lightmass / Shader Compile Workers don't run RHI work — they ship results back | Subprocesses don't run privileged Vulkan — escalate IPC ships ops to the host RHI |
| Shader Compile Worker is one binary, parameterized | `streamlib-consumer-rhi` is one crate, linked from N subprocesses |

## Per-pattern decisions

Every solved-on-host pattern, in priority order from #525's original
inventory:

| Pattern | Bucket | Reasoning |
| --- | --- | --- |
| Compute-kernel SPIR-V reflection + dispatch | **Escalate IPC** (#550 — `RegisterComputeKernel` + `RunComputeKernel`) | Mandelbrot example is the real consumer; PR #549 explicitly tagged this; mirrors Vulkan / WebGPU / CUDA / Metal / Unreal "register once, dispatch many" |
| Layout-transition + queue-family barriers beyond trivial single-shot | **Host-only**, runs at adapter acquire/release boundary | Already shipped that way (vulkan adapter `transition_layout_sync`, cpu-readback bridge) |
| Per-queue mutex (multi-threaded submit) | **Host-only** | Subprocess holds no `VkQueue` requiring guarding; submits nothing |
| Frames-in-flight sizing | **Host-only** | Subprocess has no swapchain |
| VMA pool isolation, exportable allocation | **Host-only** | NVIDIA quota lesson; subprocess never touches `VkExportMemoryAllocateInfo` |
| EGL DRM-modifier probe | **Host-only** | Modifier choice is privileged; subprocess receives the choice and imports |
| DMA-BUF FD import + bind + map | **Subprocess too — the carve-out** | Already shipped via `streamlib::adapter_support` → graduates to standalone crate (#552) |
| Tiled-image import (`VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`) | **Subprocess too**, via the same carve-out | Consumer side of `import_render_target_dma_buf` |
| Validation-layer + tracing | **Host-only**; subprocess uses `tracing::*!` macros that ship via escalate `log` op | Already shipped; not duplication |
| Single `VkDevice` per process (NVIDIA dual-device crash) | **Host has its `FullAccess` device; subprocess has its consumer-only device** | Dual-device crash triggers on *concurrent submission* on two devices ([learning](../learnings/nvidia-dual-vulkan-device-crash.md)); subprocess submits nothing, so the existing carve-out is provably safe |

## Trip-wires

Revisit this doc when **any** of these triggers — the conclusion may
shift if the constraints change:

1. **Subprocess wants to *author* a kernel from raw SPIR-V at runtime
   (rather than register a compiled blob).** That re-introduces the
   reflection / pipeline-cache problems and might justify caching the
   `Arc<VulkanComputeKernel>` per subprocess rather than per dispatch.
   Treat as escalate-IPC `RegisterComputeKernel` extension, not a
   subprocess-side `VulkanComputeKernel` mirror.
2. **Subprocess wants to allocate** (e.g. a Python ring buffer larger
   than what import covers). Escalate the allocation; do not lift the
   import-side carve-out into an export-side one.
3. **`RunComputeKernel` shows up in profiles at frame rate.** Batch
   dispatches (one escalate request covering N) before reaching for
   shared-memory rings.
4. **A new adapter's data-flow shape isn't "static FD lives forever"
   or "host runs work on every acquire."** Re-derive the seam choice;
   the trade-off table in
   [adapter-runtime-integration.md](adapter-runtime-integration.md)
   is not framework-agnostic.
5. **Host-side bug fix can't fan out via escalate IPC** — e.g. a
   driver workaround that has to be applied on the consumer-side
   `VkDevice`. Then the carve-out has to absorb it; document the
   exception explicitly.

## Follow-up issues filed

All in milestone *Surface Adapter Architecture* (#16), all P0 per
user direction "get it done once":

- **#550** — `feat(adapter-vulkan)`: escalate-IPC `RunComputeKernel` +
  `RegisterComputeKernel` ops; retire `vulkan_compute_dispatch`
  quarantine.
- **#551** — `refactor(adapter-abi)`: extract
  `Registry<T: SurfaceRegistration>` scaffolding from host adapter
  crates.
- **#552** — `refactor(consumer-rhi)`: promote
  `streamlib::adapter_support` into standalone `streamlib-consumer-rhi`
  crate; cdylibs drop full `streamlib` dep.
- **#553** — `refactor(natives)`: retire legacy
  `surface_share_vulkan_linux` module from `python-native` +
  `deno-native`. Depends on #552 if option 2 chosen.

Existing milestone-#16 work (#513 skia, #515 refactor) is `Blocked-by`
all four, with the `frozen` label applied so the next picker sees the
halt before reaching for them.

## Open questions for the user

- **Kernel cache lifetime in the host's `EscalateHandleRegistry`** — should
  `RegisterComputeKernel` return a stable kernel_id keyed by SPIR-V
  hash (so re-registration is a cache hit), or always allocate a fresh
  id (forcing the subprocess to track lifetimes)? Stable-by-hash matches
  shader-cache behavior in Vulkan / D3D / Metal pipeline caches.
  Recommend stable-by-hash; flag for confirmation when #550 is picked
  up.
- **Compute pipeline cache persistence across host restarts** — out of
  scope here; defer until startup-cost profiling shows it matters.

## Related

- [adapter-runtime-integration.md](adapter-runtime-integration.md) —
  *how* a subprocess obtains an adapter context. This doc is *what*
  RHI patterns it re-implements once it has one.
- [compute-kernel.md](compute-kernel.md) — the host's
  `VulkanComputeKernel` abstraction the escalate-IPC compute ops
  drive.
- [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md) —
  the workflow rule the import-side carve-out lives under.
- [`docs/learnings/`](../learnings/) — the bug evidence that motivates
  keeping privileged ops on one host VkDevice.
- #525 — the architecture decision request that produced this doc.
