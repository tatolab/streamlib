// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot Vulkan adapter scenario (#531).
//!
//! End-to-end gate for the subprocess `VulkanContext` runtime: the host
//! pre-allocates ONE render-target-capable DMA-BUF surface AND an
//! exportable `HostVulkanTimelineSemaphore`, registers both with surface-share
//! under a known UUID. A Python or Deno polyglot processor opens the
//! surface through `VulkanContext.acquire_write` (which imports the
//! DMA-BUF as a `VkImage` in the subprocess and imports the timeline via
//! `from_imported_opaque_fd`), dispatches the Mandelbrot compute shader,
//! and releases — the host adapter advances the timeline so the host's
//! pre-stop readback sees the writes. This binary then reads the surface
//! back via Vulkan and writes a PNG; reading the PNG with the Read tool
//! is the visual gate.
//!
//! The compute shader (`shaders/mandelbrot.comp`) is compiled to SPIR-V
//! at build time via `build.rs`, embedded as bytes here, and shipped to
//! the polyglot processor via the processor config as a hex string.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-vulkan-compute/python
//!
//! Run:
//!   cargo run -p polyglot-vulkan-compute-scenario -- \
//!       --runtime=python --output=/tmp/vulkan-mandelbrot-py.png
//!   cargo run -p polyglot-vulkan-compute-scenario -- \
//!       --runtime=deno   --output=/tmp/vulkan-mandelbrot-deno.png

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::core::context::ComputeKernelBridge;
use streamlib::core::rhi::{
    derive_bindings_from_spirv, ComputeKernelDescriptor, StreamTexture, TextureFormat,
    TextureReadbackDescriptor, TextureSourceLayout,
};
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::host_rhi::{
    HostVulkanDevice, HostVulkanTimelineSemaphore, VulkanComputeKernel, VulkanTextureReadback,
};
use streamlib::{BgraFileSourceProcessor, ProcessorSpec, Result, StreamRuntime};

/// Compiled SPIR-V for the Mandelbrot compute shader. Built by
/// `build.rs` from `shaders/mandelbrot.comp`. Shipped to the polyglot
/// processor as a hex-encoded string in the processor config.
const MANDELBROT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/mandelbrot.spv"));

/// UUID the host registers the render-target surface under. The
/// polyglot processor reads it from its config and passes it to
/// `VulkanContext.acquire_write`.
const SCENARIO_SURFACE_UUID: &str = "00000000-0000-0000-0000-0000000005c1";

/// Side length of the surface. Square keeps the kernel's group-count
/// math straightforward; 512 is large enough to be visually obvious
/// and small enough that the scenario runs in a couple seconds.
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
            Self::Python => "com.tatolab.vulkan_compute",
            Self::Deno => "com.tatolab.vulkan_compute_deno",
        }
    }
}

/// Bridge between the host runtime's `set_compute_kernel_bridge` and
/// the host's `VulkanComputeKernel`. Lives in this example because the
/// `ComputeKernelBridge` trait lives in `streamlib` and the
/// `streamlib-adapter-vulkan` crate cannot depend on the full
/// `streamlib` (the consumer-rhi capability boundary forbids it).
///
/// Holds a UUID → `StreamTexture` map populated at setup time so
/// `run_compute_kernel(surface_uuid, ...)` can resolve to the host's
/// `VkImage` for the storage_image binding. The kernel cache is
/// keyed by SHA-256(spv) hex (the same key the wire format returns
/// to the subprocess).
struct MandelbrotKernelBridge {
    device: Arc<HostVulkanDevice>,
    surfaces: HashMap<String, StreamTexture>,
    kernels: parking_lot::Mutex<HashMap<String, Arc<VulkanComputeKernel>>>,
}

impl MandelbrotKernelBridge {
    fn new(
        device: Arc<HostVulkanDevice>,
        surfaces: Vec<(String, StreamTexture)>,
    ) -> Self {
        Self {
            device,
            surfaces: surfaces.into_iter().collect(),
            kernels: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{:x}", h.finalize())
    }
}

impl ComputeKernelBridge for MandelbrotKernelBridge {
    fn register(
        &self,
        spv: &[u8],
        push_constant_size: u32,
    ) -> std::result::Result<String, String> {
        let kernel_id = Self::sha256_hex(spv);
        let mut kernels = self.kernels.lock();
        if !kernels.contains_key(&kernel_id) {
            let (bindings, reflected_push) = derive_bindings_from_spirv(spv)
                .map_err(|e| format!("derive_bindings_from_spirv: {e}"))?;
            if reflected_push != push_constant_size {
                return Err(format!(
                    "push_constant_size mismatch — caller declared {push_constant_size}, \
                     SPIR-V reflects {reflected_push}"
                ));
            }
            let descriptor = ComputeKernelDescriptor {
                label: "polyglot-mandelbrot",
                spv,
                bindings: &bindings,
                push_constant_size,
            };
            let kernel = VulkanComputeKernel::new(&self.device, &descriptor)
                .map_err(|e| format!("VulkanComputeKernel::new: {e}"))?;
            kernels.insert(kernel_id.clone(), Arc::new(kernel));
        }
        Ok(kernel_id)
    }

    fn run(
        &self,
        kernel_id: &str,
        surface_uuid: &str,
        push_constants: &[u8],
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> std::result::Result<(), String> {
        let kernel = self
            .kernels
            .lock()
            .get(kernel_id)
            .cloned()
            .ok_or_else(|| {
                format!("kernel_id '{kernel_id}' not registered with this bridge")
            })?;
        let texture = self.surfaces.get(surface_uuid).ok_or_else(|| {
            format!(
                "surface_uuid '{surface_uuid}' not registered with this bridge"
            )
        })?;
        kernel
            .set_storage_image(0, texture)
            .map_err(|e| format!("set_storage_image(0): {e}"))?;
        if !push_constants.is_empty() {
            kernel
                .set_push_constants(push_constants)
                .map_err(|e| format!("set_push_constants: {e}"))?;
        }
        kernel
            .dispatch(group_count_x, group_count_y, group_count_z)
            .map_err(|e| format!("kernel.dispatch: {e}"))?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);

    let mut runtime_kind = RuntimeKind::Python;
    let mut output_png = PathBuf::from("/tmp/vulkan-mandelbrot.png");

    for a in args {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        }
    }

    println!("=== Polyglot Vulkan adapter compute scenario (#531) ===");
    println!("Runtime:     {}", runtime_kind.as_str());
    println!(
        "Surface:     {SURFACE_SIZE}x{SURFACE_SIZE} BGRA8 (uuid {SCENARIO_SURFACE_UUID})"
    );
    println!("SPIR-V:      {} bytes", MANDELBROT_SPV.len());
    println!("Output PNG:  {}", output_png.display());
    println!();

    let runtime = StreamRuntime::new()?;

    let texture_slot: Arc<
        Mutex<Option<streamlib::core::rhi::StreamTexture>>,
    > = Arc::new(Mutex::new(None));
    let timeline_slot: Arc<Mutex<Option<Arc<HostVulkanTimelineSemaphore>>>> =
        Arc::new(Mutex::new(None));
    let readback_slot: Arc<Mutex<Option<Arc<VulkanTextureReadback>>>> =
        Arc::new(Mutex::new(None));

    {
        let texture_slot = Arc::clone(&texture_slot);
        let timeline_slot = Arc::clone(&timeline_slot);
        let readback_slot = Arc::clone(&readback_slot);
        runtime.install_setup_hook(move |gpu| {
            let texture = gpu.acquire_render_target_dma_buf_image(
                SURFACE_SIZE,
                SURFACE_SIZE,
                TextureFormat::Rgba8Unorm,
            )?;
            let host_device = Arc::clone(gpu.device().vulkan_device());
            // The Vulkan adapter on the host needs a per-surface
            // exportable timeline. The host signals it after the
            // subprocess release; the subprocess waits on it before
            // every acquire.
            let timeline = Arc::new(
                HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
                    .map_err(|e| {
                        StreamError::Configuration(format!(
                            "HostVulkanTimelineSemaphore::new_exportable: {e}"
                        ))
                    })?,
            );
            // Surface-share registration carries BOTH the DMA-BUF FD
            // and the timeline OPAQUE_FD so the subprocess can wire up
            // the host adapter's `register_host_surface` directly.
            let store = gpu.surface_store().ok_or_else(|| {
                StreamError::Configuration(
                    "surface_store unavailable — host runtime built without \
                     a surface-share service (Linux subprocess flow requires it)"
                        .into(),
                )
            })?;
            // Mandelbrot kernel writes to GENERAL (per shader binding
            // declaration) and the host-side compute pass leaves the
            // image in GENERAL after the dispatch. Declaring it here
            // means the subprocess's post-release layout view matches
            // the actual image state for the first frame onward (#633).
            store
                .register_texture(
                    SCENARIO_SURFACE_UUID,
                    &texture,
                    Some(timeline.as_ref()),
                    streamlib::core::rhi::VulkanLayout::GENERAL,
                )
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "register_texture: {e}"
                    ))
                })?;

            // Wire the compute-kernel bridge: subprocess
            // `register_compute_kernel` + `run_compute_kernel` IPCs
            // are routed through this bridge to the host's
            // `VulkanComputeKernel`. The bridge holds a
            // UUID→`StreamTexture` map populated here at setup time.
            let bridge = Arc::new(MandelbrotKernelBridge::new(
                Arc::clone(&host_device),
                vec![(SCENARIO_SURFACE_UUID.to_string(), texture.clone())],
            ));
            gpu.set_compute_kernel_bridge(bridge);

            // RHI-owned readback handle for the post-stop pixel
            // capture — the staging buffer + command resources +
            // timeline semaphore allocate once at construction.
            let readback = gpu.create_texture_readback(&TextureReadbackDescriptor {
                label: "polyglot-vulkan-compute/readback",
                format: TextureFormat::Rgba8Unorm,
                width: SURFACE_SIZE,
                height: SURFACE_SIZE,
            })?;

            *texture_slot.lock().unwrap() = Some(texture);
            *timeline_slot.lock().unwrap() = Some(timeline);
            *readback_slot.lock().unwrap() = Some(readback);
            println!(
                "✓ render-target DMA-BUF + timeline registered as '{}'",
                SCENARIO_SURFACE_UUID
            );
            Ok(())
        });
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path = manifest_dir
                .join("python/polyglot-vulkan-compute-0.1.0.slpkg");
            if !slpkg_path.exists() {
                return Err(StreamError::Configuration(format!(
                    "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-vulkan-compute/python",
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

    // Trigger source: a few BGRA frames so the polyglot processor's
    // `process()` is invoked. Frame contents are ignored — the processor
    // works on the pre-registered host surface, not the trigger frame's
    // pixel buffer.
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

    let spv_hex = bytes_to_hex(MANDELBROT_SPV);
    let variant: u32 = match runtime_kind {
        RuntimeKind::Python => 0,
        RuntimeKind::Deno => 1,
    };
    let compute_config = serde_json::json!({
        "vulkan_surface_uuid": SCENARIO_SURFACE_UUID,
        "width": SURFACE_SIZE,
        "height": SURFACE_SIZE,
        "max_iter": 256,
        "variant": variant,
        "shader_spv_hex": spv_hex,
    });
    let compute = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        compute_config,
    ))?;
    println!("+ Vulkan compute processor: {compute}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&compute, "video_in"),
    )?;
    println!(
        "\nPipeline: BgraFileSource → {} vulkan-compute\n",
        runtime_kind.as_str()
    );

    println!("Starting pipeline...");
    runtime.start()?;
    std::thread::sleep(Duration::from_secs(4));
    println!("Stopping pipeline...");
    runtime.stop()?;

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

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("vulkan-compute-trigger.bgra");
    let mut f = File::create(&path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn write_png(
    bgra: &[u8],
    width: u32,
    height: u32,
    output: &std::path::Path,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;

    // Surface is allocated as `Rgba8Unorm` end-to-end (host allocator,
    // subprocess storage-image view, shader's `rgba8` qualifier all
    // match), so the readback bytes are already RGBA — no channel
    // swap needed for PNG encoding.
    let rgba = bgra.to_vec();

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
