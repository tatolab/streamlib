// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux path for camera-python-display (#484 AvatarCharacter).
//!
//! Wires two surface adapters around AvatarCharacter:
//!
//! - `streamlib-adapter-cuda` — the camera frame is copied GPU-side into
//!   a DEVICE_LOCAL OPAQUE_FD `VkBuffer` by [`CameraToCudaCopyProcessor`]
//!   (a host-pipeline processor inserted between the camera and avatar)
//!   so AvatarCharacter Python's `_process_linux` can `acquire_read` a
//!   GPU-resident DLPack tensor straight into PyTorch — no CPU staging
//!   round-trip on the inference path. Per #612.
//! - `streamlib-adapter-opengl` — pre-registers a render-target-capable
//!   tiled DMA-BUF `VkImage` so AvatarCharacter can `acquire_write` it
//!   and ModernGL renders the skinned mesh into it. The display
//!   processor consumes the same DMA-BUF surface UUID downstream.
//!
//! Pipeline shape (#485 / #486 will extend the right side):
//!
//! ```text
//!   Camera ──→ CameraToCudaCopy ──→ AvatarCharacter ──→ Display
//!                                  (cuda read + opengl write)
//! ```
//!
//! See `docs/architecture/adapter-runtime-integration.md` for the
//! single-pattern principle these adapters ride and
//! `docs/architecture/subprocess-rhi-parity.md` for the carve-out the
//! cdylib's consumer-rhi import path stays inside.

use std::path::PathBuf;

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::{
    CameraProcessor, DisplayProcessor, ProcessorSpec, Result, StreamRuntime,
};
use streamlib_adapter_abi::SurfaceId;
use streamlib_consumer_rhi::VulkanLayout;

use crate::camera_to_cuda_copy::{CameraToCudaCopyProcessor, CUDA_CAMERA_SURFACE_ID};

/// Re-exported alias so the Python avatar's JSON config and other
/// pipeline wiring keep using the historical name; the processor's
/// own [`CUDA_CAMERA_SURFACE_ID`] is the single source of truth.
const AVATAR_CAMERA_CUDA_SURFACE_ID: SurfaceId = CUDA_CAMERA_SURFACE_ID;

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

    // OpenGL DMA-BUF mesh-render output surface stays as a setup hook
    // (one-shot pre-allocation; no per-frame host work). The cuda
    // surface used to ride a setup hook too — that's now owned by the
    // CameraToCudaCopyProcessor below, which also issues the per-frame
    // GPU-side copy.
    runtime.install_setup_hook(move |gpu| {
        register_opengl_output_surface(gpu).map_err(|e| {
            StreamError::Configuration(format!(
                "register_opengl_output_surface: {e}"
            ))
        })?;
        println!(
            "✓ OpenGL DMA-BUF output surface registered: uuid={AVATAR_OUTPUT_SURFACE_UUID}"
        );
        Ok(())
    });

    // Camera processor (V4L2 on Linux). The camera config doesn't
    // expose width/height — the camera processor picks based on the
    // device's supported formats. The host pipeline expects 1920x1080
    // BGRA-shaped ring textures; mismatched sizes are rejected by the
    // copy processor at the first frame.
    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("✓ Camera added: {camera}\n");

    // Camera → CUDA copy processor (#612). Sits between the camera
    // and avatar in the DAG; allocates the cuda DEVICE_LOCAL OPAQUE_FD
    // VkBuffer, registers it under AVATAR_CAMERA_CUDA_SURFACE_ID, and
    // issues a per-frame vkCmdCopyImageToBuffer + timeline GPU signal.
    // AvatarCharacter Python's `cuda.acquire_read(...)` waits on the
    // same timeline value.
    // Default config matches the camera processor's 1920x1080 output;
    // the cuda surface id is a hardcoded constant on the processor
    // module re-exported here as `AVATAR_CAMERA_CUDA_SURFACE_ID` so
    // the Python config below pins to the same value.
    println!("🚛 Adding camera→cuda copy processor (host-pipeline producer)...");
    let camera_to_cuda =
        runtime.add_processor(CameraToCudaCopyProcessor::node(Default::default()))?;
    println!("✓ Camera→CUDA copy added: {camera_to_cuda}\n");

    // AvatarCharacter (Python subprocess). Reads the cuda surface for
    // GPU-resident YOLO inference, renders skinned mesh into the
    // opengl DMA-BUF surface, emits the surface UUID downstream.
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

    // Wire camera → camera_to_cuda → avatar → display. The full
    // Breaking-News-PiP pipeline (compositor + CRT + glitch + lower
    // third + watermark) is gated on #485/#486 landing the remaining
    // Linux Python ports.
    println!("🔗 Connecting pipeline...");
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&camera_to_cuda, "video_in"),
    )?;
    println!("   ✓ Camera → CameraToCudaCopy");
    runtime.connect(
        OutputLinkPortRef::new(&camera_to_cuda, "video_out"),
        InputLinkPortRef::new(&avatar, "video_in"),
    )?;
    println!("   ✓ CameraToCudaCopy → AvatarCharacter");
    runtime.connect(
        OutputLinkPortRef::new(&avatar, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ AvatarCharacter → Display\n");

    println!("▶️  Starting pipeline...");
    println!("   Architecture (Linux, #484 + #612):");
    println!("     Camera ──→ CameraToCudaCopy ──→ AvatarCharacter ──→ Display");
    println!("                (cuda DEVICE_LOCAL OPAQUE_FD)   (opengl DMA-BUF)");
    println!();
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");
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
    // `GpuContext::resolve_videoframe_registration` instead of the cross-
    // process daemon lookup. This matches what `LinuxCameraProcessor`
    // does for its own ring textures (see `linux/processors/camera.rs:857`)
    // — without it, same-process consumers can't find the texture by
    // UUID even though surface-share has it.
    //
    // Declare `UNDEFINED` as the registration's initial layout: the
    // OpenGL adapter writes to this VkImage via EGL DMA-BUF import and
    // does not transition the Vulkan-side layout (it issues `glFinish`
    // on release; DMA-BUF kernel-fence semantics carry data visibility,
    // but Vulkan's layout tracker stays at the image's `initialLayout`
    // which is `UNDEFINED` from `acquire_render_target_dma_buf_image`).
    // Display's first-frame barrier transitions UNDEFINED →
    // SHADER_READ_ONLY_OPTIMAL — content is technically allowed to be
    // discarded by the spec on this transition but NVIDIA preserves
    // it (verified empirically on RTX 3090). After that first barrier,
    // display's `update_layout` advances the registration to
    // SHADER_READ_ONLY_OPTIMAL; subsequent GL writes don't change the
    // Vulkan tracker, so steady-state barriers are SHADER_READ_ONLY
    // → SHADER_READ_ONLY no-ops.
    gpu.register_texture_with_layout(
        AVATAR_OUTPUT_SURFACE_UUID,
        texture,
        VulkanLayout::UNDEFINED,
    );

    Ok(())
}
