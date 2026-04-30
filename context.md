# #588 — Branch context (DELETE BEFORE MERGE)

> Persistent across sessions on this branch. New sessions should read this file in full before touching code. The GitHub issue body is the authoritative scope; this file is the *why* behind the scope.

## TL;DR

`streamlib-adapter-cuda` shipped in #587 (PR #592, merge `0092e03`) with a host-flavor scaffold backed by an OPAQUE_FD `VkBuffer` carve-out test that bypasses the consumer-rhi import path. The next chain links #589 (Python cdylib) and #590 (Deno cdylib) cannot stand up a `ConsumerVulkanDevice`-backed runtime today because the OPAQUE_FD plumbing is missing in three places:

1. `ConsumerVulkanDevice::import_dma_buf_memory` is hard-coded to `DMA_BUF_EXT` — no OPAQUE_FD import path exists.
2. `RhiExternalHandle` enum is closed at `DmaBuf` — workspace-wide ripple to extend.
3. `SurfaceStore::register_pixel_buffer_with_timeline` exports via `RhiPixelBufferExport::export_handle()` which is hard-typed to DMA-BUF.

This issue lands the OPAQUE_FD plumbing end-to-end so #589 / #590 can proceed. The work is consolidated in one PR (this branch) by user request: splitting into multiple GitHub issues has historically caused context to get lost at the boundaries.

## Why OPAQUE_FD VkBuffer (not DMA-BUF VkImage)

This was the most important finding. The original #588 framing was wrong.

DLPack's `DLTensor.data` is `*mut c_void` — a flat device pointer. PyTorch / JAX / NumPy-CUDA `from_dlpack` enforce this strictly. There is no DLPack tensor flavor that wraps an opaque CUDA array.

CUDA's two external-memory mapping APIs:

- `cudaExternalMemoryGetMappedBuffer` → returns flat `void*` device pointer. Source memory must be `VkBuffer` (or `VkImage` with linear tiling, which on NVIDIA Linux render targets is not generally usable per `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
- `cudaExternalMemoryGetMappedMipmappedArray` → returns `cudaMipmappedArray_t` (opaque). **Cannot be wrapped in a DLPack capsule.**

Therefore: **the only correct DLPack zero-copy path is OPAQUE_FD `VkBuffer`**, period. The original "VkImage wire-format extension" framing of #588 is a dead end. This is structural, not a driver/version issue.

The cost is one `vkCmdCopyImageToBuffer` per acquire (host pipeline writes its rendered VkImage into the staging buffer before signaling the timeline) — bandwidth-trivial at ~8 MB/frame for 1080p BGRA8.

## Why no CUDA bridge trait

Per the existing `streamlib-adapter-cuda::adapter.rs:23-27` doc comment that landed in #587:

> Per-acquire host *work* (e.g. `vkCmdCopyImageToBuffer`) is **not present** here, on purpose: CUDA imports the OPAQUE_FD memory once at registration time and dispatches kernels in its own context. The timeline semaphore is the only sync surface that has to cross the Vulkan↔CUDA boundary per acquire.

`CpuReadbackBridge` exists because cpu-readback's per-acquire copy must run on the *host* `VkDevice` + queue — there's no other place. CUDA's equivalent runs in CUDA's own context, no bridge needed.

If the host pipeline produces frames into a tiled `VkImage` that needs copying into the staging buffer per frame, that copy runs as a *pipeline step* — it's authored by whoever wires `install_setup_hook` on the application side, not by the CUDA adapter.

(The forensic agent noted #587's PR description has an internal contradiction: the follow-ups bullet listed "CUDA bridge trait" as needed work without re-checking against the adapter's own docs that say it isn't. We're explicitly resolving this here as: no bridge.)

## Forensic record on #587 / PR #592

- **Deliberate scope-cut**: PR #592 body says verbatim *"VkBuffer (not VkImage) for the carve-out — byte-equal validation requires deterministic linear layout. VkImage import via `cudaExternalMemoryGetMappedMipmappedArray` lands when #588 extends the wire format with full `VkImageCreateInfo`."* The premise — that VkImage was the right downstream target — was wrong, but the scope-cut itself was sound for the carve-out test.
- **Real shipped artifacts**: `HostVulkanPixelBuffer::new_opaque_fd_export`, `HostVulkanTimelineSemaphore::new_exportable` + `export_opaque_fd`, `physical_device_uuid()`, `CudaSurfaceAdapter<D>`, `CudaContext`, `CudaReadView` / `CudaWriteView` (vk::Buffer + size only, no CUDA types), `register_host_surface` / `unregister_host_surface`, empty `streamlib-adapter-cuda-helpers` lib + carve-out integration test exercising real `cudaImportExternalMemory(OPAQUE_FD)` → `cudaExternalMemoryGetMappedBuffer` → `cudaImportExternalSemaphore` → `cudaWaitExternalSemaphoresAsync_v2` → `cudaMemcpyAsync` D→H.
- **Did NOT land**: no `cuda_bridge.rs`, no `set_cuda_bridge` setter, no VkImage export path, no schema changes, no architecture / learnings doc, no update to #589 / #590 bodies.

## Open empirical question (Stage 8)

⚠️ **Confirm `cudaPointerGetAttributes(dev_ptr).type == cudaMemoryTypeDevice` for HOST_VISIBLE OPAQUE_FD imports.** The current `new_opaque_fd_export` allocates HOST_VISIBLE | DEVICE_LOCAL-equivalent memory (host-mapped via VMA pool). Some CUDA driver versions can degrade HOST_VISIBLE imports to pinned-host (PCIe per access) which would tank inference performance. The existing `cuda_carve_out.rs` test asserts byte equality but doesn't probe pointer attributes.

If the probe reports `cudaMemoryTypeHost`: drop HOST_VISIBLE on the staging buffer. The host-side mapped-pointer convenience disappears, which is fine — the host-side code paths that need to populate the buffer can do so via `vkCmdCopyImageToBuffer` / `vkCmdCopyBuffer` instead of memcpy through the mapped pointer.

### Stage 8 status (2026-04-30)

Probe **assertion shipped** in `cuda_carve_out.rs::host_buffer_to_cuda_byte_equal_round_trip` (Phase 4a). Calls `sys::cudaPointerGetAttributes(dev_ptr)` after `cudaExternalMemoryGetMappedBuffer` returns the device pointer; matches on `cudaMemoryType`:
- `cudaMemoryTypeDevice` → expected (pass-through, no panic).
- `cudaMemoryTypeHost` → panics with the action-list above (drop HOST_VISIBLE, document, flip dlpack default).
- `cudaMemoryTypeUnregistered` / `cudaMemoryTypeManaged` → panics with "investigate driver before proceeding".

**Empirical answer: PENDING** — confirmation requires a rig with `libcudart.so.12.9` or newer. This branch's local rig has `libcudart.so.12.0` (from IsaacSim's bundled CUDA runtime); cudarc 0.19.4 with `cuda-12090` feature eagerly loads symbols including `cudaEventElapsedTime_v2`, which 12.0 doesn't export, so culib initialization fails before reaching the probe. (Confirmed pre-existing: failure reproduces on the pristine pre-Stage-8 commit, so it's unrelated to the probe addition.) The cdylib production rigs (Jetson Orin / x86 + dGPU with current NVIDIA driver) ship libcudart 12.x where x ≥ 9 alongside the driver, and the assertion will fire there.

**Decision until empirical confirmation arrives**: stay on `kDLCUDA = 2` for the cdylib's DLPack capsule device. The capsule-builder API takes the `dlpack::Device` as a parameter (not hard-coded), so a flip to `kDLCUDAHost = 3` later is a one-line change inside the cdylib's `cudaPointerGetAttributes` branch — no API churn in `streamlib-adapter-cuda` or `streamlib-consumer-rhi`.

## Design decisions taken in this branch

(Each session appends decisions here as they're made. Future sessions should treat anything below as load-bearing for this PR.)

- **2026-04-30** — Issue scope expanded from "wire-format VkImage extension + bridge" to "full OPAQUE_FD plumbing chain" per parallel-agent research. Original framing struck through in issue body with reasoning preserved.

- **2026-04-30** — DLPack capsule construction approach: **vendored `dlpark = "=0.6.0"` with `default-features = false`**. Rationale (Opus parallel research, max reasoning, summarized in the Stage 7 commit message):
  - All ML / Python deps in dlpark (`pyo3`, `ndarray`, `cudarc`, `half`, `image`, `candle-core`) are optional; `default-features = false` pulls in only `bitflags` + `snafu`, both already permissive and idiomatic in our tree.
  - License Apache-2.0 — compatible with BUSL-1.1 inbound.
  - dlpark's `dlpark::ffi` module exposes the exact `#[repr(C)]` mirrors of the DLPack v0.8 spec (`Tensor`, `ManagedTensor`, `Device`, `DeviceType`, `DataType`, `DataTypeCode`) — same structs we'd hand-roll, but pre-tested. Also ships v1.0 `ManagedTensorVersioned` for free, in case a future consumer requires it.
  - We use `dlpark::ffi::*` as the layout-stable C-ABI mirror only; the manager-ctx + deleter plumbing that keeps an `Arc`-or-equivalent alive lives in `streamlib-adapter-cuda::dlpack`. dlpark's own safe wrappers (`SafeManagedTensor`, `ManagerCtx<T, L>`, `TensorLike`/`MemoryLayout` traits) are NOT used — they assume single-process Python ownership models we don't want to thread through our crate.
  - Layout regression test (Stage 7) pins `Device`/`DataType`/`Tensor`/`ManagedTensor` field offsets and key enum discriminants (`DeviceType::Cuda = 2`, `DeviceType::CudaHost = 3`) — catches dlpark drift if upstream ever ships a breaking change.
  - Pin to `=0.6.0` exact (not `^0.6.0`) at workspace level so the upgrade story stays explicit. If dlpark stalls or pulls in unwanted deps in a future minor, we fork ~400 LOC of `ffi/` + `legacy/manager_context.rs` into our own crate — a 1-day swap.

- **2026-04-30** — No CUDA bridge trait. The single-pattern principle (`docs/architecture/subprocess-rhi-parity.md`) is preserved without one — CUDA falls under "no per-acquire host work" alongside the Vulkan / OpenGL adapters, not alongside cpu-readback.

- **2026-04-30** — Stage ordering (2→3→4→5→6) is a hard dependency chain; later stages (7, 8, 9) parallelize after Stage 6.

- **2026-04-30** — Stage 3 polymorphic-export design. The `RhiPixelBufferExport::export_handle()` trait method stays as-is; its impl for `RhiPixelBuffer` delegates to a new `HostVulkanPixelBuffer::export_external_handle()` which dispatches internally on `is_opaque_fd_export`. Rejected alternatives:
  - Adding `is_opaque_fd_export()` accessor + caller-side dispatch — leaks the flag.
  - Adding a separate `export_opaque_fd_handle()` trait method — two parallel paths the caller has to know about.
  - Try-OPAQUE_FD-fallback-DMA-BUF — error path as control flow.
  Chosen shape: caller asks for "the natural handle" and gets back a tagged variant; surface-share / consumer-rhi code dispatches on the variant. Engine-grade single-path.

## File-path index

### Foundation from #587 (don't edit unless explicitly required)
- `libs/streamlib-adapter-cuda/src/lib.rs`
- `libs/streamlib-adapter-cuda/src/adapter.rs` — `CudaSurfaceAdapter<D>`
- `libs/streamlib-adapter-cuda/src/state.rs` — `HostSurfaceRegistration`
- `libs/streamlib-adapter-cuda/src/context.rs`
- `libs/streamlib-adapter-cuda/src/view.rs` — `CudaReadView` / `CudaWriteView` (Stage 7 extends)
- `libs/streamlib-adapter-cuda-helpers/tests/cuda_carve_out.rs` (Stage 8 extends)

### Files that change in this PR
- `libs/streamlib/src/core/rhi/external_handle.rs` — Stage 2: new `OpaqueFd` variant
- `libs/streamlib/src/vulkan/rhi/vulkan_pixel_buffer.rs` — Stage 3: wire `export_opaque_fd_memory` through `RhiPixelBufferExport`
- `libs/streamlib/src/linux/surface_share/{state,unix_socket_service}.rs` — Stage 4: wire format `handle_type` discriminator
- `libs/streamlib/src/core/context/surface_store.rs` — Stage 4: register/lookup OPAQUE_FD pixel-buffer-with-timeline
- `libs/streamlib-surface-client/src/linux.rs` — Stage 4: client-side lookup
- `libs/streamlib-consumer-rhi/src/consumer_vulkan_device.rs` — Stage 5: `import_opaque_fd_memory`
- `libs/streamlib-consumer-rhi/src/consumer_vulkan_pixel_buffer.rs` — Stage 5: `from_opaque_fd`
- `libs/streamlib-adapter-cuda-helpers/tests/` — Stage 6: cross-crate integration test
- `libs/streamlib-adapter-cuda/src/view.rs` — Stage 7: DLPack capsule accessor
- `libs/streamlib-adapter-cuda-helpers/tests/cuda_carve_out.rs` — Stage 8: pointer-attributes assertion
- `docs/architecture/adapter-runtime-integration.md` — Stage 9: CUDA row
- `docs/architecture/subprocess-rhi-parity.md` — Stage 9: OPAQUE_FD row

## References

- GitHub issue: tatolab/streamlib#588
- Foundation: PR #592 / commit `0092e03`
- Engineering analysis source: 3 parallel Opus agents 2026-04-30, summarized in this file.

### Architectural anchors
- `docs/architecture/adapter-runtime-integration.md` — single-pattern principle, surface-share seam
- `docs/architecture/subprocess-rhi-parity.md` — what subprocess Vulkan does (carve-out only)
- `docs/learnings/nvidia-egl-dmabuf-render-target.md` — why linear DMA-BUF VkImage is sampler-only on NVIDIA
- `docs/learnings/nvidia-dma-buf-after-swapchain.md` — pre-swapchain allocation rule (already followed)

### Closely-related code (read for shape parity)
- `libs/streamlib/src/core/context/cpu_readback_bridge.rs` — what we're explicitly NOT doing for CUDA, but the right shape if a future adapter needs per-acquire host work
- `libs/streamlib-adapter-cpu-readback/` — closest precedent for the consumer-flavor adapter shape (Stage 6 should mirror its integration test)
- `libs/streamlib-consumer-rhi/src/consumer_vulkan_pixel_buffer.rs::from_dma_buf_fd[s]` — shape to mirror for `from_opaque_fd` (Stage 5)
- `libs/streamlib-consumer-rhi/src/consumer_vulkan_sync.rs::ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd` — already exists; OPAQUE_FD timeline import works today, so Stage 5 only needs to add the *memory* import to match

## Anti-patterns to avoid

(Each session should re-read this list before starting work.)

1. **Don't add `cudaImportExternalMemory` calls in this PR.** Stage 6's test asserts the OPAQUE_FD plumbing without `cudarc`. Real CUDA imports happen in #589 / #590, where they already do (carve-out test).
2. **Don't introduce VkImage import into `consumer-rhi`** as a "while I'm here" cleanup. The DLPack constraint rules it out for the immediate downstream work, and `subprocess-rhi-parity.md` says the carve-out is intentionally narrow.
3. **Don't widen the `RhiExternalHandle` enum beyond `OpaqueFd`** in this PR. Future variants (e.g. `Win32Handle`) belong in their own scoped issue.
4. **Don't restructure `surface-share`'s wire format** beyond the additive `handle_type` field. Even if the JSON shape feels suboptimal, this PR is a vertical slice — wire-format refactors live in their own issue.
5. **Don't add a CUDA bridge trait** even if it "feels symmetric" with cpu-readback. See "Why no CUDA bridge trait" above.

## Pre-flight per session

When picking this branch up:

1. `git fetch origin && git checkout feat/cuda-opaque-fd-plumbing-588 && git pull --ff-only`.
2. Read this file in full, including the per-session log in `tasklist.md`.
3. Run `cargo check --workspace` to confirm the branch builds before you touch anything.
4. Pick the next unchecked stage in `tasklist.md`. Start with whichever stage's prerequisites are met.
5. Update `tasklist.md` per-session log and design decisions (this file) as you work.
6. Commit per-stage; each stage that adds tests should be a self-contained commit so the next session can revert cleanly if needed.
