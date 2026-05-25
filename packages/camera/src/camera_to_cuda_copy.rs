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
//! signaling the cuda adapter's timeline GPU-side. Subprocess
//! `acquire_read` waits on the same timeline value, so consumer
//! Python / Deno inference reads GPU-resident bytes with zero CPU
//! copies on the cuda path.
//!
//! The processor owns the cuda OPAQUE_FD VkBuffer + the exportable
//! timeline semaphore + the surface-share registration for both, all
//! created in `setup()` and torn down when the processor's `Drop`
//! fires. There is no setup-hook variant — the lifecycle binding to
//! a single processor instance is what guarantees the cuda surface
//! never outlives its producer.
//!
//! Linux-only. CUDA is Linux-only on the in-tree adapter set;
//! macOS / iOS builds compile a no-op stub that returns a
//! configuration error from `setup()`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::engine::{HostGpuDeviceExt, HostTextureExt};
use streamlib::sdk::error::{Error, Result};
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::HostSurfaceStoreExt;

use crate::_generated_::VideoFrame;

#[cfg(target_os = "linux")]
use streamlib::sdk::context::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use streamlib::sdk::engine::host_rhi::{HostMarker, HostVulkanBuffer, HostVulkanTimelineSemaphore};
#[cfg(target_os = "linux")]
use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};
#[cfg(target_os = "linux")]
use streamlib_adapter_abi::{AdapterError, SurfaceId};
#[cfg(target_os = "linux")]
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration};
// Import VulkanLayout from consumer-rhi directly for consistency with
// the rest of the camera package (`linux/camera.rs` also imports from
// `streamlib_consumer_rhi`). `streamlib_adapter_cuda` re-exports the
// same type — both resolve to the identical newtype — but pinning the
// import to the canonical home avoids future drift if the adapter
// re-export ever diverges.
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::VulkanLayout;

/// Surface id this processor registers the cuda OPAQUE_FD `VkBuffer`
/// under. Downstream Python / Deno consumers read it under the same
/// id; the consumer's config pins to this constant so the wiring is
/// fixed across the IPC boundary.
#[cfg(target_os = "linux")]
pub const CUDA_CAMERA_SURFACE_ID: SurfaceId = 484_001;

#[cfg(not(target_os = "linux"))]
pub const CUDA_CAMERA_SURFACE_ID: u64 = 484_001;

/// Default cuda buffer dimensions applied when the consumer's
/// `ProcessorSpec` config omits `width` / `height`. Matches the
/// camera processor's default capture resolution; consumers that
/// run at a non-default resolution must pass `{"width": N, "height": M}`
/// matching their camera config.
const DEFAULT_SURFACE_WIDTH: u32 = 1920;
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
    /// Owns the cuda OPAQUE_FD `VkBuffer` + exportable timeline; lives
    /// for the processor's runtime window so surface-share's daemon-
    /// duped fds stay valid.
    adapter: Arc<CudaSurfaceAdapter<streamlib::sdk::engine::host_rhi::HostVulkanDevice>>,
    surface_id: SurfaceId,
    /// Hot-path-cached so `process()` doesn't go through `Arc::clone`
    /// on the limited-access GpuContext every frame.
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
#[streamlib::sdk::processor("CameraToCudaCopy")]
pub struct CameraToCudaCopyProcessor {
    backend: GpuBackendStash,
    frame_count: AtomicU64,
}

impl streamlib::sdk::processors::ReactiveProcessor for CameraToCudaCopyProcessor::Processor {
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

        let gpu_full = ctx.gpu_full_access();
        let gpu_lim = ctx.gpu_limited_access().clone();
        let host_device = Arc::clone(gpu_full.device().vulkan_device());

        // 1. DEVICE_LOCAL OPAQUE_FD VkBuffer. CPU never touches the
        //    bytes; the GPU copy in `process()` populates them, and
        //    the cdylib's `cudaImportExternalMemory` →
        //    `cudaExternalMemoryGetMappedBuffer` exposes a
        //    `kDLCUDA`-classified device pointer to PyTorch / JAX.
        let pixel_buffer = HostVulkanBuffer::new_opaque_fd_export_device_local(
            &host_device,
            (width as u64) * (height as u64) * 4,
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "CameraToCudaCopy: new_opaque_fd_export_device_local: {e}"
            ))
        })?;
        let pixel_buffer_arc = Arc::new(pixel_buffer);
        let pixel_buffer_rhi = PixelBuffer::from_host_vulkan_buffer(
            Arc::clone(&pixel_buffer_arc),
            width,
            height,
            4,
            PixelFormat::Bgra32,
        );

        // 2. Exportable timeline. The cdylib imports it as a CUDA
        //    timeline external semaphore so `acquire_read` blocks
        //    on the GPU signal this processor emits per frame.
        let timeline = Arc::new(
            HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0).map_err(|e| {
                Error::Configuration(format!(
                    "CameraToCudaCopy: HostVulkanTimelineSemaphore::new_exportable: {e}"
                ))
            })?,
        );

        // 3. Surface-share registration so subprocess customers can
        //    `check_out` the OPAQUE_FD memory + timeline in one round
        //    trip.
        let surface_store = gpu_full.surface_store().ok_or_else(|| {
            Error::Configuration(
                "CameraToCudaCopy: GpuContext has no surface_store (Linux runtime?)".into(),
            )
        })?;
        let surface_key = CUDA_CAMERA_SURFACE_ID.to_string();
        surface_store
            .register_pixel_buffer_with_timeline(
                &surface_key,
                &pixel_buffer_rhi,
                Some(timeline.as_ref()),
            )
            .map_err(|e| {
                Error::Configuration(format!(
                    "CameraToCudaCopy: register_pixel_buffer_with_timeline: {e}"
                ))
            })?;

        // 4. Cuda adapter — owns the registration's `Arc`s and runs
        //    the timeline-wait protocol on per-acquire from the
        //    cdylib customer.
        let adapter: Arc<CudaSurfaceAdapter<streamlib::sdk::engine::host_rhi::HostVulkanDevice>> = Arc::new(
            CudaSurfaceAdapter::new(Arc::clone(&host_device)),
        );
        adapter
            .register_host_surface(
                CUDA_CAMERA_SURFACE_ID,
                HostSurfaceRegistration::<HostMarker> {
                    pixel_buffer: pixel_buffer_arc,
                    timeline,
                    initial_layout: VulkanLayout::UNDEFINED,
                },
            )
            .map_err(|e| {
                Error::Configuration(format!(
                    "CameraToCudaCopy: register_host_surface: {e:?}"
                ))
            })?;

        self.backend = Some(LinuxState {
            adapter,
            surface_id: CUDA_CAMERA_SURFACE_ID,
            gpu_ctx: gpu_lim,
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
        let backend = self.backend.as_ref().ok_or_else(|| {
            Error::Configuration("CameraToCudaCopy: backend not initialized".into())
        })?;

        // Resolve the camera ring texture. The camera processor
        // produces RGBA8 ring textures registered under fresh UUIDs
        // and rotates through them; `frame.surface_id` carries the
        // current ring slot's UUID.
        let texture = backend
            .gpu_ctx
            .resolve_texture_by_surface_id(
                &frame.surface_id,
                frame.texture_layout,
                frame.width,
                frame.height,
            )
            .map_err(|e| {
            Error::Configuration(format!(
                "CameraToCudaCopy: resolve_texture_by_surface_id('{}'): {e}",
                frame.surface_id
            ))
        })?;
        let host_texture = texture.vulkan_inner();
        let cam_w = host_texture.width();
        let cam_h = host_texture.height();
        if cam_w != backend.width || cam_h != backend.height {
            return Err(Error::Configuration(format!(
                "CameraToCudaCopy: camera resolution {cam_w}x{cam_h} differs from \
                 cuda surface {}x{}; GPU-side resize is not implemented in this processor \
                 (the cuda buffer is a flat VkBuffer, not a VkImage, so vkCmdBlitImage \
                 cannot target it). Configure the camera at the same resolution.",
                backend.width, backend.height
            )));
        }

        // The camera processor leaves ring textures in `GENERAL`
        // layout (storage|sampled). Tell the cuda adapter to copy
        // out and restore the same layout — the camera processor
        // and any downstream sampler keep their layout invariant
        // over the copy window. The adapter handles the VkImage /
        // extent extraction internally so this crate stays out of
        // `vulkanalia` per the engine boundary rule.
        //
        // `WriteContended` is the expected race when a consumer's
        // subprocess `cuda.acquire_read` is mid-inference (~30 ms
        // at 640x640 on Jetson-class GPUs); the camera frame is
        // dropped on this tick and the next ring slot will succeed.
        // Tearing down the pipeline on this error would defeat the
        // whole point of the host pipeline shape.
        match backend.adapter.submit_host_copy_image_to_buffer(
            backend.surface_id,
            host_texture.as_ref(),
            VulkanLayout::GENERAL,
        ) {
            Ok(_) => {}
            Err(AdapterError::WriteContended { .. }) => {
                if self.frame_count.load(Ordering::Relaxed) % 60 == 0 {
                    tracing::debug!(
                        "CameraToCudaCopy: write contended (subprocess reader still \
                         holding the cuda surface) — dropping this camera tick"
                    );
                }
            }
            Err(e) => {
                return Err(Error::Configuration(format!(
                    "CameraToCudaCopy: submit_host_copy_image_to_buffer: {e:?}"
                )));
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn process_frame_inner(&mut self, _frame: &VideoFrame) -> Result<()> {
        Err(Error::Configuration(
            "CameraToCudaCopy: only supported on Linux".into(),
        ))
    }
}
