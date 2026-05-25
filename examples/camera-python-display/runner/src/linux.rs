// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux path for camera-python-display (#484 AvatarCharacter, #485
//! Skia-on-Vulkan overlays).
//!
//! Wires three surface adapters across the four Linux Python ports
//! that exist today:
//!
//! - `streamlib-adapter-cuda` — the camera frame is copied GPU-side into
//!   a DEVICE_LOCAL OPAQUE_FD `VkBuffer` by [`CameraToCudaCopyProcessor`]
//!   (a host-pipeline processor inserted between the camera and avatar)
//!   so AvatarCharacter Python's `_process_linux` can `acquire_read` a
//!   GPU-resident DLPack tensor straight into PyTorch — no CPU staging
//!   round-trip on the inference path. Per #612.
//! - `streamlib-adapter-opengl` — pre-registers a render-target-capable
//!   tiled DMA-BUF `VkImage` so AvatarCharacter can `acquire_write` it
//!   and ModernGL renders the skinned mesh into it.
//! - `streamlib-adapter-skia` (#485) — pre-registers two more
//!   render-target-capable tiled DMA-BUF `VkImage`s for the Python Skia
//!   overlays (`CyberpunkLowerThird` and `CyberpunkWatermark`). Skia
//!   composes on the OpenGL adapter via
//!   `skia.GrDirectContext.MakeGL(MakeEGL())`; the host pre-allocation
//!   side is identical to the OpenGL adapter's — same
//!   `acquire_render_target_dma_buf_image` + surface-share
//!   registration flow.
//!
//! Pipeline shape (post-#487):
//!
//! ```text
//!   Camera ──→ CameraToCudaCopy ──┬──→ AvatarCharacter ──┐
//!                                 │                       ▼
//!                                 │   LowerThird ────→ Blending ──→ CrtFilmGrain ──→ Glitch ──→ Display
//!                                 │                       ▲
//!                                 │   Watermark ──────────┘
//! ```
//!
//! `Glitch` is a Python subprocess processor (`cyberpunk_glitch:CyberpunkGlitch`)
//! that reads CrtFilmGrain's output (a Vulkan-allocated tiled DMA-BUF
//! VkImage; cross-process accessible because the CRT processor dual-
//! registers each ring slot in `surface_store`) and applies a GLSL
//! fragment shader (chromatic aberration / scanlines / slice
//! displacement / film grain). It writes into the host-pre-registered
//! `GLITCH_OUTPUT_SURFACE_UUID` and emits the UUID downstream to
//! Display.
//!
//! `CrtFilmGrain` is an in-process Rust processor that owns a
//! sandboxed graphics-kernel wrapper (`SandboxedCrtFilmGrain` in
//! `crt_film_grain_kernel.rs`). Pre-#487 the kernel + its compute
//! shader lived in `libs/streamlib/src/vulkan/rhi/`; that placement
//! encoded a single demo's app content (Blade Runner CRT vibe) into
//! the engine. They migrated out into the example as transitional
//! sandboxed code (gated by an explicit `xtask check-boundaries`
//! allowlist exception) and migrate into RDG passes when #631 ships.
//!
//! See `docs/architecture/adapter-runtime-integration.md` for the
//! single-pattern principle these adapters ride and
//! `docs/architecture/subprocess-rhi-parity.md` for the carve-out the
//! cdylib's consumer-rhi import path stays inside.

use std::path::PathBuf;

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::HostSurfaceStoreExt;
use streamlib::sdk::rhi::TextureFormat;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::error::Error;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;
use streamlib_adapter_abi::SurfaceId;
use streamlib_consumer_rhi::VulkanLayout;

use crate::blending_compositor::BlendingCompositorProcessor;
use crate::crt_film_grain::CrtFilmGrainProcessor;

/// Cuda surface id the host-side `CameraToCudaCopy` processor
/// registers under and the Python `AvatarCharacter` consumes via
/// its config. The single source of truth is
/// `packages/camera/src/camera_to_cuda_copy.rs::CUDA_CAMERA_SURFACE_ID`
/// — the example doesn't Cargo-dep `@tatolab/camera`, so the value
/// is duplicated here as a literal. If `@tatolab/camera`'s constant
/// changes, the package's bump becomes visible to this consumer
/// the same way any package contract bump becomes visible: through
/// the package's published version.
const AVATAR_CAMERA_CUDA_SURFACE_ID: SurfaceId = 484_001;

/// Surface UUID for the avatar mesh-render output (tiled DMA-BUF
/// `VkImage`). The Python processor renders into it via
/// `OpenGLContext.acquire_write`; the BlendingCompositor consumes it
/// as the `pip_in` input.
const AVATAR_OUTPUT_SURFACE_UUID: &str = "00000000-0000-0000-0000-000000000484";

/// Surface UUID for the cyberpunk lower-third overlay output (tiled
/// DMA-BUF `VkImage`). The Python processor renders into it via
/// `SkiaContext.acquire_write` (Skia-on-GL); the BlendingCompositor
/// consumes it as the `lower_third_in` input. UUID encodes the issue
/// number for traceability.
const LOWER_THIRD_OUTPUT_SURFACE_UUID: &str = "00000000-0000-0000-0000-000000000485";

/// Surface UUID for the spray-paint watermark overlay output. Same
/// shape as the lower-third — tiled DMA-BUF VkImage written via
/// SkiaContext, consumed by BlendingCompositor as `watermark_in`.
const WATERMARK_OUTPUT_SURFACE_UUID: &str = "00000000-0000-0000-0000-000000000486";

/// Surface UUID for the cyberpunk glitch GLSL post-process output
/// (#486). Tiled DMA-BUF VkImage written by the Python `Glitch`
/// subprocess via `OpenGLContext.acquire_write` (ModernGL fragment
/// shader); consumed in-process by `Display` via Path 1. UUID's last
/// octet (`487`) is sequenced after the watermark slot, leaving 486
/// stable for back-traceability.
const GLITCH_OUTPUT_SURFACE_UUID: &str = "00000000-0000-0000-0000-000000000487";

/// Pin everything to 1920x1080 for the first iteration. The Linux
/// camera processor's default capture resolution and the host's
/// pre-allocated cuda + opengl + skia surfaces all use this size.
const SURFACE_WIDTH: u32 = 1920;
const SURFACE_HEIGHT: u32 = 1080;
const BYTES_PER_PIXEL: u32 = 4;

pub fn main() -> Result<()> {
    println!("=== AvatarCharacter (Linux, #484 cuda + opengl adapters) ===\n");

    let runtime = Runner::new()?;

    // Load `@tatolab/camera` and `@tatolab/display` at runtime — both
    // must have been staged via
    // `cargo xtask build-plugins --package @tatolab/camera --package @tatolab/display`
    // before this example runs.
    runtime
        .load_workspace_packages(["@tatolab/camera", "@tatolab/display"])
        .map_err(streamlib::sdk::error::Error::from)?;

    // Load the runner's project. Its `streamlib.yaml` declares the
    // sibling cyberpunk Python sub-package via
    // `patch: path: ../python`, so this single call registers the
    // Python processors alongside the runner's own Rust-backed
    // CrtFilmGrain / BlendingCompositor declarations.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    runtime.load_project(&manifest_dir)?;
    println!("✓ Loaded processor package from streamlib.yaml\n");

    // OpenGL + Skia DMA-BUF render-target output surfaces stay as setup
    // hooks (one-shot pre-allocation; no per-frame host work). Each
    // surface is allocated render-target-capable (tiled DRM modifier)
    // and dual-registered (surface-share for cross-process consumers,
    // GpuContext::texture_cache for in-process Path 1 fast path — the
    // BlendingCompositor reads all three via Path 1).
    //
    // The cuda surface used to ride a setup hook too — that's now
    // owned by the CameraToCudaCopyProcessor below, which also issues
    // the per-frame GPU-side copy.
    runtime.install_setup_hook(move |gpu| {
        register_render_target_surface(
            gpu,
            AVATAR_OUTPUT_SURFACE_UUID,
            "avatar mesh-render output",
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "register avatar surface: {e}"
            ))
        })?;
        println!(
            "✓ Avatar OpenGL DMA-BUF output surface registered: uuid={AVATAR_OUTPUT_SURFACE_UUID}"
        );
        register_render_target_surface(
            gpu,
            LOWER_THIRD_OUTPUT_SURFACE_UUID,
            "lower-third Skia output (#485)",
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "register lower-third surface: {e}"
            ))
        })?;
        println!(
            "✓ Lower-third Skia DMA-BUF output surface registered: uuid={LOWER_THIRD_OUTPUT_SURFACE_UUID}"
        );
        register_render_target_surface(
            gpu,
            WATERMARK_OUTPUT_SURFACE_UUID,
            "watermark Skia output (#485)",
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "register watermark surface: {e}"
            ))
        })?;
        println!(
            "✓ Watermark Skia DMA-BUF output surface registered: uuid={WATERMARK_OUTPUT_SURFACE_UUID}"
        );
        register_render_target_surface(
            gpu,
            GLITCH_OUTPUT_SURFACE_UUID,
            "glitch OpenGL output (#486)",
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "register glitch surface: {e}"
            ))
        })?;
        println!(
            "✓ Glitch OpenGL DMA-BUF output surface registered: uuid={GLITCH_OUTPUT_SURFACE_UUID}"
        );
        Ok(())
    });

    // Camera processor (V4L2 on Linux). The camera config doesn't
    // expose width/height — the camera processor picks based on the
    // device's supported formats. The host pipeline expects 1920x1080
    // BGRA-shaped ring textures; mismatched sizes are rejected by the
    // copy processor at the first frame.
    println!("📷 Adding camera processor...");
    // Match the env-var convention used in `examples/camera-display` so
    // the same flag (`STREAMLIB_CAMERA_DEVICE=/dev/videoN`) targets vivid
    // / v4l2loopback fixtures during E2E. Default `None` lets the camera
    // processor pick by capability.
    let device_id = std::env::var("STREAMLIB_CAMERA_DEVICE").ok();
    let camera_config = match device_id.as_deref() {
        Some(id) => serde_json::json!({ "device_id": id }),
        None => serde_json::json!({}),
    };
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        camera_config,
    ))?;
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
    // `CameraToCudaCopy` is registered through `@tatolab/camera`'s
    // cdylib `STREAMLIB_PLUGIN` callback alongside `Camera`, so the
    // example wires it via `ProcessorSpec` against the package
    // identifier — no in-tree registration. Default config (1920x1080)
    // matches the camera processor's output dimensions.
    let camera_to_cuda = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "CameraToCudaCopy", "1.0.0"),
        serde_json::json!({}),
    ))?;
    println!("✓ Camera→CUDA copy added: {camera_to_cuda}\n");

    // AvatarCharacter (Python subprocess). Reads the cuda surface for
    // GPU-resident YOLO inference, renders skinned mesh into the
    // opengl DMA-BUF surface, emits the surface UUID downstream.
    println!("🐍 Adding Python avatar character (subprocess, PyTorch pose + ModernGL)...");
    let avatar = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "cyberpunk-processor",
            "AvatarCharacter"
        )?,
        serde_json::json!({
            "cuda_camera_surface_id": AVATAR_CAMERA_CUDA_SURFACE_ID,
            "opengl_output_surface_uuid": AVATAR_OUTPUT_SURFACE_UUID,
            "width": SURFACE_WIDTH,
            "height": SURFACE_HEIGHT,
            "channels": BYTES_PER_PIXEL,
        }),
    ))?;
    println!("✓ Avatar character processor added: {avatar}\n");

    // Cyberpunk LowerThird (Python subprocess, Skia-on-GL). Continuous
    // RGBA generator drawing into a pre-registered DMA-BUF VkImage via
    // SkiaContext.acquire_write.
    println!("🐍 Adding Python cyberpunk lower third (subprocess, Skia-on-GL)...");
    let lower_third = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "cyberpunk-processor",
            "CyberpunkLowerThird"
        )?,
        serde_json::json!({
            "output_surface_uuid": LOWER_THIRD_OUTPUT_SURFACE_UUID,
            "width": SURFACE_WIDTH,
            "height": SURFACE_HEIGHT,
        }),
    ))?;
    println!("✓ Lower third processor added: {lower_third}\n");

    // Cyberpunk Watermark (Python subprocess, Skia-on-GL). Same shape
    // as lower-third — distinct UUID, same allocation pattern.
    println!("🐍 Adding Python cyberpunk watermark (subprocess, Skia-on-GL)...");
    let watermark = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "cyberpunk-processor",
            "CyberpunkWatermark"
        )?,
        serde_json::json!({
            "output_surface_uuid": WATERMARK_OUTPUT_SURFACE_UUID,
            "width": SURFACE_WIDTH,
            "height": SURFACE_HEIGHT,
        }),
    ))?;
    println!("✓ Watermark processor added: {watermark}\n");

    // BlendingCompositor (Rust ManualProcessor backed by a sandboxed
    // graphics kernel — see `blending_compositor_kernel.rs` for the
    // transitional rationale). Composites four input layers (video,
    // lower_third, watermark, pip) into one output frame paced against
    // the display refresh rate. Each output ring slot is
    // dual-registered (`texture_cache` for in-process consumers
    // — CrtFilmGrain reads here — `surface_store` for cross-process
    // consumers — would be reachable via the OpenGL adapter); see
    // `blending_compositor.rs::setup_inner`.
    println!("🎨 Adding blending compositor (parallel layer blending)...");
    let blending = runtime.add_processor(BlendingCompositorProcessor::node(Default::default()))?;
    println!("✓ Blending compositor added: {blending}\n");

    // CrtFilmGrain (Rust ReactiveProcessor, Linux only post-#485).
    // Pre-#487 this kernel + its shader lived in `libs/streamlib/`;
    // they relocated to the example as transitional sandboxed content
    // (`crt_film_grain_kernel.rs`) and the .comp shader was ported to
    // .vert + .frag for the texture-throughout pipeline. The
    // processor allocates and dual-registers its own 2-slot output
    // ring in `setup_inner`, so it doesn't need a setup-hook entry
    // here.
    println!("📺 Adding CRT/film-grain post-effect...");
    let crt = runtime.add_processor(CrtFilmGrainProcessor::node(Default::default()))?;
    println!("✓ CRT/film-grain added: {crt}\n");

    // Cyberpunk Glitch (Python subprocess, OpenGL adapter, GLSL
    // fragment shader). Reads CrtFilmGrain's output cross-process via
    // `OpenGLContext.acquire_read`, applies chromatic aberration /
    // scanlines / slice displacement / film-grain glitches, writes
    // into the host-pre-registered GLITCH_OUTPUT_SURFACE_UUID. The
    // intermittent dramatic-mode trigger lives Python-side (single
    // timer, 0–8 s after a 2 s cooldown — see
    // `cyberpunk_glitch.py::GlitchState`).
    println!("🐍 Adding Python cyberpunk glitch (subprocess, OpenGL fragment shader)...");
    let glitch = runtime.add_processor(ProcessorSpec::new(
        streamlib::sdk::schema_ident_any_version!(
            "tatolab",
            "cyberpunk-processor",
            "CyberpunkGlitch"
        )?,
        serde_json::json!({
            "output_surface_uuid": GLITCH_OUTPUT_SURFACE_UUID,
            "width": SURFACE_WIDTH,
            "height": SURFACE_HEIGHT,
        }),
    ))?;
    println!("✓ Glitch processor added: {glitch}\n");

    // Display processor (Vulkan swapchain).
    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": SURFACE_WIDTH,
            "height": SURFACE_HEIGHT,
            "title": "Cyberpunk Pipeline Linux (#484 + #485)",
            "vsync": true,
        }),
    ))?;
    println!("✓ Display added: {display}\n");

    // Wire camera → camera_to_cuda → avatar (PiP) and the camera
    // background + lower_third + watermark + avatar all into the
    // BlendingCompositor → Display. CRT/FilmGrain + Glitch land
    // alongside this in #486/#487.
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
    println!("   ✓ CameraToCudaCopy → AvatarCharacter (cuda inference + camera bg)");
    runtime.connect(
        OutputLinkPortRef::new(&camera_to_cuda, "video_out"),
        InputLinkPortRef::new(&blending, "video_in"),
    )?;
    println!("   ✓ CameraToCudaCopy → BlendingCompositor.video_in (camera always visible)");
    runtime.connect(
        OutputLinkPortRef::new(&avatar, "video_out"),
        InputLinkPortRef::new(&blending, "pip_in"),
    )?;
    println!("   ✓ AvatarCharacter → BlendingCompositor.pip_in (Breaking-News-PiP)");
    runtime.connect(
        OutputLinkPortRef::new(&lower_third, "video_out"),
        InputLinkPortRef::new(&blending, "lower_third_in"),
    )?;
    println!("   ✓ LowerThird → BlendingCompositor.lower_third_in");
    runtime.connect(
        OutputLinkPortRef::new(&watermark, "video_out"),
        InputLinkPortRef::new(&blending, "watermark_in"),
    )?;
    println!("   ✓ Watermark → BlendingCompositor.watermark_in");
    runtime.connect(
        OutputLinkPortRef::new(&blending, "video_out"),
        InputLinkPortRef::new(&crt, "video_in"),
    )?;
    println!("   ✓ BlendingCompositor → CrtFilmGrain (Rust graphics kernel)");
    runtime.connect(
        OutputLinkPortRef::new(&crt, "video_out"),
        InputLinkPortRef::new(&glitch, "video_in"),
    )?;
    println!("   ✓ CrtFilmGrain → Glitch (Python OpenGL fragment shader)");
    runtime.connect(
        OutputLinkPortRef::new(&glitch, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ Glitch → Display\n");

    println!("▶️  Starting pipeline...");
    println!("   Architecture (Linux, #484 + #485 + #486 + #487 + #612):");
    println!("     Camera ──→ CameraToCudaCopy ──┬──→ AvatarCharacter ──┐");
    println!("                                   ├──────────────────────┴── BlendingCompositor ──→ CrtFilmGrain ──→ Glitch ──→ Display");
    println!("                                   │   LowerThird ───────────/");
    println!("                                   │   Watermark ───────────/");
    println!("                (cuda OPAQUE_FD + opengl DMA-BUF + skia-on-GL DMA-BUFs)");
    println!();
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");
    Ok(())
}

/// Allocate a render-target-capable tiled DMA-BUF `VkImage` for one
/// of the Python adapter outputs (avatar OpenGL, lower-third Skia,
/// watermark Skia) and dual-register it under `uuid`. The Skia adapter
/// composes on the OpenGL adapter, so the host pre-allocation side is
/// identical for both — same `acquire_render_target_dma_buf_image` +
/// surface-share registration with no explicit timeline.
fn register_render_target_surface(
    gpu: &GpuContext,
    uuid: &str,
    label: &str,
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
        .map_err(|e| format!("{label}: acquire_render_target_dma_buf_image: {e}"))?;

    let surface_store = gpu
        .surface_store()
        .ok_or_else(|| format!("{label}: GpuContext has no surface_store"))?;
    // OpenGL/Skia adapters don't need an explicit Vulkan timeline:
    // `glFinish` on release plus DMA-BUF kernel-fence semantics carry
    // visibility for downstream consumers. GL writes leave the
    // underlying DMA-BUF in GENERAL from Vulkan's perspective.
    // Declaring it here means cross-process consumers reaching the
    // surface via Path 2 issue their first QFOT acquire barrier from
    // GENERAL — same convention as `polyglot-opengl-fragment-shader`
    // (#633).
    surface_store
        .register_texture(
            uuid,
            &texture,
            None,
            streamlib::sdk::rhi::VulkanLayout::GENERAL,
        )
        .map_err(|e| format!("{label}: surface_store.register_texture: {e}"))?;

    // Mirror the texture into the GpuContext's local same-process cache
    // so downstream processors (BlendingCompositor here) hit Path 1 in
    // `GpuContext::resolve_texture_registration_by_surface_id` instead of the cross-
    // process daemon lookup. This matches what `LinuxCameraProcessor`
    // does for its own ring textures (see `linux/processors/camera.rs`)
    // — without it, same-process consumers can't find the texture by
    // UUID even though surface-share has it.
    //
    // Declare `UNDEFINED` as the registration's initial layout: the
    // OpenGL adapter writes to this VkImage via EGL DMA-BUF import and
    // does not transition the Vulkan-side layout (it issues `glFinish`
    // on release; DMA-BUF kernel-fence semantics carry data visibility,
    // but Vulkan's layout tracker stays at the image's `initialLayout`
    // which is `UNDEFINED` from `acquire_render_target_dma_buf_image`).
    // The consumer's first-frame barrier transitions UNDEFINED →
    // SHADER_READ_ONLY_OPTIMAL — content is technically allowed to be
    // discarded by the spec on this transition but NVIDIA preserves
    // it (verified empirically on RTX 3090). After that first barrier,
    // the consumer's `update_layout` advances the registration to
    // SHADER_READ_ONLY_OPTIMAL; subsequent GL writes don't change the
    // Vulkan tracker, so steady-state barriers are SHADER_READ_ONLY
    // → SHADER_READ_ONLY no-ops.
    gpu.register_texture_with_layout(
        uuid,
        texture,
        VulkanLayout::UNDEFINED,
    );

    Ok(())
}
