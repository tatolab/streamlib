// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot cpu-readback adapter scenario (#529).
//!
//! End-to-end gate for the cpu-readback subprocess runtime: the host
//! pre-registers ONE cpu-readback surface and uploads a known input
//! pattern; a Python or Deno polyglot processor opens the surface
//! through `CpuReadbackContext.acquire_write`, applies a Gaussian blur
//! (cv2 in Python, hand-rolled separable kernel in Deno), and on
//! release the host-side adapter flushes CPU→GPU. After the runtime
//! stops, this binary reads the surface back through the adapter and
//! writes the result to a PNG. Reading that PNG with the Read tool is
//! the visual gate — the output must show the blurred input pattern.
//!
//! Pipeline shape:
//!
//!   ┌──────────────────┐   trigger frame   ┌────────────────────────┐
//!   │ BgraFileSource   │ ────────────────► │ Polyglot Blur Processor│
//!   │ (reads tiny      │                   │  (Python / Deno)       │
//!   │  BGRA fixture)   │                   │  acquires surface_id=1 │
//!   └──────────────────┘                   │  applies Gaussian blur │
//!                                          └────────────────────────┘
//!
//! BgraFileSource's emitted `Videoframe` is just the trigger that
//! drives the polyglot processor's `process()` call — the polyglot
//! processor ignores `frame.surface_id` and works on the
//! cpu-readback host surface (id `1`) the host pre-registered.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-cpu-readback-blur/python
//!
//! Run:
//!   cargo run -p polyglot-cpu-readback-blur-scenario -- \
//!       --runtime=python --output=/tmp/cpu-readback-blur.png
//!   cargo run -p polyglot-cpu-readback-blur-scenario -- \
//!       --runtime=deno   --output=/tmp/cpu-readback-blur.png

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::adapter_support::HostVulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::{BgraFileSourceProcessor, ProcessorSpec, Result, StreamRuntime};
use streamlib_adapter_abi::{SurfaceFormat, SurfaceId};
use streamlib_adapter_cpu_readback::{
    CpuReadbackBridgeImpl, CpuReadbackSurfaceAdapter, HostSurfaceRegistration,
};

/// Single host surface id used throughout this scenario. The polyglot
/// processor receives this id via its config.
const SCENARIO_SURFACE_ID: SurfaceId = 1;

/// Square dimensions for the cpu-readback surface. Small enough to
/// keep the run fast; large enough that a Gaussian blur is visually
/// obvious in the output PNG.
const SURFACE_SIZE: u32 = 256;

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
            Self::Python => "com.tatolab.cpu_readback_blur",
            Self::Deno => "com.tatolab.cpu_readback_blur_deno",
        }
    }
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);

    let mut runtime_kind = RuntimeKind::Python;
    let mut output_png = PathBuf::from("/tmp/cpu-readback-blur.png");
    let mut kernel_size: u32 = 11;
    let mut sigma: f32 = 4.0;

    for a in args {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        } else if let Some(value) = a.strip_prefix("--kernel-size=") {
            kernel_size = value.parse().map_err(|e| {
                StreamError::Configuration(format!("invalid --kernel-size: {e}"))
            })?;
        } else if let Some(value) = a.strip_prefix("--sigma=") {
            sigma = value.parse().map_err(|e| {
                StreamError::Configuration(format!("invalid --sigma: {e}"))
            })?;
        }
    }

    println!("=== Polyglot cpu-readback adapter scenario (#529) ===");
    println!("Runtime:     {}", runtime_kind.as_str());
    println!(
        "Surface:     {SURFACE_SIZE}x{SURFACE_SIZE} BGRA8 (id {SCENARIO_SURFACE_ID})"
    );
    println!("Blur:        kernel={kernel_size} sigma={sigma}");
    println!("Output PNG:  {}", output_png.display());
    println!();

    let runtime = StreamRuntime::new()?;

    // Slot the setup hook will populate with the cpu-readback adapter
    // it constructs — main.rs reuses this Arc post-stop to read the
    // surface back for the output PNG.
    let adapter_slot: Arc<Mutex<Option<Arc<CpuReadbackSurfaceAdapter>>>> =
        Arc::new(Mutex::new(None));

    {
        let adapter_slot = Arc::clone(&adapter_slot);
        runtime.install_setup_hook(move |gpu| {
            let adapter = Arc::new(CpuReadbackSurfaceAdapter::new(Arc::clone(
                gpu.device().vulkan_device(),
            )));
            register_host_surface(&adapter, gpu)?;
            upload_input_pattern(&adapter)?;
            gpu.set_cpu_readback_bridge(Arc::new(CpuReadbackBridgeImpl::new(
                Arc::clone(&adapter),
            )));
            *adapter_slot.lock().unwrap() = Some(adapter);
            println!("✓ cpu-readback adapter registered, surface uploaded, bridge installed");
            Ok(())
        });
    }

    // Load the polyglot package.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path =
                manifest_dir.join("python/polyglot-cpu-readback-blur-0.1.0.slpkg");
            if !slpkg_path.exists() {
                return Err(StreamError::Configuration(format!(
                    "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-cpu-readback-blur/python",
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

    // Trigger source: a tiny BGRA fixture that drives Videoframes
    // through the pipeline so the polyglot processor's `process()` is
    // invoked. Frame contents are unused (the polyglot processor works
    // on the pre-registered cpu-readback surface, not the trigger
    // frame's pixel buffer).
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

    let blur_config = serde_json::json!({
        "cpu_readback_surface_id": SCENARIO_SURFACE_ID,
        "kernel_size": kernel_size,
        "sigma": sigma,
    });
    let blur = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        blur_config,
    ))?;
    println!("+ Blur:           {blur}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&blur, "video_in"),
    )?;
    println!("\nPipeline: BgraFileSource → {} blur\n", runtime_kind.as_str());

    println!("Starting pipeline...");
    runtime.start()?;

    // Give the polyglot processor time to receive at least one trigger
    // frame and complete the cpu-readback acquire/blur/release cycle.
    std::thread::sleep(Duration::from_secs(3));

    println!("Stopping pipeline...");
    runtime.stop()?;

    // Read the surface back through the adapter and write the output
    // PNG. Reading this PNG with the Read tool is the visual gate.
    println!("\nReading cpu-readback surface back through the adapter...");
    let adapter = adapter_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| {
            StreamError::Runtime(
                "cpu-readback adapter slot is empty — setup hook never ran"
                    .into(),
            )
        })?;
    write_output_png(&adapter, &output_png)?;
    println!("✓ Output PNG written: {}", output_png.display());

    Ok(())
}

/// Allocate a render-target-capable DMA-BUF VkImage and an exportable
/// timeline semaphore, then register the pair with the cpu-readback
/// adapter under [`SCENARIO_SURFACE_ID`].
fn register_host_surface(
    adapter: &Arc<CpuReadbackSurfaceAdapter>,
    gpu: &GpuContext,
) -> Result<()> {
    let texture = gpu.acquire_render_target_dma_buf_image(
        SURFACE_SIZE,
        SURFACE_SIZE,
        TextureFormat::Bgra8Unorm,
    )?;
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new(adapter.device().device(), 0).map_err(|e| {
            StreamError::Configuration(format!("create timeline semaphore: {e}"))
        })?,
    );
    adapter
        .register_host_surface(
            SCENARIO_SURFACE_ID,
            HostSurfaceRegistration {
                texture,
                timeline,
                initial_image_layout: vulkanalia::vk::ImageLayout::UNDEFINED.as_raw(),
                format: SurfaceFormat::Bgra8,
            },
        )
        .map_err(|e| {
            StreamError::Configuration(format!("register_host_surface: {e}"))
        })?;
    Ok(())
}

/// Pre-populate the cpu-readback surface with a known input pattern —
/// vertical color bands so the Gaussian blur's smoothing is obvious in
/// the output PNG. Uses the adapter's `acquire_write_by_id` API; the
/// guard's Drop runs the CPU→GPU sync.
fn upload_input_pattern(adapter: &Arc<CpuReadbackSurfaceAdapter>) -> Result<()> {
    let mut guard = adapter.acquire_write_by_id(SCENARIO_SURFACE_ID).map_err(|e| {
        StreamError::Configuration(format!("upload_input_pattern acquire: {e}"))
    })?;

    {
        let view = guard.view_mut();
        let plane = view.plane_mut(0);
        let bytes = plane.bytes_mut();
        let stride = (SURFACE_SIZE * 4) as usize;
        let band_w = (SURFACE_SIZE / 4) as usize;

        // BGRA bands: blue, green, red, white (in BGRA byte order).
        let bands: [[u8; 4]; 4] = [
            [255, 0, 0, 255],     // Blue
            [0, 255, 0, 255],     // Green
            [0, 0, 255, 255],     // Red
            [255, 255, 255, 255], // White
        ];

        for y in 0..SURFACE_SIZE as usize {
            for x in 0..SURFACE_SIZE as usize {
                let band = (x / band_w).min(3);
                let pixel_off = y * stride + x * 4;
                bytes[pixel_off..pixel_off + 4].copy_from_slice(&bands[band]);
            }
        }
    }
    drop(guard);
    Ok(())
}

/// Write a minimal BGRA fixture file. BgraFileSource reads it
/// frame-by-frame; the resulting Videoframes are the trigger that
/// drives the polyglot processor's `process()` call. Frame contents
/// are unused — the polyglot processor works on the pre-registered
/// cpu-readback surface, not the trigger frame's pixel buffer.
fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("cpu-readback-blur-trigger.bgra");
    // 4x4 BGRA × 3 frames = 192 bytes of zeros. Just enough bytes for
    // BgraFileSource to consume `frame_count=3` × `width*height*4`.
    let mut f = File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Acquire the cpu-readback surface for read, dump the BGRA bytes into
/// a PNG (BGRA→RGBA channel swap to match PNG color order), write to
/// `output`. The reader of this PNG is the visual gate that decides
/// whether the polyglot subprocess actually applied the blur.
fn write_output_png(
    adapter: &Arc<CpuReadbackSurfaceAdapter>,
    output: &std::path::Path,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;

    let guard = adapter.acquire_read_by_id(SCENARIO_SURFACE_ID).map_err(|e| {
        StreamError::Configuration(format!(
            "acquire_read_by_id for output PNG: {e}"
        ))
    })?;

    let view = guard.view();
    let plane = view.plane(0);
    let bgra = plane.bytes();
    let mut rgba = vec![0u8; bgra.len()];
    for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
        dst[0] = src[2]; // R ← B
        dst[1] = src[1]; // G ← G
        dst[2] = src[0]; // B ← R
        dst[3] = src[3]; // A
    }

    let file = File::create(output).map_err(|e| {
        StreamError::Configuration(format!(
            "create output PNG {}: {e}",
            output.display()
        ))
    })?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), SURFACE_SIZE, SURFACE_SIZE);
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
