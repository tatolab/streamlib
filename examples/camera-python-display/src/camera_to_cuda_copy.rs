// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → CUDA host-pipeline copy processor (Linux, #612).
//!
//! Drops the CPU staging hop in `AvatarCharacter`'s Linux inference
//! path. The host allocates a DEVICE_LOCAL OPAQUE_FD `VkBuffer` for
//! the cuda surface (instead of a HOST_VISIBLE one), and this
//! processor sits between the camera and the avatar in the DAG,
//! issuing a per-frame `vkCmdCopyImageToBuffer` from the camera's
//! ring `VkImage` into the cuda `VkBuffer` and signaling the cuda
//! adapter's timeline GPU-side. Subprocess `acquire_read` waits on
//! the same timeline value, so AvatarCharacter Python's inference
//! reads GPU-resident bytes with zero CPU copies in the cuda path.
//!
//! This processor takes over the cuda-surface lifecycle that used
//! to live in `linux.rs::register_cuda_camera_surface`.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use streamlib::core::{
    Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamError,
};
use streamlib::Videoframe;

#[cfg(target_os = "linux")]
use streamlib::core::rhi::{PixelFormat, RhiPixelBuffer};
#[cfg(target_os = "linux")]
use streamlib::core::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use streamlib::host_rhi::{HostMarker, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore};
#[cfg(target_os = "linux")]
use streamlib_adapter_abi::{AdapterError, SurfaceId};
#[cfg(target_os = "linux")]
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};

/// Surface id this processor registers the cuda OPAQUE_FD `VkBuffer`
/// under. AvatarCharacter Python's `_process_linux` reads it under
/// the same id; the Rust example main wires the avatar's Python
/// config to this constant.
#[cfg(target_os = "linux")]
pub const CUDA_CAMERA_SURFACE_ID: SurfaceId = 484_001;

#[cfg(not(target_os = "linux"))]
pub const CUDA_CAMERA_SURFACE_ID: u64 = 484_001;

const SURFACE_WIDTH: u32 = 1920;
const SURFACE_HEIGHT: u32 = 1080;

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
    adapter: Arc<CudaSurfaceAdapter<streamlib::host_rhi::HostVulkanDevice>>,
    surface_id: SurfaceId,
    /// Hot-path-cached so `process()` doesn't go through `Arc::clone`
    /// on the limited-access GpuContext every frame.
    gpu_ctx: GpuContextLimitedAccess,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraToCudaCopyConfig {
    /// Buffer width in pixels. Must match the camera ring texture
    /// width; mismatched sizes are rejected at the first copy.
    pub width: u32,
    /// Buffer height in pixels. Same constraint as `width`.
    pub height: u32,
}

impl Default for CameraToCudaCopyConfig {
    fn default() -> Self {
        Self {
            width: SURFACE_WIDTH,
            height: SURFACE_HEIGHT,
        }
    }
}

#[streamlib::processor("com.tatolab.camera_to_cuda_copy")]
pub struct CameraToCudaCopyProcessor {
    config: CameraToCudaCopyConfig,
    backend: GpuBackendStash,
    frame_count: AtomicU64,
}

impl streamlib::core::ReactiveProcessor for CameraToCudaCopyProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = self.setup_inner(ctx);
        std::future::ready(result)
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "CameraToCudaCopy: shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;
        self.process_frame_inner(&frame)?;
        // Forward the frame downstream verbatim — AvatarCharacter still
        // needs the camera surface_id for its ModernGL background
        // texture upload (handled separately, not via this processor).
        // The cuda surface_id is in AvatarCharacter's config.
        self.outputs.write("video_out", &frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl CameraToCudaCopyProcessor::Processor {
    #[cfg(target_os = "linux")]
    fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let width = self.config.width;
        let height = self.config.height;
        if width == 0 || height == 0 {
            return Err(StreamError::Configuration(format!(
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
        //    `kDLCUDA`-classified device pointer to PyTorch.
        let pixel_buffer = HostVulkanPixelBuffer::new_opaque_fd_export_device_local(
            &host_device,
            width,
            height,
            4,
            PixelFormat::Bgra32,
        )
        .map_err(|e| {
            StreamError::Configuration(format!(
                "CameraToCudaCopy: new_opaque_fd_export_device_local: {e}"
            ))
        })?;
        let pixel_buffer_arc = Arc::new(pixel_buffer);
        let pixel_buffer_rhi =
            RhiPixelBuffer::from_host_vulkan_pixel_buffer(Arc::clone(&pixel_buffer_arc));

        // 2. Exportable timeline. The cdylib imports it as a CUDA
        //    timeline external semaphore so `acquire_read` blocks
        //    on the GPU signal this processor emits per frame.
        let timeline = Arc::new(
            HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0).map_err(|e| {
                StreamError::Configuration(format!(
                    "CameraToCudaCopy: HostVulkanTimelineSemaphore::new_exportable: {e}"
                ))
            })?,
        );

        // 3. Surface-share registration so subprocess customers can
        //    `check_out` the OPAQUE_FD memory + timeline in one round
        //    trip.
        let surface_store = gpu_full.surface_store().ok_or_else(|| {
            StreamError::Configuration(
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
                StreamError::Configuration(format!(
                    "CameraToCudaCopy: register_pixel_buffer_with_timeline: {e}"
                ))
            })?;

        // 4. Cuda adapter — owns the registration's `Arc`s and runs
        //    the timeline-wait protocol on per-acquire from the
        //    cdylib customer.
        let adapter: Arc<CudaSurfaceAdapter<streamlib::host_rhi::HostVulkanDevice>> = Arc::new(
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
                StreamError::Configuration(format!(
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
        Err(StreamError::Configuration(
            "CameraToCudaCopy: only supported on Linux (cuda is Linux-only)".into(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn process_frame_inner(&mut self, frame: &Videoframe) -> Result<()> {
        let backend = self.backend.as_ref().ok_or_else(|| {
            StreamError::Configuration("CameraToCudaCopy: backend not initialized".into())
        })?;

        // Resolve the camera ring texture. The camera processor
        // produces RGBA8 ring textures registered under fresh UUIDs
        // and rotates through them; `frame.surface_id` carries the
        // current ring slot's UUID.
        let texture = backend.gpu_ctx.resolve_videoframe_texture(frame).map_err(|e| {
            StreamError::Configuration(format!(
                "CameraToCudaCopy: resolve_videoframe_texture('{}'): {e}",
                frame.surface_id
            ))
        })?;
        let host_texture = texture.vulkan_inner();
        let cam_w = host_texture.width();
        let cam_h = host_texture.height();
        if cam_w != backend.width || cam_h != backend.height {
            return Err(StreamError::Configuration(format!(
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
        // `WriteContended` is the expected race when AvatarCharacter
        // Python's `cuda.acquire_read` is mid-YOLO-inference (~30 ms
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
                return Err(StreamError::Configuration(format!(
                    "CameraToCudaCopy: submit_host_copy_image_to_buffer: {e:?}"
                )));
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn process_frame_inner(&mut self, _frame: &Videoframe) -> Result<()> {
        Err(StreamError::Configuration(
            "CameraToCudaCopy: only supported on Linux".into(),
        ))
    }
}
