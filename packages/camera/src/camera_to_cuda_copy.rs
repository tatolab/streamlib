// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → CUDA host-pipeline copy processor (Linux only).
//!
//! Drops the CPU staging hop for any consumer that wants the camera
//! frame as a GPU-resident CUDA tensor (e.g. PyTorch / JAX inference,
//! cuDNN, OptiX). The host allocates a DEVICE_LOCAL OPAQUE_FD
//! `VkBuffer` for the cuda surface (instead of a HOST_VISIBLE one),
//! and this processor sits between the camera and downstream
//! consumers in the DAG, issuing a per-frame `vkCmdCopyImageToBuffer`
//! from the camera's ring `VkImage` into the cuda `VkBuffer` and
//! signaling the `produce_done` timeline GPU-side. Subprocess
//! `acquire_read` waits on the same timeline value, so consumer
//! Python / Deno inference reads GPU-resident bytes with zero CPU
//! copies on the cuda path.
//!
//! The processor owns the cuda OPAQUE_FD VkBuffer + the exportable
//! timeline semaphores + the surface-share registration + the cached
//! per-frame copy recorder, all created in `setup()` and torn down
//! when the processor's `Drop` fires. There is no setup-hook variant —
//! the lifecycle binding to a single processor instance is what
//! guarantees the cuda surface never outlives its producer.
//!
//! Linux-only. CUDA is Linux-only on the in-tree adapter set;
//! macOS / iOS builds compile a no-op stub that returns a
//! configuration error from `setup()`.

use std::sync::atomic::{AtomicU64, Ordering};
use streamlib_plugin_sdk::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib_plugin_sdk::sdk::error::{Error, Result};

use crate::_generated_::VideoFrame;

#[cfg(target_os = "linux")]
use streamlib_plugin_sdk::sdk::context::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use streamlib_plugin_sdk::sdk::rhi::{
    HostTimelineSemaphore, ImageCopyRegion, PixelBuffer, PixelFormat, RhiCommandRecorder,
    StorageBuffer, VulkanAccess, VulkanLayout, VulkanStage,
};

/// Surface id this processor registers the cuda OPAQUE_FD `VkBuffer`
/// under. Downstream Python / Deno consumers read it under the same
/// id; the consumer's config pins to this constant so the wiring is
/// fixed across the IPC boundary. Raw `u64` is the surface-id ABI type.
pub const CUDA_CAMERA_SURFACE_ID: u64 = 484_001;

/// Default cuda buffer dimensions applied when the consumer's
/// `ProcessorSpec` config omits `width` / `height`. Matches the
/// camera processor's default capture resolution; consumers that
/// run at a non-default resolution must pass `{"width": N, "height": M}`
/// matching their camera config.
#[cfg(target_os = "linux")]
const DEFAULT_SURFACE_WIDTH: u32 = 1920;
#[cfg(target_os = "linux")]
const DEFAULT_SURFACE_HEIGHT: u32 = 1080;

// Per-platform backend stash. The proc-macro strips `#[cfg]` from
// individual struct fields, so we collapse the platform-conditional
// type into a single alias and instantiate against the right impl
// at setup time.
#[cfg(target_os = "linux")]
type GpuBackendStash = Option<LinuxState>;
#[cfg(not(target_os = "linux"))]
type GpuBackendStash = ();

#[cfg(target_os = "linux")]
struct LinuxState {
    /// Cuda OPAQUE_FD DEVICE_LOCAL `VkBuffer` the per-frame copy fills.
    storage_buffer: StorageBuffer,
    /// The same allocation viewed as a `PixelBuffer` for the surface-store
    /// `register_pixel_buffer_with_timeline` producer path. Held as a lifetime
    /// anchor after registration (never read again), so the exported OPAQUE_FD
    /// the surface-share daemon duped stays bound to a live allocation.
    #[allow(dead_code)]
    pixel_buffer: PixelBuffer,
    /// Single-writer-per-edge exportable timelines. Both MUST live for the
    /// processor's life: `register_pixel_buffer_with_timeline` only borrows
    /// them and exports each one's OPAQUE_FD once — dropping either drops the
    /// host `VkSemaphore` refcount to zero and a subprocess consumer blocks
    /// forever on it. See `docs/architecture/adapter-timeline-single-writer.md`.
    /// `produce_done` is advanced per frame; `consume_done` is a lifetime
    /// anchor (the host GPU-waits it on the consumer's edge, never this side).
    produce_done: HostTimelineSemaphore,
    #[allow(dead_code)]
    consume_done: HostTimelineSemaphore,
    /// Cached per-frame `vkCmdCopyImageToBuffer` recorder — driven scope-free
    /// in `process()` (no re-escalation; escalate_end would `wait_device_idle`
    /// every frame).
    copy_recorder: RhiCommandRecorder,
    /// Monotonic `produce_done` signal value, advanced once per submitted copy.
    produce_signal_value: u64,
    /// Hot-path-cached so `process()` doesn't clone the limited-access
    /// GpuContext every frame.
    gpu_ctx: GpuContextLimitedAccess,
    width: u32,
    height: u32,
}

// `CameraToCudaCopyConfig` is codegen'd from
// `schemas/camera_to_cuda_copy_config.yaml` into `crate::_generated_`
// — the macro pulls it via the `config:` block declared in the
// package's `streamlib.yaml`. The `config` struct field is bound to
// `Self::Config` (the schema-derived type), so JSON payloads passed
// via `ProcessorSpec::new(..., serde_json::json!({...}))` reach the
// `setup()` body through `self.config.width` / `.height` as
// expected. (Hand-defining the struct here as a "custom field"
// would silently discard the JSON — the macro initializes custom
// fields via `Default::default()`. Schemas-driven config is the
// only path that flows external config through to the processor.)
#[streamlib_plugin_sdk::sdk::processor("CameraToCudaCopy")]
pub struct CameraToCudaCopyProcessor {
    backend: GpuBackendStash,
    frame_count: AtomicU64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for CameraToCudaCopyProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.setup_inner(ctx)
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "CameraToCudaCopy: shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;
        self.process_frame_inner(&frame)?;
        // Forward the frame downstream verbatim — downstream consumers
        // still need the camera surface_id for any side-channel work
        // (e.g. ModernGL background texture upload); the cuda surface_id
        // travels via consumer config, not via the VideoFrame wire.
        self.outputs.write("video_out", &frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl CameraToCudaCopyProcessor::Processor {
    #[cfg(target_os = "linux")]
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let width = self.config.width.unwrap_or(DEFAULT_SURFACE_WIDTH);
        let height = self.config.height.unwrap_or(DEFAULT_SURFACE_HEIGHT);
        if width == 0 || height == 0 {
            return Err(Error::Configuration(format!(
                "CameraToCudaCopy: width/height must be non-zero, got {width}x{height}"
            )));
        }

        // setup() runs inside the host's privileged lifecycle dispatch, so
        // `ctx.gpu_full_access()` is already escalated. Calling
        // `gpu_limited_access().escalate(...)` here would re-enter the same
        // gate and be rejected; per-frame work instead runs scope-free on the
        // cached `copy_recorder` (an `escalate` per frame would
        // `wait_device_idle` on every scope exit).
        let full = ctx.gpu_full_access();

        // DEVICE_LOCAL OPAQUE_FD VkBuffer. CPU never touches the bytes; the
        // per-frame GPU copy populates them and the cdylib consumer's
        // `cudaImportExternalMemory` → `cudaExternalMemoryGetMappedBuffer`
        // exposes a `kDLCUDA`-classified device pointer to PyTorch / JAX.
        let storage_buffer =
            full.create_opaque_fd_export_buffer((width as u64) * (height as u64) * 4, true)?;

        // View the flat OPAQUE_FD allocation as a PixelBuffer so it registers
        // through the surface-store pixel-buffer-with-timeline producer path.
        let pixel_buffer = full.wrap_storage_buffer_as_pixel_buffer(
            &storage_buffer,
            width,
            height,
            4,
            PixelFormat::Bgra32,
        )?;

        // One exportable timeline per single-writer edge; the cdylib imports
        // them as CUDA timeline external semaphores so `acquire_read` blocks
        // on the producer's GPU signal (`produce_done`) and the host's next
        // write waits on the consumer's signal (`consume_done`).
        let produce_done = full.create_exportable_timeline_semaphore(0)?;
        let consume_done = full.create_exportable_timeline_semaphore(0)?;

        // Surface-share registration so subprocess customers can `check_out`
        // the OPAQUE_FD memory + timelines in one round trip. The host always
        // writes a value; branch on the null-handle sentinel.
        let surface_store = full.surface_store();
        if surface_store.is_none() {
            return Err(Error::Configuration(
                "CameraToCudaCopy: GpuContext has no surface_store (Linux runtime?)".into(),
            ));
        }
        let surface_key = CUDA_CAMERA_SURFACE_ID.to_string();
        surface_store.register_pixel_buffer_with_timeline(
            &surface_key,
            &pixel_buffer,
            Some(&produce_done),
            Some(&consume_done),
        )?;

        let copy_recorder = full.create_command_recorder("camera_to_cuda_copy")?;

        self.backend = Some(LinuxState {
            storage_buffer,
            pixel_buffer,
            produce_done,
            consume_done,
            copy_recorder,
            produce_signal_value: 0,
            gpu_ctx: ctx.gpu_limited_access().clone(),
            width,
            height,
        });

        tracing::info!(
            "CameraToCudaCopy: registered cuda OPAQUE_FD DEVICE_LOCAL surface_id={} ({}x{}) — \
             host pipeline will copy camera→cuda per frame",
            CUDA_CAMERA_SURFACE_ID,
            width,
            height
        );
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn setup_inner(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Configuration(
            "CameraToCudaCopy: only supported on Linux (cuda is Linux-only)".into(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn process_frame_inner(&mut self, frame: &VideoFrame) -> Result<()> {
        let backend = self.backend.as_mut().ok_or_else(|| {
            Error::Configuration("CameraToCudaCopy: backend not initialized".into())
        })?;

        // Resolve the camera ring texture. The camera processor produces
        // RGBA8 ring textures registered under fresh UUIDs and rotates through
        // them; `frame.surface_id` carries the current ring slot's UUID.
        let texture = backend.gpu_ctx.resolve_texture_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;

        let cam_w = texture.width();
        let cam_h = texture.height();
        if cam_w != backend.width || cam_h != backend.height {
            return Err(Error::Configuration(format!(
                "CameraToCudaCopy: camera resolution {cam_w}x{cam_h} differs from \
                 cuda surface {}x{}; GPU-side resize is not implemented in this processor \
                 (the cuda buffer is a flat VkBuffer, not a VkImage, so vkCmdBlitImage \
                 cannot target it). Configure the camera at the same resolution.",
                backend.width, backend.height
            )));
        }

        backend.produce_signal_value += 1;
        let signal_value = backend.produce_signal_value;
        let region = ImageCopyRegion::tightly_packed(backend.width, backend.height);
        let recorder = &mut backend.copy_recorder;

        // The camera leaves ring textures in `GENERAL`. Transition to
        // TRANSFER_SRC for the copy, then RESTORE to GENERAL — the camera
        // processor and every downstream sampler keep GENERAL as their layout
        // invariant across the copy window, so omitting the restore strands the
        // ring slot in TRANSFER_SRC_OPTIMAL and breaks the next sampler.
        recorder.begin()?;
        recorder.record_image_barrier(
            &texture,
            VulkanLayout::GENERAL,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_READ,
        )?;
        recorder.record_copy_image_to_buffer(
            &texture,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            &backend.storage_buffer,
            region,
        )?;
        recorder.record_image_barrier(
            &texture,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanLayout::GENERAL,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_READ,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        )?;
        // Signal `produce_done` on GPU-queue completion (single-writer edge) —
        // the cross-process consumer's `acquire_read` unblocks off this value.
        // No `consume_done` wait on the write path: the camera drops ticks when
        // the consumer skips a frame, and a GPU-wait on a stale consume edge
        // would deadlock the producer against a consumer that never read.
        recorder.submit_signaling_timeline(&backend.produce_done, signal_value)?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn process_frame_inner(&mut self, _frame: &VideoFrame) -> Result<()> {
        Err(Error::Configuration(
            "CameraToCudaCopy: only supported on Linux".into(),
        ))
    }
}
