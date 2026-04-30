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
  - [ ] #589 body updated (DMA-BUF→OPAQUE_FD wording, strike VkImage).
  - [ ] #590 body updated (same).

- [ ] **Stage 2** — `RhiExternalHandle::OpaqueFd` variant.
  - [ ] Add `OpaqueFd { fd: RawFd, size: u64 }` variant to `libs/streamlib/src/core/rhi/external_handle.rs`.
  - [ ] Update every closed match arm (`grep RhiExternalHandle::DmaBuf` for the ripple list).
  - [ ] `tracing::instrument` per CLAUDE.md.
  - [ ] Unit tests for FD lifecycle + debug repr.

- [ ] **Stage 3** — Host RHI OPAQUE_FD export through `RhiPixelBufferExport`.
  - [ ] Decide trait shape (extend `export_handle()` vs add `export_opaque_fd_handle()`) — record in `context.md`.
  - [ ] Wire `HostVulkanPixelBuffer::export_opaque_fd_memory` (already exists from #587) through the new path.
  - [ ] Conformance test.

- [ ] **Stage 4** — Surface-share wire format extension.
  - [ ] Add `handle_type` discriminator on register/lookup JSON (additive, defaults to `"dma_buf"`).
  - [ ] `SurfaceStore::register_pixel_buffer_opaque_fd_with_timeline` (or extend existing API).
  - [ ] Client-side `lookup_*` returns OPAQUE_FD when registered that way.
  - [ ] Round-trip test extending `check_in_check_out_multi_fd_roundtrip`.

- [ ] **Stage 5** — `streamlib-consumer-rhi` OPAQUE_FD import.
  - [ ] `ConsumerVulkanDevice::import_opaque_fd_memory(fd, size)`.
  - [ ] `ConsumerVulkanPixelBuffer::from_opaque_fd(device, fd, size, …)`.
  - [ ] Conformance test in the consumer-rhi suite.
  - [ ] Update `consumer-rhi::lib.rs` doc.

- [ ] **Stage 6** — End-to-end integration test (no `cudarc`).
  - [ ] Cross-crate test in `streamlib-adapter-cuda-helpers/tests/`.
  - [ ] Full chain: host export → surface-share wire → consumer-rhi import → `CudaSurfaceAdapter<ConsumerVulkanDevice>` acquire.

- [ ] **Stage 7** — DLPack capsule on `CudaReadView` / `CudaWriteView`.
  - [ ] `dlpark` vs hand-rolled decision in `context.md`.
  - [ ] `CudaReadView::dlpack(&self) -> *mut DLManagedTensor`.
  - [ ] Layout regression test.

- [ ] **Stage 8** — Empirical CUDA-side verification.
  - [ ] Extend `cuda_carve_out.rs` with `cudaPointerGetAttributes` assertion.
  - [ ] If `cudaMemoryTypeHost`: drop HOST_VISIBLE on staging buffer; record decision.

- [ ] **Stage 9** — Documentation.
  - [ ] `docs/architecture/adapter-runtime-integration.md` — CUDA row.
  - [ ] `docs/architecture/subprocess-rhi-parity.md` — OPAQUE_FD pattern row.
  - [ ] `streamlib-adapter-cuda::lib.rs` module doc — ready-for-cdylib state.

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

- Stage 1 partial done: comprehensive issue body shipped to #588, branch + scratchpads created.
- Three Opus agents at max reasoning ran the staleness/correctness/gap audits. Findings consolidated in `context.md`.
- Open: #589 + #590 body rewrites; Stage 2.
