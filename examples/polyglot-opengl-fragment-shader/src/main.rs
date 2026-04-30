// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot OpenGL adapter scenario (#530).
//!
//! End-to-end gate for the subprocess `OpenGLContext` runtime: the host
//! pre-allocates ONE render-target-capable DMA-BUF surface and registers
//! it with surface-share under a known UUID. A Python or Deno polyglot
//! processor opens the surface through `OpenGLContext.acquire_write`,
//! compiles a fragment shader (Mandelbrot in Python, plasma waves in
//! Deno), binds an FBO to the imported `GL_TEXTURE_2D`, draws a
//! fullscreen quad, releases — the adapter's `glFinish` on release
//! ensures cross-API consumers see the writes through the underlying
//! DMA-BUF. After the runtime stops, this binary reads the surface
//! back via Vulkan and writes a PNG; reading that PNG with the Read
//! tool is the visual gate.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-opengl-fragment-shader/python
//!
//! Run:
//!   cargo run -p polyglot-opengl-fragment-shader-scenario -- \
//!       --runtime=python --output=/tmp/opengl-mandelbrot.png
//!   cargo run -p polyglot-opengl-fragment-shader-scenario -- \
//!       --runtime=deno   --output=/tmp/opengl-plasma.png

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::core::rhi::{
    TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
};
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::host_rhi::VulkanTextureReadback;
use streamlib::{BgraFileSourceProcessor, ProcessorSpec, Result, StreamRuntime};

/// UUID the host registers the render-target surface under. The
/// polyglot processor reads it from its config and passes it to
/// `OpenGLContext.acquire_write`.
const SCENARIO_SURFACE_UUID: &str = "00000000-0000-0000-0000-0000000005c0";

/// Side length of the surface. Square keeps Mandelbrot/plasma math
/// straightforward; 512 is large enough to be visually obvious and
/// small enough that the scenario runs in a couple seconds.
const SURFACE_SIZE: u32 = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    Python,
    Deno,
}

impl RuntimeKind {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "python" => Ok(Self::Python),
            "deno" => Ok(Self::Deno),
            other => Err(format!(
                "unknown --runtime value '{other}' (expected 'python' or 'deno')"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Deno => "deno",
        }
    }

    fn processor_name(self) -> &'static str {
        match self {
            Self::Python => "com.tatolab.opengl_fragment_shader",
            Self::Deno => "com.tatolab.opengl_fragment_shader_deno",
        }
    }
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);

    let mut runtime_kind = RuntimeKind::Python;
    let mut output_png = PathBuf::from("/tmp/opengl-fragment-shader.png");

    for a in args {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        }
    }

    println!("=== Polyglot OpenGL adapter fragment-shader scenario (#530) ===");
    println!("Runtime:     {}", runtime_kind.as_str());
    println!(
        "Surface:     {SURFACE_SIZE}x{SURFACE_SIZE} BGRA8 (uuid {SCENARIO_SURFACE_UUID})"
    );
    println!("Output PNG:  {}", output_png.display());
    println!();

    let runtime = StreamRuntime::new()?;

    // Slots the setup hook populates so main.rs can read the surface
    // back post-stop and write the output PNG. We can't keep the
    // `&GpuContext` borrow past the hook, so we Arc-clone the bits we
    // need.
    let texture_slot: Arc<
        Mutex<Option<streamlib::core::rhi::StreamTexture>>,
    > = Arc::new(Mutex::new(None));
    let readback_slot: Arc<Mutex<Option<Arc<VulkanTextureReadback>>>> =
        Arc::new(Mutex::new(None));

    {
        let texture_slot = Arc::clone(&texture_slot);
        let readback_slot = Arc::clone(&readback_slot);
        runtime.install_setup_hook(move |gpu| {
            let texture = gpu.acquire_render_target_dma_buf_image(
                SURFACE_SIZE,
                SURFACE_SIZE,
                TextureFormat::Bgra8Unorm,
            )?;
            // Register with surface-share so the subprocess can look
            // it up via `gpu_limited_access.resolve_surface`. The
            // adapter's `OpenGlSurfaceAdapter` then imports the
            // resulting DMA-BUF FD as an EGLImage + GL_TEXTURE_2D.
            let store = gpu.surface_store().ok_or_else(|| {
                StreamError::Configuration(
                    "surface_store unavailable — host runtime built without \
                     a surface-share service (Linux subprocess flow requires it)"
                        .into(),
                )
            })?;
            // OpenGL adapter doesn't need an explicit Vulkan timeline:
            // `glFinish` on release plus DMA-BUF kernel-fence semantics
            // carry visibility for the host's pre-stop readback.
            store
                .register_texture(SCENARIO_SURFACE_UUID, &texture, None)
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "register_texture: {e}"
                    ))
                })?;

            // RHI-owned readback handle for the post-stop pixel
            // capture — staging buffer + command resources + timeline
            // semaphore allocate once at construction.
            let readback = gpu.create_texture_readback(&TextureReadbackDescriptor {
                label: "polyglot-opengl-fragment-shader/readback",
                format: TextureFormat::Bgra8Unorm,
                width: SURFACE_SIZE,
                height: SURFACE_SIZE,
            })?;

            *texture_slot.lock().unwrap() = Some(texture);
            *readback_slot.lock().unwrap() = Some(readback);
            println!(
                "✓ render-target DMA-BUF surface registered as '{}'",
                SCENARIO_SURFACE_UUID
            );
            Ok(())
        });
    }

    // Load the polyglot package.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path = manifest_dir
                .join("python/polyglot-opengl-fragment-shader-0.1.0.slpkg");
            if !slpkg_path.exists() {
                return Err(StreamError::Configuration(format!(
                    "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-opengl-fragment-shader/python",
                    slpkg_path.display()
                )));
            }
            runtime.load_package(&slpkg_path)?;
        }
        RuntimeKind::Deno => {
            let project_path = manifest_dir.join("deno");
            if !project_path.join("streamlib.yaml").exists() {
                return Err(StreamError::Configuration(format!(
                    "Deno project not found: {}",
                    project_path.display()
                )));
            }
            runtime.load_project(&project_path)?;
        }
    }

    // Trigger source: BgraFileSource emits a few `Videoframe`s so the
    // polyglot processor's `process()` is invoked. The processor
    // ignores frame contents — it works on the pre-registered host
    // surface, not the trigger frame's pixel buffer.
    let fixture_path = write_trigger_fixture()
        .map_err(StreamError::Configuration)?;

    let source = runtime.add_processor(BgraFileSourceProcessor::Processor::node(
        BgraFileSourceProcessor::Config {
            file_path: fixture_path
                .to_str()
                .ok_or_else(|| {
                    StreamError::Configuration(
                        "fixture path has non-utf8 component".into(),
                    )
                })?
                .to_string(),
            width: 4,
            height: 4,
            fps: 5,
            frame_count: 3,
        },
    ))?;
    println!("+ BgraFileSource: {source}");

    let shader_config = serde_json::json!({
        "opengl_surface_uuid": SCENARIO_SURFACE_UUID,
        "width": SURFACE_SIZE,
        "height": SURFACE_SIZE,
    });
    let shader = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        shader_config,
    ))?;
    println!("+ Fragment shader: {shader}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&shader, "video_in"),
    )?;
    println!(
        "\nPipeline: BgraFileSource → {} fragment-shader\n",
        runtime_kind.as_str()
    );

    println!("Starting pipeline...");
    runtime.start()?;

    // Give the polyglot processor time to receive a trigger frame and
    // complete the GL acquire/draw/release cycle. The Python/Deno
    // processors guard against re-rendering on subsequent frames so the
    // PNG stays clean.
    std::thread::sleep(Duration::from_secs(4));

    println!("Stopping pipeline...");
    runtime.stop()?;

    // Read the surface back via Vulkan and write the output PNG.
    println!("\nReading host surface back via Vulkan...");
    let texture = texture_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| {
            StreamError::Runtime(
                "host texture slot is empty — setup hook never ran".into(),
            )
        })?;
    let readback = readback_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| StreamError::Runtime("readback slot is empty".into()))?;
    // OpenGL adapter leaves the image in GENERAL.
    let ticket = readback
        .submit(&texture, TextureSourceLayout::General)
        .map_err(|e| StreamError::Runtime(format!("readback submit: {e}")))?;
    let bgra = readback
        .wait_and_read(ticket, u64::MAX)
        .map_err(|e| StreamError::Runtime(format!("readback wait: {e}")))?
        .to_vec();
    write_png(&bgra, SURFACE_SIZE, SURFACE_SIZE, &output_png)?;
    println!("✓ Output PNG written: {}", output_png.display());

    Ok(())
}

/// Write a tiny BGRA fixture file. BgraFileSource reads it
/// frame-by-frame; the resulting `Videoframe`s are the trigger that
/// drives the polyglot processor's `process()` call.
fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("opengl-fragment-shader-trigger.bgra");
    let mut f = File::create(&path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Encode BGRA bytes as RGBA PNG (channel-swap on the fly).
fn write_png(
    bgra: &[u8],
    width: u32,
    height: u32,
    output: &std::path::Path,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;

    let mut rgba = vec![0u8; bgra.len()];
    for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = src[3];
    }

    let file = File::create(output).map_err(|e| {
        StreamError::Configuration(format!(
            "create output PNG {}: {e}",
            output.display()
        ))
    })?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| StreamError::Configuration(format!("PNG header: {e}")))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| StreamError::Configuration(format!("PNG body: {e}")))?;
    Ok(())
}
