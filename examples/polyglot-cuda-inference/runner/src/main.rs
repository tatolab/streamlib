// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot CUDA adapter scenario (#591, milestone closer for the
//! CUDA adapter).
//!
//! End-to-end gate for the CUDA subprocess runtime: the host pre-
//! allocates one HOST_VISIBLE OPAQUE_FD-exportable `VkBuffer` and one
//! exportable timeline semaphore, registers the pair with the surface-
//! share service so the subprocess can import them through
//! `streamlib-consumer-rhi`'s `ConsumerVulkanBuffer` /
//! `ConsumerVulkanTimelineSemaphore` and re-import them into CUDA via
//! `cudaImportExternalMemory(OPAQUE_FD)` /
//! `cudaImportExternalSemaphore(TimelineSemaphoreFd)`. The Python
//! processor opens the surface through `CudaContext.acquire_write` to
//! upload a test image (loaded from disk or downloaded from
//! ultralytics' demo asset URL), then through
//! `CudaContext.acquire_read` to run `torch.from_dlpack` against a real
//! YOLOv8n CUDA model and writes an annotated PNG. The Deno processor
//! verifies the DLPack capsule's structural shape (`device_type ==
//! kDLCUDA`, non-zero `device_ptr`, expected `size`) — Deno's ML
//! ecosystem has no `from_dlpack` consumer for `DLManagedTensor*` (per
//! `libs/streamlib-deno/adapters/cuda.ts` lines 28–37) so the gate is
//! capsule-shape validation, not model inference.
//!
//! Pipeline shape:
//!
//!   ┌──────────────────┐   trigger frame   ┌─────────────────────────┐
//!   │ BgraFileSource   │ ────────────────► │ Polyglot CUDA Processor │
//!   │ (tiny BGRA       │                   │  (Python YOLO / Deno    │
//!   │  fixture)        │                   │   capsule validation)   │
//!   └──────────────────┘                   └─────────────────────────┘
//!
//! The trigger frame's contents are unused — the polyglot processor
//! works on the pre-registered cuda OPAQUE_FD surface (id `1`).
//!
//! Run:
//!   cargo run -p polyglot-cuda-inference-scenario -- \
//!       --runtime=python --output=/tmp/cuda-inference.png
//!   cargo run -p polyglot-cuda-inference-scenario -- \
//!       --runtime=deno   --output=/tmp/cuda-inference-deno.png

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::engine::HostSurfaceStoreExt;

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::descriptors::SchemaIdent;
use streamlib::sdk::rhi::{PixelFormat, PixelBuffer};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::error::Error;
use streamlib::sdk::engine::host_rhi::{
    HostMarker,
    HostVulkanBuffer,
    HostVulkanTimelineSemaphore,
};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::schema_ident;
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::Runner;
use streamlib_adapter_abi::SurfaceId;
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};

/// Single host surface id used throughout this scenario. The polyglot
/// processor receives this id via its config.
const SCENARIO_SURFACE_ID: SurfaceId = 1;

/// Width × Height × 4 bytes (BGRA8) — 640×640 is YOLOv8's default
/// imgsz, sized so the model receives the buffer as-is without
/// host-side resizing.
const SURFACE_WIDTH: u32 = 640;
const SURFACE_HEIGHT: u32 = 640;
const BYTES_PER_PIXEL: u32 = 4;

type HostAdapter = CudaSurfaceAdapter<streamlib::sdk::engine::host_rhi::HostVulkanDevice>;

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

    fn processor_ident(self) -> Result<SchemaIdent> {
        match self {
            Self::Python => streamlib::sdk::schema_ident_any_version!(
                "tatolab",
                "polyglot-cuda-inference",
                "CudaInference"
            ),
            Self::Deno => streamlib::sdk::schema_ident_any_version!(
                "tatolab",
                "polyglot-cuda-inference-deno",
                "CudaInferenceProcessor"
            ),
        }
    }
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);

    let mut runtime_kind = RuntimeKind::Python;
    let mut output_png = PathBuf::from("/tmp/cuda-inference.png");
    let mut timeout_secs: u64 = 60;

    for a in args {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(Error::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        } else if let Some(value) = a.strip_prefix("--timeout-secs=") {
            timeout_secs = value.parse().map_err(|e| {
                Error::Configuration(format!("invalid --timeout-secs: {e}"))
            })?;
        }
    }

    println!("=== Polyglot CUDA adapter scenario (#591) ===");
    println!("Runtime:     {}", runtime_kind.as_str());
    println!(
        "Surface:     {SURFACE_WIDTH}x{SURFACE_HEIGHT} BGRA8 OPAQUE_FD (id {SCENARIO_SURFACE_ID})"
    );
    println!("Output PNG:  {}", output_png.display());
    println!("Timeout:     {timeout_secs}s");
    println!();

    let runtime = Runner::new()?;

    // Slot the setup hook will populate with the cuda adapter so it
    // (and the host-side `Arc`s it holds) outlives the runtime's start
    // → stop cycle. The CUDA adapter has no per-acquire host work
    // (#588 / `subprocess-rhi-parity.md`) so unlike cpu-readback there
    // is no `set_cuda_bridge` to wire — keeping the adapter alive is
    // about preserving the OPAQUE_FD `VkBuffer` + timeline `Arc`s that
    // surface-share's daemon dup'd from on registration.
    let adapter_slot: Arc<Mutex<Option<Arc<HostAdapter>>>> =
        Arc::new(Mutex::new(None));

    {
        let adapter_slot = Arc::clone(&adapter_slot);
        runtime.install_setup_hook(move |gpu| {
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let adapter: Arc<HostAdapter> =
                Arc::new(CudaSurfaceAdapter::new(Arc::clone(&host_device)));

            register_host_surface(&adapter, gpu).map_err(|e| {
                Error::Configuration(format!("register_host_surface: {e}"))
            })?;

            *adapter_slot.lock().unwrap() = Some(adapter);
            println!(
                "✓ CUDA adapter registered, OPAQUE_FD buffer + exportable timeline \
                 surface-share-published"
            );
            Ok(())
        });
    }

    // Load the BgraFileSource processor from `@tatolab/debug-utilities`
    // at runtime — `cargo xtask build-plugins --package @tatolab/debug-utilities`
    // must have run first.
    runtime
        .load_workspace_packages(["@tatolab/debug-utilities"])
        .map_err(streamlib::sdk::error::Error::from)?;

    // Load the polyglot processors declaratively. The runner's
    // `streamlib.yaml` declares both the Python and Deno sub-packages
    // via `patch:` `path:` overrides; `load_project` walks the manifest,
    // resolves both, and registers each package's processors + schemas.
    // The runner then picks which one to instantiate via
    // `schema_ident_any_version!` based on `--runtime`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    runtime.load_project(&manifest_dir)?;

    // Trigger source: a tiny BGRA fixture that drives Videoframes
    // through the pipeline so the polyglot processor's `process()` is
    // invoked. Frame contents are unused (the polyglot processor works
    // on the pre-registered cuda OPAQUE_FD surface, not the trigger
    // frame's pixel buffer). Same shape as cpu-readback-blur.
    let fixture_path = write_trigger_fixture()
        .map_err(Error::Configuration)?;

    let fixture_path_str = fixture_path
        .to_str()
        .ok_or_else(|| Error::Configuration("fixture path has non-utf8 component".into()))?;
    let source = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "debug-utilities", "BgraFileSource", "1.0.0"),
        serde_json::json!({
            "file_path": fixture_path_str,
            "width": 4,
            "height": 4,
            "fps": 5,
            "frame_count": 3,
        }),
    ))?;
    println!("+ BgraFileSource: {source}");

    let inference_config = serde_json::json!({
        "cuda_surface_id": SCENARIO_SURFACE_ID,
        "width": SURFACE_WIDTH,
        "height": SURFACE_HEIGHT,
        "channels": BYTES_PER_PIXEL,
        "output_path": output_png.to_string_lossy(),
    });
    let inference = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_ident()?,
        inference_config,
    ))?;
    println!("+ Inference:      {inference}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&inference, "video_in"),
    )?;
    println!(
        "\nPipeline: BgraFileSource → {} cuda-inference\n",
        runtime_kind.as_str()
    );

    println!("Starting pipeline...");
    runtime.start()?;

    // Give the polyglot processor time to download model weights
    // (yolov8n.pt, ~6 MB) and a test image (bus.jpg, ~140 KB) on
    // first run, plus run inference. Subsequent runs are much faster
    // because ultralytics caches the weights under ~/.cache.
    println!(
        "Waiting up to {timeout_secs}s for the polyglot processor to finish..."
    );
    std::thread::sleep(Duration::from_secs(timeout_secs));

    println!("Stopping pipeline...");
    runtime.stop()?;

    let adapter_alive = adapter_slot.lock().unwrap().is_some();
    println!(
        "\n✓ Scenario complete. Adapter held alive through stop: {adapter_alive}"
    );
    println!("Inspect the output PNG with the Read tool: {}", output_png.display());

    Ok(())
}

/// Allocate the OPAQUE_FD HOST_VISIBLE staging `VkBuffer`, allocate an
/// exportable timeline semaphore, register both via surface-share so
/// the subprocess can import them in one `check_out`, and register
/// the pair with the cuda adapter under [`SCENARIO_SURFACE_ID`].
fn register_host_surface(
    adapter: &Arc<HostAdapter>,
    gpu: &GpuContext,
) -> std::result::Result<(), String> {
    let host_device = adapter.device();
    let buffer_size = (SURFACE_WIDTH * SURFACE_HEIGHT * BYTES_PER_PIXEL) as usize;

    // 1. Allocate the OPAQUE_FD-exportable HOST_VISIBLE staging buffer.
    //    OPAQUE_FD (not DMA-BUF) is required because DLPack consumers
    //    (PyTorch / NumPy / JAX `from_dlpack`) need a flat `void*`
    //    device pointer from `cudaExternalMemoryGetMappedBuffer`,
    //    which only works when the source memory is a `VkBuffer`
    //    exported as OPAQUE_FD. See
    //    `docs/architecture/subprocess-rhi-parity.md` →
    //    "OPAQUE_FD VkBuffer import (cuda — #588)".
    let pixel_buffer = HostVulkanBuffer::new_opaque_fd_export(host_device, (SURFACE_WIDTH as u64) * (SURFACE_HEIGHT as u64) * (BYTES_PER_PIXEL as u64))
    .map_err(|e| format!("HostVulkanBuffer::new_opaque_fd_export: {e}"))?;
    let pixel_buffer_arc = Arc::new(pixel_buffer);
    let pixel_buffer_rhi = PixelBuffer::from_host_vulkan_buffer(
        Arc::clone(&pixel_buffer_arc),
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
        BYTES_PER_PIXEL,
        PixelFormat::Bgra32,
    );

    // 2. Allocate the exportable timeline semaphore (initial value 0).
    //    First subprocess `acquire_*` will wait on 0 → satisfied
    //    immediately; release advances to 1.
    let timeline = Arc::new(
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .map_err(|e| format!("HostVulkanTimelineSemaphore::new_exportable: {e}"))?,
    );

    // 3. Pre-fill the buffer with a known sentinel pattern (all zeros)
    //    so the subprocess's first `acquire_read` observes deterministic
    //    bytes if the polyglot processor's `acquire_write` upload path
    //    ever gets skipped.
    //
    //    SAFETY: the OPAQUE_FD buffer is HOST_VISIBLE | HOST_COHERENT
    //    and the mapped pointer stays valid for the buffer's lifetime
    //    (the `Arc` we hold keeps it alive). No other owner has a
    //    handle to the buffer yet — register_pixel_buffer_with_timeline
    //    is what publishes it to the daemon — so this write is
    //    uncontended.
    unsafe {
        std::ptr::write_bytes(pixel_buffer_arc.mapped_ptr(), 0u8, buffer_size);
    }

    // 4. Register staging buffer + timeline with the surface-share
    //    service. `register_pixel_buffer_with_timeline` inspects the
    //    pixel buffer's `RhiExternalHandle` variant and stamps
    //    `handle_type: "opaque_fd"` on the wire when the underlying
    //    memory is OPAQUE_FD-exported (#588 surface_store extension).
    let surface_store = gpu
        .surface_store()
        .ok_or_else(|| "GpuContext has no surface_store".to_string())?;
    surface_store
        .register_pixel_buffer_with_timeline(
            &SCENARIO_SURFACE_ID.to_string(),
            &pixel_buffer_rhi,
            Some(timeline.as_ref()),
        )
        .map_err(|e| format!("register_pixel_buffer_with_timeline: {e}"))?;

    // 5. Register the surface with the host-side cuda adapter.
    adapter
        .register_host_surface(
            SCENARIO_SURFACE_ID,
            HostSurfaceRegistration::<HostMarker> {
                pixel_buffer: pixel_buffer_arc,
                timeline,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .map_err(|e| format!("register_host_surface: {e:?}"))?;
    Ok(())
}

/// Write a minimal BGRA fixture file. BgraFileSource reads it
/// frame-by-frame; the resulting Videoframes are the trigger that
/// drives the polyglot processor's `process()` call. Frame contents
/// are unused — the polyglot processor works on the pre-registered
/// cuda OPAQUE_FD surface, not the trigger frame's pixel buffer.
fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("cuda-inference-trigger.bgra");
    let mut f =
        File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}
