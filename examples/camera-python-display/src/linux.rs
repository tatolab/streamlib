// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux path for camera-python-display (#484 AvatarCharacter).
//!
//! First in-tree consumer of TWO surface adapters against the same
//! camera-frame production lifecycle inside a single subprocess
//! processor. Wires:
//!
//! - `streamlib-adapter-cuda` — pre-registers a HOST_VISIBLE OPAQUE_FD
//!   `VkBuffer` + exportable timeline so the AvatarCharacter Python
//!   processor can `acquire_write` the camera frame and `acquire_read`
//!   it as a DLPack tensor for PyTorch pose detection.
//! - `streamlib-adapter-opengl` — pre-registers a render-target-capable
//!   tiled DMA-BUF `VkImage` so AvatarCharacter can `acquire_write` it
//!   and ModernGL renders the skinned mesh into it. The display
//!   processor consumes the same DMA-BUF surface UUID downstream.
//!
//! Pipeline shape (#485 / #486 will extend the right side):
//!
//! ```text
//!   Camera ──→ AvatarCharacter ──→ Display
//!              (cuda + opengl)
//! ```
//!
//! See `docs/architecture/adapter-runtime-integration.md` for the
//! single-pattern principle these adapters ride and
//! `docs/architecture/subprocess-rhi-parity.md` for the carve-out the
//! cdylib's consumer-rhi import path stays inside.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::{PixelFormat, RhiPixelBuffer, TextureFormat};
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::host_rhi::{HostMarker, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore};
use streamlib::{
    CameraProcessor, DisplayProcessor, ProcessorSpec, Result, StreamRuntime,
};
use streamlib_adapter_abi::SurfaceId;
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};

/// Surface ID for the camera-input cuda OPAQUE_FD buffer.
///
/// Numerically distinct from the surface UUIDs used by sibling polyglot
/// scenarios so multiple runtimes in the same process can coexist (none
/// today, but the cost is one constant).
const AVATAR_CAMERA_CUDA_SURFACE_ID: SurfaceId = 484_001;

/// Surface UUID for the avatar mesh-render output (tiled DMA-BUF
/// `VkImage`). The Python processor renders into it via
/// `OpenGLContext.acquire_write`; the display processor reads it
/// downstream via the standard surface-share lookup.
const AVATAR_OUTPUT_SURFACE_UUID: &str = "00000000-0000-0000-0000-000000000484";

/// Pin everything to 1920x1080 for the first iteration. The Linux
/// camera processor's default capture resolution and the host's
/// pre-allocated cuda + opengl surfaces all use this size.
const SURFACE_WIDTH: u32 = 1920;
const SURFACE_HEIGHT: u32 = 1080;
const BYTES_PER_PIXEL: u32 = 4;

type HostAdapter = CudaSurfaceAdapter<streamlib::host_rhi::HostVulkanDevice>;

pub fn main() -> Result<()> {
    println!("=== AvatarCharacter (Linux, #484 cuda + opengl adapters) ===\n");

    let runtime = StreamRuntime::new()?;
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");

    // Load processor package from streamlib.yaml. The Python processors
    // (avatar_character, cyberpunk_*) compile-import their adapter
    // dependencies; the unused ones (#485/#486) just sit dormant until
    // we add their pipeline edges.
    runtime.load_project(&project_path)?;
    println!("✓ Loaded processor package from streamlib.yaml\n");

    // Slot keeping the cuda adapter `Arc` alive for the runtime's
    // start→stop window. The CUDA adapter has no per-acquire host work
    // (#588 / `subprocess-rhi-parity.md`) — keeping the adapter alive
    // is about preserving the OPAQUE_FD `VkBuffer` + timeline `Arc`s
    // surface-share's daemon dup'd from at registration time.
    let cuda_adapter_slot: Arc<Mutex<Option<Arc<HostAdapter>>>> =
        Arc::new(Mutex::new(None));

    {
        let cuda_adapter_slot = Arc::clone(&cuda_adapter_slot);
        runtime.install_setup_hook(move |gpu| {
            // 1. CUDA OPAQUE_FD camera-input surface ----------------------
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let adapter: Arc<HostAdapter> =
                Arc::new(CudaSurfaceAdapter::new(Arc::clone(&host_device)));
            register_cuda_camera_surface(&adapter, gpu).map_err(|e| {
                StreamError::Configuration(format!(
                    "register_cuda_camera_surface: {e}"
                ))
            })?;
            *cuda_adapter_slot.lock().unwrap() = Some(adapter);

            // 2. OpenGL DMA-BUF mesh-render output surface ----------------
            register_opengl_output_surface(gpu).map_err(|e| {
                StreamError::Configuration(format!(
                    "register_opengl_output_surface: {e}"
                ))
            })?;

            println!(
                "✓ AvatarCharacter setup hooks installed: \
                 cuda OPAQUE_FD camera surface_id={AVATAR_CAMERA_CUDA_SURFACE_ID}, \
                 opengl DMA-BUF output uuid={AVATAR_OUTPUT_SURFACE_UUID}"
            );
            Ok(())
        });
    }

    // Camera processor (V4L2 on Linux). The camera config doesn't
    // expose width/height — the camera processor picks based on the
    // device's supported formats. The Python processor resizes
    // incoming camera bytes to the pre-registered host surface
    // dimensions.
    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("✓ Camera added: {camera}\n");

    // AvatarCharacter (Python subprocess). Reads camera frame, copies
    // bytes into the cuda OPAQUE_FD surface, runs PyTorch pose
    // detection, renders skinned mesh into the opengl DMA-BUF surface,
    // emits the surface UUID downstream.
    println!("🐍 Adding Python avatar character (subprocess, PyTorch pose + ModernGL)...");
    let avatar = runtime.add_processor(ProcessorSpec::new(
        "com.tatolab.avatar_character",
        serde_json::json!({
            "cuda_camera_surface_id": AVATAR_CAMERA_CUDA_SURFACE_ID,
            "opengl_output_surface_uuid": AVATAR_OUTPUT_SURFACE_UUID,
            "width": SURFACE_WIDTH,
            "height": SURFACE_HEIGHT,
            "channels": BYTES_PER_PIXEL,
        }),
    ))?;
    println!("✓ Avatar character processor added: {avatar}\n");

    // Display processor (Vulkan swapchain).
    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: SURFACE_WIDTH,
        height: SURFACE_HEIGHT,
        title: Some("AvatarCharacter Linux (#484)".to_string()),
        scaling_mode: Default::default(),
        vsync: Some(true),
        ..Default::default()
    }))?;
    println!("✓ Display added: {display}\n");

    // Wire camera → avatar → display. The full Breaking-News-PiP
    // pipeline (compositor + CRT + glitch + lower third + watermark)
    // is gated on #485/#486 landing the remaining Linux Python ports.
    println!("🔗 Connecting pipeline...");
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&avatar, "video_in"),
    )?;
    println!("   ✓ Camera → AvatarCharacter");
    runtime.connect(
        OutputLinkPortRef::new(&avatar, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ AvatarCharacter → Display\n");

    println!("▶️  Starting pipeline...");
    println!("   Architecture (Linux, #484):");
    println!("     Camera ──→ AvatarCharacter ──→ Display");
    println!("                (cuda OPAQUE_FD + opengl DMA-BUF)");
    println!();
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");
    Ok(())
}

/// Allocate the OPAQUE_FD HOST_VISIBLE staging `VkBuffer` for the
/// camera frame, allocate an exportable timeline semaphore, register
/// the pair via surface-share so the subprocess can import them in one
/// `check_out`, and register the result with the cuda adapter under
/// [`AVATAR_CAMERA_CUDA_SURFACE_ID`].
fn register_cuda_camera_surface(
    adapter: &Arc<HostAdapter>,
    gpu: &GpuContext,
) -> std::result::Result<(), String> {
    let host_device = adapter.device();
    let buffer_size = (SURFACE_WIDTH * SURFACE_HEIGHT * BYTES_PER_PIXEL) as usize;

    // OPAQUE_FD (not DMA-BUF) is required because DLPack consumers
    // (`torch.from_dlpack`) need a flat `void*` device pointer from
    // `cudaExternalMemoryGetMappedBuffer`, which only works when the
    // source memory is a `VkBuffer` exported as OPAQUE_FD. See
    // `docs/architecture/subprocess-rhi-parity.md` →
    // "OPAQUE_FD VkBuffer import (cuda — #588)".
    let pixel_buffer = HostVulkanPixelBuffer::new_opaque_fd_export(
        host_device,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
        BYTES_PER_PIXEL,
        PixelFormat::Bgra32,
    )
    .map_err(|e| format!("HostVulkanPixelBuffer::new_opaque_fd_export: {e}"))?;
    let pixel_buffer_arc = Arc::new(pixel_buffer);
    let pixel_buffer_rhi =
        RhiPixelBuffer::from_host_vulkan_pixel_buffer(Arc::clone(&pixel_buffer_arc));

    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .map_err(|e| {
                format!("HostVulkanTimelineSemaphore::new_exportable: {e}")
            })?,
    );

    // Pre-fill the buffer with zeros so a deterministic baseline
    // exists if the subprocess `acquire_read` ever fires before any
    // `acquire_write` upload. SAFETY: the OPAQUE_FD buffer is
    // HOST_VISIBLE | HOST_COHERENT and the mapped pointer stays valid
    // for the buffer's lifetime; no other owner has a handle yet —
    // surface-share registration is what publishes it to the daemon.
    unsafe {
        std::ptr::write_bytes(pixel_buffer_arc.mapped_ptr(), 0u8, buffer_size);
    }

    let surface_store = gpu
        .surface_store()
        .ok_or_else(|| "GpuContext has no surface_store".to_string())?;
    surface_store
        .register_pixel_buffer_with_timeline(
            &AVATAR_CAMERA_CUDA_SURFACE_ID.to_string(),
            &pixel_buffer_rhi,
            Some(timeline.as_ref()),
        )
        .map_err(|e| format!("register_pixel_buffer_with_timeline: {e}"))?;

    adapter
        .register_host_surface(
            AVATAR_CAMERA_CUDA_SURFACE_ID,
            HostSurfaceRegistration::<HostMarker> {
                pixel_buffer: pixel_buffer_arc,
                timeline,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .map_err(|e| format!("register_host_surface: {e:?}"))?;

    Ok(())
}

/// Allocate a render-target-capable tiled DMA-BUF `VkImage` for the
/// avatar mesh-render output and register it with the surface-share
/// service under [`AVATAR_OUTPUT_SURFACE_UUID`]. The opengl adapter
/// imports it subprocess-side as an `EGLImage` + `GL_TEXTURE_2D`; the
/// display processor reads it via the same UUID downstream.
fn register_opengl_output_surface(
    gpu: &GpuContext,
) -> std::result::Result<(), String> {
    // `acquire_render_target_dma_buf_image` picks a tiled DRM modifier
    // — required on NVIDIA where linear DMA-BUFs are sampler-only when
    // imported through EGL (per
    // `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
    let texture = gpu
        .acquire_render_target_dma_buf_image(
            SURFACE_WIDTH,
            SURFACE_HEIGHT,
            TextureFormat::Bgra8Unorm,
        )
        .map_err(|e| format!("acquire_render_target_dma_buf_image: {e}"))?;

    let surface_store = gpu
        .surface_store()
        .ok_or_else(|| "GpuContext has no surface_store".to_string())?;
    // OpenGL adapter doesn't need an explicit Vulkan timeline:
    // `glFinish` on release plus DMA-BUF kernel-fence semantics carry
    // visibility for downstream consumers.
    surface_store
        .register_texture(AVATAR_OUTPUT_SURFACE_UUID, &texture, None)
        .map_err(|e| format!("register_texture: {e}"))?;

    // Mirror the texture into the GpuContext's local same-process cache
    // so downstream processors (in this case the display) hit Path 1 in
    // `GpuContext::resolve_videoframe_texture` instead of the cross-
    // process daemon lookup. This matches what `LinuxCameraProcessor`
    // does for its own ring textures (see `linux/processors/camera.rs:857`)
    // — without it, same-process consumers can't find the texture by
    // UUID even though surface-share has it.
    gpu.register_texture(AVATAR_OUTPUT_SURFACE_UUID, texture);

    Ok(())
}
