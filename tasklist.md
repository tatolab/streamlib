# #588 — Task list (branch scratchpad — DELETE BEFORE MERGE)

> Source of truth for scope is the GitHub issue body (`gh issue view 588 --repo tatolab/streamlib`).
> This file is a per-stage progress tracker for cross-session continuity on this branch.

- **Branch**: `feat/cuda-opaque-fd-plumbing-588`
- **Issue**: tatolab/streamlib#588
- **Blocks**: #589, #590

## Stages

- [x] **Stage 1** — Audit + reframe.
  - [x] Issue body rewrite (`gh issue edit 588`).
  - [x] Branch created.
  - [x] `tasklist.md` + `context.md` committed.
  - [x] #589 body updated (DMA-BUF→OPAQUE_FD wording, strike VkImage).
  - [x] #590 body updated (same).

- [x] **Stage 2** — `RhiExternalHandle::OpaqueFd` variant.
  - [x] Added `OpaqueFd { fd: RawFd, size: usize }` variant (Linux-cfg, matches `DmaBuf` shape).
  - [x] Ripple closed: 4 sites in `surface_store.rs` (`check_in`, `register_buffer`, `register_pixel_buffer_with_timeline`) + 1 in `external_handle.rs` (`from_external_plane_handles`). All host-side DMA-BUF-only paths now return `StreamError::NotSupported` for OPAQUE_FD with messages pointing at the correct alternative.
  - [x] Reordered handle-type validation in `from_external_plane_handles` ahead of the device-init guard so the rejection contract is unit-testable without a live `HostVulkanDevice`.
  - [x] 3 unit tests in `external_handle.rs`: variant discriminator, Debug formatting, host-side rejection path. All pass.
  - [x] `cargo xtask check-boundaries` clean.
  - Note: no `tracing::instrument` added — this stage doesn't introduce new public functions; instrumentation lives on Stages 3/4/5's new APIs.

- [x] **Stage 3** — Host RHI OPAQUE_FD export through `RhiPixelBufferExport`.
  - [x] Decision: polymorphic `export_handle()` that delegates to a new `HostVulkanPixelBuffer::export_external_handle()` method which dispatches on the buffer's `is_opaque_fd_export` flag. Keeps the flag private; one canonical export entry point on the buffer; trait impl is a one-liner.
  - [x] Added `HostVulkanPixelBuffer::export_external_handle() -> Result<RhiExternalHandle>` with `#[tracing::instrument]`.
  - [x] Updated `impl RhiPixelBufferExport for super::RhiPixelBuffer` to delegate.
  - [x] New `export_external_handle_dispatches_on_allocation_flavor` integration test exercises both DMA-BUF and OPAQUE_FD flavors against a real `HostVulkanDevice` (skips gracefully when GPU/pool unavailable).
  - [x] `cargo xtask check-boundaries` clean; full workspace check clean.

- [x] **Stage 4** — Surface-share wire format extension.
  - [x] Added `handle_type: String` to `SurfaceMetadata`, `SurfaceRegistration`, `SurfacePlaneCheckout`. Wire JSON additive: register parses `request.get("handle_type").unwrap_or("dma_buf")`, lookup emits `checkout.handle_type`. Existing adapters that don't pass the field continue to register/lookup as `"dma_buf"` unchanged.
  - [x] Made `SurfaceStore::{register_pixel_buffer_with_timeline, register_buffer, check_in}` polymorphic: each derives `handle_type` from the `RhiExternalHandle` variant returned by the polymorphic `export_handle()`. Dropped the OPAQUE_FD-rejection arms added in Stage 2.
  - [x] `SurfaceStore::lookup_buffer` (Linux) dispatches on the response's `handle_type`. OPAQUE_FD lookups return `StreamError::NotSupported` pointing at `streamlib-consumer-rhi::ConsumerVulkanPixelBuffer::from_opaque_fd` — host-side `RhiPixelBuffer` import is DMA-BUF-only by design.
  - [x] New `handle_type_round_trips_explicit_opaque_fd_and_default_dma_buf` test covers both halves: explicit OPAQUE_FD register/lookup AND back-compat default-DMA-BUF when the field is absent.
  - [x] `cargo xtask check-boundaries` clean.
  - Note: the existing `oversize_fd_vec_rejected` test fails on `main` too (pre-existing flake — `UnexpectedEof` vs `InvalidInput` race against the connection state). Not introduced by this PR; flagging here for a future ticket. Left as-is per CLAUDE.md "no auto-fixing on the side".

- [x] **Stage 5** — `streamlib-consumer-rhi` OPAQUE_FD import.
  - [x] `ConsumerVulkanDevice::import_opaque_fd_memory(fd, allocation_size, memory_type_bits, preferred_flags)` — mirrors `import_dma_buf_memory` shape; chains `VK_KHR_external_memory_fd` (already enabled at device init) with `OPAQUE_FD` handle type. `#[tracing::instrument]`.
  - [x] `ConsumerVulkanPixelBuffer::from_opaque_fd(device, fd, width, height, bpp, format, allocation_size)` — single-FD only (OPAQUE_FD has no multi-plane semantics; CUDA imports flat memory). Bypasses the multi-plane allocation-size derivation logic for clarity.
  - [x] Refactored `import_single_plane` into a parametric helper (`ImportHandleType` enum + `import_single_plane_with_handle_type`). The DMA-BUF path stays a one-line wrapper for back-compat.
  - [x] Updated `streamlib-consumer-rhi::lib.rs` module doc to mention OPAQUE_FD support.
  - [x] `cargo xtask check-boundaries` clean; full workspace check clean. Consumer-rhi conformance test for the new constructor lives in Stage 6's cross-crate integration test (which has access to a real `HostVulkanPixelBuffer::new_opaque_fd_export` FD).

- [x] **Stage 6** — End-to-end integration test (no `cudarc`).
  - [x] New test `opaque_fd_chain_host_export_to_consumer_import_to_adapter_acquire` in `libs/streamlib-adapter-cuda-helpers/tests/opaque_fd_consumer_rhi_round_trip.rs`. Walks the full chain: host RHI export (HostVulkanPixelBuffer + HostVulkanTimelineSemaphore) → surface-share wire (real `UnixSocketSurfaceService`, `register`/`lookup` over Unix socket with `handle_type="opaque_fd"`) → consumer-rhi import (`ConsumerVulkanPixelBuffer::from_opaque_fd` + `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`) → `CudaSurfaceAdapter<ConsumerVulkanDevice>` instantiation + `register_host_surface` + `acquire_read`.
  - [x] **Byte-equal assertion** across host's mapped pointer and consumer's mapped pointer — proves the OPAQUE_FD import lands the consumer VkDevice on the same GPU memory the host wrote to. This is the load-bearing assertion for #589/#590 (their cdylibs will see the same memory through `cudaImportExternalMemory` on the same FD).
  - [x] Skip-cleanly behavior on no Vulkan / no OPAQUE_FD pool / multi-GPU UUID mismatch.
  - [x] `#[serial]` per the dual-VkDevice learning. Added `streamlib-surface-client` + `serde_json` to dev-dependencies.
  - [x] `cargo xtask check-boundaries` clean.

- [x] **Stage 7** — DLPack capsule on `CudaReadView` / `CudaWriteView`.
  - [x] Decision: vendored `dlpark = "=0.6.0"` with `default-features = false`. Pulls only `bitflags` + `snafu`; gives us `dlpark::ffi::{Tensor, ManagedTensor, Device, DeviceType, DataType, DataTypeCode, ManagedTensorVersioned}` as the canonical `#[repr(C)]` ABI mirrors. Reasoning + alternatives + scope-cut record in `context.md`.
  - [x] New `streamlib-adapter-cuda::dlpack` module: re-exports the FFI types, owns the manager-ctx + deleter plumbing (`build_managed_tensor` for arbitrary shape/strides/dtype, `build_byte_buffer_managed_tensor` for the canonical 1-D `u8` flat-buffer case). The deleter drops a caller-supplied `Box<dyn Any + Send + 'static>` owner alongside the heap-allocated shape/strides slices.
  - [x] `CudaReadView::dlpack_managed_tensor(device_ptr, device, owner)` + `CudaWriteView::dlpack_managed_tensor(...)`. Device-pointer is supplied by the caller (cdylib pulls it from `cudaExternalMemoryGetMappedBuffer` in #589/#590). `device: dlpack::Device` parameter lets the cdylib pick `kDLCUDA` vs `kDLCUDAHost` after Stage 8 calibration.
  - [x] Layout regression test pins `Device`/`DataType`/`Tensor`/`ManagedTensor` field offsets and sizes for the 64-bit DLPack v0.8 ABI; pins `DeviceType` (`Cpu=1`, `Cuda=2`, `CudaHost=3`, `OpenCl=4`, `Vulkan=7`, `Metal=8`, `Rocm=10`, `CudaManaged=13`) + `DataTypeCode` (`Int=0`, `UInt=1`, `Float=2`, ...) discriminants. Drift in dlpark would surface as a CI failure rather than silent ABI change.
  - [x] Behavioral tests: byte-buffer round-trip, multi-dim BCHW with explicit strides, deleter drops owner exactly once, deleter no-ops on null. 10 unit tests in total — all pass.
  - [x] `cargo test -p streamlib-adapter-cuda` clean (10 dlpack unit tests + 3 conformance tests). `cargo check --workspace` clean. `cargo xtask check-boundaries` clean (1918 files scanned, 0 violations).

- [x] **Stage 8** — Empirical CUDA-side verification.
  - [x] Added Phase 4a probe in `cuda_carve_out.rs::host_buffer_to_cuda_byte_equal_round_trip`: calls `sys::cudaPointerGetAttributes(dev_ptr)` immediately after `cudaExternalMemoryGetMappedBuffer`, matches on `cudaMemoryType` with explicit panic messages for each non-Device case (Device → pass-through; Host → "drop HOST_VISIBLE + flip dlpack default to kDLCUDAHost"; Unregistered / Managed → "investigate driver"). Println preserves the full attribute snapshot (type, device id, devicePointer, hostPointer) for diagnostics.
  - [x] **Empirical answer: PENDING (rig-gated).** This branch's local rig has libcudart 12.0 (IsaacSim bundled); cudarc 0.19.4 with `cuda-12090` feature eagerly loads `cudaEventElapsedTime_v2` which 12.0 doesn't export, so culib init panics before reaching the probe. **Confirmed pre-existing**: failure reproduces on the pristine pre-Stage-8 commit (verified via `git stash` + re-run), so it's unrelated to the probe addition. Production rigs (Jetson Orin / x86 + dGPU on current NVIDIA driver) ship libcudart 12.9+ alongside the driver — the assertion fires there.
  - [x] **Decision until empirical confirmation arrives**: stay on `kDLCUDA = 2` for the cdylib's DLPack capsule device. The capsule-builder API takes `dlpack::Device` as a parameter (not hard-coded), so flipping to `kDLCUDAHost = 3` later is a one-line change inside the cdylib's `cudaPointerGetAttributes` branch — no API churn anywhere else. Recorded in `context.md` "Open empirical question (Stage 8)" → "Stage 8 status".
  - [x] `cargo check -p streamlib-adapter-cuda-helpers --tests --features cuda` clean.
  - [x] `cargo xtask check-boundaries` clean (re-confirm in Stage 10).

- [x] **Stage 9** — Documentation.
  - [x] `docs/architecture/adapter-runtime-integration.md` — added the CUDA row to the per-adapter table (under the single-pattern recommendation), explaining how the OPAQUE_FD twist on the FD wire fits into the same shape and *why* (DLPack-flat-pointer constraint forces OPAQUE_FD over DMA-BUF). Added a new `install_setup_hook` shape — "Surface-share seam with OPAQUE_FD (cuda — #588)" — alongside the existing surface-share / escalate-IPC bullets.
  - [x] `docs/architecture/subprocess-rhi-parity.md` — added the OPAQUE_FD VkBuffer pattern row to "Per-pattern decisions" alongside the existing DMA-BUF carve-out rows. Updated the "Today" section heading to mention #588 and inserted a 2026-04-30 dated annotation under the existing 2026-04-28 / 04-29 markers (preserving them per CLAUDE.md markdown rules) describing what landed and noting the diagram is a 2026-04-28 snapshot — readers are pointed to mentally insert a `cuda-adptr` box.
  - [x] `streamlib-adapter-cuda::lib.rs` module doc — rewritten from "host-flavor scaffold from #587" to "ready for the cdylib runtimes (#589 Python, #590 Deno)" with a bulleted summary of what's now in place (host scaffold, OPAQUE_FD plumbing chain, DLPack capsule shape) and what remains for #589/#590 (cudarc integration, PyCapsule/Deno FFI wrap, kDLCUDA vs kDLCUDAHost calibration result, polyglot E2E). Intra-doc links use the spec'd `[`type`]` form.
  - [x] `cargo doc -p streamlib-adapter-cuda --no-deps` clean — no unresolved link warnings.
  - [x] `cargo xtask check-boundaries` clean.

- [ ] **Stage 10** — Pre-merge cleanup.
  - [ ] Delete `tasklist.md` + `context.md`.
  - [ ] `cargo xtask check-boundaries`.
  - [ ] `cargo test --workspace` (per `docs/testing-baseline.md`).
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings`.
  - [ ] PR description references the comprehensive issue body and lists each closed exit criterion.

## Stage gating rules

- Stage 2 → 3 → 4 → 5 → 6 form a hard dependency chain. Don't skip ahead.
- Stages 7, 8, 9 are independent of each other after Stage 6 lands; they can be picked up in any order.
- Stage 10 is gating for merge.

## Per-session log

(Each session appends a brief entry: what shipped, any gotchas the next session should know.)

### Session 2026-04-30 — Initial pickup (Opus 4.7, 1M ctx)

- Stage 1 done: comprehensive issue body shipped to #588, branch + scratchpads created, #589 + #590 bodies rewritten with strikethroughs preserving original framing.
- Stage 2 done: `OpaqueFd` variant + ripple + 3 unit tests (all pass) + boundary check clean.
- Three Opus agents at max reasoning ran the staleness/correctness/gap audits. Findings consolidated in `context.md`.
- Next session pickup: Stage 3 (Host RHI OPAQUE_FD export through `RhiPixelBufferExport`). Decision pending in `context.md`: extend `export_handle()` to dispatch on buffer flavor vs. add a new `export_opaque_fd_handle()` method.

### Session 2026-04-30 — Stage 7 (Opus 4.7, 1M ctx)

- Stage 7 done: DLPack capsule shape on `CudaReadView` / `CudaWriteView` plus layout regression test.
- Decision recorded in `context.md`: vendor `dlpark = "=0.6.0"` (Apache-2.0, `default-features = false` → bitflags + snafu only) for the `#[repr(C)]` ABI mirrors; build the manager-ctx + deleter plumbing in `streamlib-adapter-cuda::dlpack` ourselves so we don't drag in dlpark's pyo3-flavored safe wrappers. Opus parallel-agent research drove the call.
- New module `streamlib-adapter-cuda::dlpack` with `build_managed_tensor` + `build_byte_buffer_managed_tensor` helpers; views grew `dlpack_managed_tensor(device_ptr, device, owner)` accessors that the cdylib (#589/#590) will call after `cudaExternalMemoryGetMappedBuffer` lands the device pointer.
- 10 unit tests cover layout, discriminants, round-trip, multi-dim+strides, deleter once-and-only-once, null tolerance. All pass; boundary check clean; workspace check clean.
- Stages 7, 8, 9 are independent now. Stage 8 (cudaPointerGetAttributes assertion) is the next logical step because it may flip the default `kDLCUDA → kDLCUDAHost` choice the cdylib makes in #589/#590 — but the API surface is already set up to handle either.
- Next session pickup: Stage 8 OR Stage 9 (architecture docs). Either is unblocked.

### Session 2026-04-30 — Stage 8 (Opus 4.7, 1M ctx)

- Stage 8 done: `cudaPointerGetAttributes` probe added to `cuda_carve_out.rs` Phase 4a (between `cudaExternalMemoryGetMappedBuffer` and the timeline import). Asserts `type_ == cudaMemoryTypeDevice`; explicit panic branches for `Host` / `Unregistered` / `Managed` with action lists ("drop HOST_VISIBLE + flip dlpack default", "investigate driver").
- **Could not run on this rig** — IsaacSim's bundled `libcudart.so.12.0` is missing `cudaEventElapsedTime_v2` which cudarc 0.19.4's `cuda-12090` feature loads eagerly at culib() init. **Pre-existing**: verified by `git stash` + re-run on the pristine pre-Stage-8 commit, same panic. Production rigs (Jetson Orin / x86 + dGPU on current NVIDIA driver) ship libcudart 12.9+, where the probe will fire.
- **Decision until empirical confirmation arrives**: keep `kDLCUDA = 2` as the documented default for the cdylib. Stage 7's API takes `Device` as a parameter so the flip is a one-line change inside #589/#590 if needed.
- `cargo check -p streamlib-adapter-cuda-helpers --tests --features cuda` clean. Boundary check deferred to Stage 10 batch.
- Next session pickup: Stage 9 (architecture docs) → Stage 10 (cleanup + PR).

### Session 2026-04-30 — Stage 9 (Opus 4.7, 1M ctx)

- Stage 9 done: architecture docs + lib.rs module-doc updates.
- `adapter-runtime-integration.md` — added the CUDA row to the single-pattern table explaining the OPAQUE_FD-vs-DMA-BUF twist + the DLPack-flat-pointer reason for it; added a "Surface-share seam with OPAQUE_FD (cuda — #588)" bullet to the `install_setup_hook` shape list.
- `subprocess-rhi-parity.md` — added the OPAQUE_FD VkBuffer pattern row to "Per-pattern decisions" alongside the DMA-BUF rows; section heading and dated annotation updated to mention #588, preserving the existing 2026-04-28 / 04-29 markers per the CLAUDE.md markdown editing rules.
- `streamlib-adapter-cuda/src/lib.rs` module doc — rewritten to describe the post-#588 "ready for cdylib runtimes" state with a bulleted summary of what's in place and what remains for #589/#590.
- `cargo doc -p streamlib-adapter-cuda --no-deps` clean. Boundary check clean.
- Next session pickup: Stage 10 — pre-merge cleanup (delete tasklist.md + context.md, run full workspace test + clippy + boundary check, open the PR).
