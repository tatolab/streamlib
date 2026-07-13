// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot CUDA texture-interop scenario.
//!
//! Validates the OPAQUE_FD `VkImage` registration path end-to-end
//! through a drone-racing-relevant ML pipeline. The host loads the
//! ultralytics `bus.jpg` demo image, applies a pure-Rust polynomial
//! radial-distortion (fisheye barrel) warp, allocates a DEVICE_LOCAL
//! OPAQUE_FD `VkImage` (`Rgba8Unorm`, `VK_IMAGE_TILING_OPTIMAL`, no
//! DRM modifier), uploads the warped pixels via `vkCmdCopyBufferToImage`
//! from a HOST_VISIBLE staging buffer, and registers the image with
//! the cuda adapter + the surface-share service. The Python subprocess
//! imports the surface as a `cudaTextureObject_t`, undistorts via a
//! `cupy.RawKernel` (hardware-bilinear sampling — the canonical TMU
//! workload), runs YOLOv8n detection, and writes an annotated PNG.
//!
//! Sibling of `polyglot-cuda-inference` (which validates the DLPack
//! `VkBuffer` flat-tensor path). The Rust runner sits at the example
//! root and the Python processor lives in a sibling `python/`
//! sub-package with its own `streamlib.yaml`; it lives in this app's
//! `streamlib_modules/` folder (populated by `./setup.sh`) and the
//! runtime lazily discovers + loads it on the first `processor_type_ref!`
//! reference — no module-loading call in app code.
//!
//! Run:
//!   cargo run -p cuda-fisheye-detection-scenario -- \
//!       --output=/tmp/cuda-fisheye-detected.png
//!
//! The annotated PNG goes to `--output` (default
//! `/tmp/cuda-fisheye-detected.png`). Inspect with the Read tool.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::host_rhi::{
    HostMarker, HostVulkanBuffer, HostVulkanTexture, HostVulkanTimelineSemaphore, ImageCopyRegion,
    RhiCommandRecorder, VulkanAccess, VulkanStage,
};
use streamlib::sdk::engine::{HostGpuDeviceExt, HostSurfaceStoreExt, HostTextureExt};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::rhi::{StorageBuffer, Texture, TextureDescriptor, TextureFormat, VulkanLayout};
use streamlib::sdk::runtime::Runner;
use streamlib_adapter_abi::SurfaceId;
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostImageSurfaceRegistration};

/// Host-assigned surface id the python processor receives via config
/// and threads through `CudaContext.acquire_texture`.
const SCENARIO_SURFACE_ID: SurfaceId = 1;

/// Image dimensions. 640x640 matches YOLOv8n's default `imgsz` so the
/// model receives the recovered texture without host- or model-side
/// resizing.
const SURFACE_WIDTH: u32 = 640;
const SURFACE_HEIGHT: u32 = 640;
const BYTES_PER_PIXEL: u32 = 4;

/// Polynomial radial-distortion coefficients. Negative `K1` produces
/// the classic fisheye barrel look — for each warped pixel, samples
/// from a source pixel closer to the image center, so source content
/// gets pushed outward into the corners and the image curves inward
/// at the edges. `K2` is a small second-order correction that makes
/// the warp look more like a real fisheye lens (less linear at high
/// radius). Picked to be visually obvious without being so extreme
/// the undistortion can't recover enough signal for YOLOv8n.
// Tuned for high-coverage recovery: k1=-0.1, k2=0 produces a gentle
// barrel where the forward warp samples source content out to
// r_src ≈ 1.131 (vs sqrt(2) ≈ 1.414 at the corners), so the inverse
// can recover ≥90% of pixels — only the very corners of the source
// (where the rectangular sensor would be physically dark in a real
// fisheye lens) are masked. Mentally swap to k1=-0.25, k2=-0.05 and
// the recoverable annulus collapses to r_u ≤ 0.7 (38% of pixels);
// the math still works, just throws away more.
const FISHEYE_K1: f32 = -0.10;
const FISHEYE_K2: f32 = 0.0;

type HostAdapter = CudaSurfaceAdapter<streamlib::sdk::engine::host_rhi::HostVulkanDevice>;

/// Ultralytics' DOTA8 sample dataset — 8 clean aerial images from
/// DOTAv1 (no baked-in dataset annotations, unlike the published
/// VisDrone sample). We extract one image (`P0861__1024__0___1648.jpg`,
/// a marina + parking lot with many cars and boats — drone-perspective
/// content YOLOv8n's COCO training detects reliably). Stable URL on
/// the `ultralytics/assets` GitHub release.
const TEST_DATASET_ZIP_URL: &str =
    "https://github.com/ultralytics/assets/releases/download/v0.0.0/dota8.zip";
const TEST_IMAGE_INSIDE_ZIP: &str = "dota8/images/train/P0861__1024__0___1648.jpg";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut output_png = PathBuf::from("/tmp/cuda-fisheye-detected.png");
    let mut timeout_secs: u64 = 60;
    for a in &args {
        if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        } else if let Some(value) = a.strip_prefix("--timeout-secs=") {
            timeout_secs = value
                .parse()
                .map_err(|e| Error::Configuration(format!("invalid --timeout-secs: {e}")))?;
        }
    }

    println!("=== Polyglot CUDA fisheye-detection scenario ===");
    println!(
        "Surface:     {SURFACE_WIDTH}x{SURFACE_HEIGHT} Rgba8Unorm OPAQUE_FD VkImage (id {SCENARIO_SURFACE_ID})"
    );
    println!("Distortion:  k1={FISHEYE_K1} k2={FISHEYE_K2} (forward fisheye barrel)");
    println!("Output PNG:  {}", output_png.display());
    println!("Timeout:     {timeout_secs}s");
    println!();

    let runtime = Runner::with_auto_build()?;

    // Setup-hook captures keep the adapter `Arc` alive across start
    // → stop. The CUDA adapter has no per-acquire host work (no
    // bridge to wire), so the slot's sole job is to retain the
    // OPAQUE_FD `VkImage` + timeline `Arc`s that surface-share
    // duplicated from on registration.
    let adapter_slot: Arc<Mutex<Option<Arc<HostAdapter>>>> = Arc::new(Mutex::new(None));
    {
        let adapter_slot = Arc::clone(&adapter_slot);
        let output_png_for_hook = output_png.clone();
        runtime.install_setup_hook(move |gpu| {
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let adapter: Arc<HostAdapter> =
                Arc::new(CudaSurfaceAdapter::new(Arc::clone(&host_device)));

            register_warped_host_surface(&adapter, gpu, &output_png_for_hook)
                .map_err(|e| Error::Configuration(format!("register_warped_host_surface: {e}")))?;

            *adapter_slot.lock().unwrap() = Some(adapter);
            println!(
                "✓ CUDA adapter registered, OPAQUE_FD VkImage + exportable timeline \
                 surface-share-published"
            );
            Ok(())
        });
    }

    // No module-loading calls: `@tatolab/debug-utilities` (the
    // `BgraFileSource` trigger) and the example-local `./python` package
    // (`@tatolab/cuda-fisheye-python`) live in this app's `streamlib_modules/`
    // folder (populated by `./setup.sh`). The runtime lazily discovers +
    // loads each on the first `processor_type_ref!` reference.

    // Trigger source — emits a tiny BGRA fixture frame whose contents
    // are unused. The polyglot processor runs against the
    // pre-registered cuda OPAQUE_FD surface, not the trigger frame's
    // pixel buffer. Same shape as `polyglot-cuda-inference`.
    let fixture_path = write_trigger_fixture().map_err(Error::Configuration)?;
    let source = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "debug-utilities", "BgraFileSource"),
        serde_json::json!({
            "file_path": fixture_path
                .to_str()
                .ok_or_else(|| Error::Configuration("fixture path has non-utf8 component".into()))?,
            "width": 4,
            "height": 4,
            "fps": 5,
            "frame_count": 3,
        }),
    ))?;
    println!("+ BgraFileSource: {source}");

    let undistort_ident = processor_type_ref!(
        "tatolab",
        "cuda-fisheye-python",
        "CudaFisheyeUndistortion"
    );
    let reference_path = cache_subpath("warped-reference.rgba").map_err(Error::Configuration)?;
    let stages_dir = output_png
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let undistort_config = serde_json::json!({
        "cuda_surface_id": SCENARIO_SURFACE_ID,
        "width": SURFACE_WIDTH,
        "height": SURFACE_HEIGHT,
        "channels": BYTES_PER_PIXEL,
        "fisheye_k1": FISHEYE_K1,
        "fisheye_k2": FISHEYE_K2,
        "output_path": output_png.to_string_lossy(),
        "reference_warped_rgba_path": reference_path.to_string_lossy(),
        "stages_dir": stages_dir.to_string_lossy(),
    });
    let undistort = runtime.add_processor(ProcessorSpec::new(undistort_ident, undistort_config))?;
    println!("+ CudaFisheyeUndistortion: {undistort}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&undistort, "video_in"),
    )?;
    println!("\nPipeline: BgraFileSource → CudaFisheyeUndistortion\n");

    println!("Starting pipeline...");
    runtime.start()?;

    println!("Waiting up to {timeout_secs}s for the polyglot processor to finish...");
    std::thread::sleep(Duration::from_secs(timeout_secs));

    println!("Stopping pipeline...");
    runtime.stop()?;

    let adapter_alive = adapter_slot.lock().unwrap().is_some();
    println!("\n✓ Scenario complete. Adapter held alive through stop: {adapter_alive}");
    println!(
        "Inspect the output PNG with the Read tool: {}",
        output_png.display()
    );

    Ok(())
}

/// Load `bus.jpg` (caching it in `~/.cache/streamlib-cuda-fisheye/`),
/// resize to the scenario surface dimensions, apply a pure-Rust
/// forward fisheye warp, upload the warped pixels into a freshly
/// allocated OPAQUE_FD `VkImage` via `vkCmdCopyBufferToImage`, and
/// register the image + an exportable timeline with both the cuda
/// adapter and the surface-share service.
fn register_warped_host_surface(
    adapter: &Arc<HostAdapter>,
    gpu: &GpuContext,
    output_png: &std::path::Path,
) -> std::result::Result<(), String> {
    let host_device = adapter.device();
    let pixel_count = (SURFACE_WIDTH as usize) * (SURFACE_HEIGHT as usize);
    let byte_count = pixel_count * (BYTES_PER_PIXEL as usize);

    // 1. Decode + resize the source image. JPEG is plenty — the
    //    `image` crate's default features are off so we only pay the
    //    JPEG + PNG decoders this scenario actually needs.
    let source_rgba = load_resized_test_image_rgba(SURFACE_WIDTH, SURFACE_HEIGHT)?;
    if source_rgba.len() != byte_count {
        return Err(format!(
            "source image decode produced {} bytes, expected {}",
            source_rgba.len(),
            byte_count
        ));
    }

    // 2. Apply the fisheye warp. Pure-Rust polynomial radial pull —
    //    for each output pixel, sample the source at a radius scaled
    //    by `(1 + k1*r^2 + k2*r^4)`. Negative `k1` pulls samples
    //    toward the center, producing the classic barrel look.
    let warped_rgba = apply_fisheye_warp(
        &source_rgba,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
        FISHEYE_K1,
        FISHEYE_K2,
    );

    // 2a. Persist the warped CPU-side reference so the Python processor
    //     can byte-compare it against the texture path's identity-sample
    //     output. That comparison is what locks "the bytes the host
    //     wrote are the bytes CUDA reads through the imported texture"
    //     — the strongest correctness gate this example provides.
    let reference_path = cache_subpath("warped-reference.rgba")?;
    std::fs::write(&reference_path, &warped_rgba)
        .map_err(|e| format!("write reference rgba {}: {e}", reference_path.display()))?;

    // 2b. Persist the source + warped images as PNGs so a human (or a
    //     Telegram reply) can inspect every stage of the chain:
    //     source.png (un-warped), warped.png (host-side fisheye applied),
    //     recovered.png (Python writes after the undistortion kernel,
    //     before YOLO), detected.png (YOLO-annotated). Files live in
    //     the same directory as `output_png` so a `--output=<dir>/x.png`
    //     CLI flag implicitly drops every stage alongside it.
    let stages_dir = output_png
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&stages_dir)
        .map_err(|e| format!("create stages dir {}: {e}", stages_dir.display()))?;
    save_png(
        &source_rgba,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
        &stages_dir.join("source.png"),
    )?;
    save_png(
        &warped_rgba,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
        &stages_dir.join("warped.png"),
    )?;

    // 3. HOST_VISIBLE staging buffer — the warped pixels go here so
    //    `vkCmdCopyBufferToImage` can DMA them into the DEVICE_LOCAL
    //    `VkImage`. Wrap as `StorageBuffer` so the `RhiCommandRecorder`
    //    helpers accept it via the `VulkanBufferLike` trait.
    let staging_host = HostVulkanBuffer::new(host_device, byte_count as u64)
        .map_err(|e| format!("HostVulkanBuffer::new: {e}"))?;
    // SAFETY: the staging buffer is HOST_VISIBLE | HOST_COHERENT and
    // we hold the sole reference to it. No other writer can race the
    // pre-upload memcpy.
    unsafe {
        std::ptr::copy_nonoverlapping(warped_rgba.as_ptr(), staging_host.mapped_ptr(), byte_count);
    }
    let staging = StorageBuffer::from_host_vulkan_buffer(Arc::new(staging_host));

    // 4. DEVICE_LOCAL OPAQUE_FD `VkImage`. The cuda adapter's image
    //    registration validates the format gate at registration time;
    //    the host RHI constructor enforces the same gate, so passing
    //    a non-CUDA-mappable `TextureFormat` fails here, not late at
    //    the cdylib's `cudaExternalMemoryGetMappedMipmappedArray` call.
    let texture_descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm);
    let host_texture = HostVulkanTexture::new_opaque_fd_export(host_device, &texture_descriptor)
        .map_err(|e| format!("HostVulkanTexture::new_opaque_fd_export: {e}"))?;
    let texture: Texture = Texture::from_vulkan(host_texture);
    let texture_arc = Arc::clone(texture.vulkan_inner());

    // 5. Exportable timelines — one per single-writer edge per
    //    `docs/architecture/adapter-timeline-single-writer.md`. Initial
    //    values 0; the first subprocess `acquire_texture` waits on 0,
    //    which is satisfied immediately. Each release advances the
    //    edge's writer-side counter by 1.
    let produce_done = Arc::new(
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0).map_err(|e| {
            format!("HostVulkanTimelineSemaphore::new_exportable (produce_done): {e}")
        })?,
    );
    let consume_done = Arc::new(
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0).map_err(|e| {
            format!("HostVulkanTimelineSemaphore::new_exportable (consume_done): {e}")
        })?,
    );

    // 6. One-shot upload + layout transition via the canonical
    //    `RhiCommandRecorder` helper. Transition UNDEFINED →
    //    TRANSFER_DST_OPTIMAL, copy from the staging buffer, then
    //    transition TRANSFER_DST_OPTIMAL → SHADER_READ_ONLY_OPTIMAL.
    //    `submit_and_wait` blocks until the GPU drains — this is a
    //    one-shot setup path, not a per-frame hot path.
    let mut recorder = RhiCommandRecorder::new(host_device, "cuda-fisheye-upload")
        .map_err(|e| format!("RhiCommandRecorder::new: {e}"))?;
    recorder
        .begin()
        .map_err(|e| format!("recorder.begin(): {e}"))?;
    recorder
        .record_image_barrier(
            &texture,
            VulkanLayout::UNDEFINED,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanStage::TOP_OF_PIPE,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::NONE,
            VulkanAccess::TRANSFER_WRITE,
        )
        .map_err(|e| format!("record_image_barrier(UNDEFINED → TRANSFER_DST): {e}"))?;
    let region = ImageCopyRegion::tightly_packed(SURFACE_WIDTH, SURFACE_HEIGHT);
    recorder
        .record_copy_buffer_to_image(
            &staging,
            &texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            region,
        )
        .map_err(|e| format!("record_copy_buffer_to_image: {e}"))?;
    recorder
        .record_image_barrier(
            &texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::SHADER_READ,
        )
        .map_err(|e| format!("record_image_barrier(TRANSFER_DST → SHADER_READ_ONLY): {e}"))?;
    recorder
        .submit_and_wait()
        .map_err(|e| format!("submit_and_wait: {e}"))?;

    // 7. Surface-share registration. `register_texture` dispatches
    //    internally on `is_opaque_fd_export()`: with the OPAQUE_FD
    //    flavor it ships the `VkImageCreateInfo` round-trip fields
    //    (#806) the cdylib's `cudaExternalMemoryGetMappedMipmappedArray`
    //    import needs.
    let surface_store = gpu
        .surface_store()
        .ok_or_else(|| "GpuContext has no surface_store".to_string())?;
    surface_store
        .register_texture(
            &SCENARIO_SURFACE_ID.to_string(),
            &texture,
            Some(produce_done.as_ref()),
            Some(consume_done.as_ref()),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        )
        .map_err(|e| format!("surface_store.register_texture: {e}"))?;

    // 8. Cuda adapter registration. The adapter owns the host-side
    //    `Arc<HostVulkanTexture>` + per-edge timeline `Arc`s for the
    //    surface's lifetime so the underlying GPU memory stays alive
    //    while CUDA references the imported handles.
    adapter
        .register_host_image_surface(
            SCENARIO_SURFACE_ID,
            HostImageSurfaceRegistration::<HostMarker> {
                texture: texture_arc,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            },
        )
        .map_err(|e| format!("register_host_image_surface: {e:?}"))?;

    Ok(())
}

/// Download (or read from cache) the DOTA8 aerial sample, decode +
/// resize to `(width, height)`, and return a row-major RGBA8 byte
/// buffer (alpha = 255).
///
/// The DOTA8 zip ships 8 unannotated aerial JPGs on a stable
/// `ultralytics/assets` GitHub release. We extract one (a marina +
/// parking lot — many cars and boats, drone-perspective, YOLOv8n-
/// detectable) and cache the JPG for subsequent runs.
fn load_resized_test_image_rgba(width: u32, height: u32) -> std::result::Result<Vec<u8>, String> {
    use image::{GenericImageView, imageops::FilterType};

    let jpg_path = cache_subpath("dota8-marina.jpg")?;
    if !jpg_path.exists() {
        let zip_path = cache_subpath("dota8.zip")?;
        if !zip_path.exists() {
            println!(
                "[host] downloading drone fixture (DOTA8 aerial sample set) \
                 {TEST_DATASET_ZIP_URL} -> {} (one-time, ~1.4 MB)",
                zip_path.display()
            );
            download_to_file(TEST_DATASET_ZIP_URL, &zip_path)?;
        }
        println!(
            "[host] extracting {} from {} -> {} (one-time)",
            TEST_IMAGE_INSIDE_ZIP,
            zip_path.display(),
            jpg_path.display(),
        );
        let status = std::process::Command::new("unzip")
            .arg("-p")
            .arg(&zip_path)
            .arg(TEST_IMAGE_INSIDE_ZIP)
            .stdout(
                std::fs::File::create(&jpg_path)
                    .map_err(|e| format!("create {}: {e}", jpg_path.display()))?,
            )
            .status()
            .map_err(|e| format!("spawn unzip: {e}"))?;
        if !status.success() {
            return Err(format!("unzip exited non-zero: {status}"));
        }
    }

    let img =
        image::open(&jpg_path).map_err(|e| format!("image::open({}): {e}", jpg_path.display()))?;
    let (src_w, src_h) = img.dimensions();
    let resized = if src_w == width && src_h == height {
        img.to_rgba8()
    } else {
        img.resize_exact(width, height, FilterType::Triangle)
            .to_rgba8()
    };
    Ok(resized.into_raw())
}

/// Encode `rgba` (row-major 8-bit RGBA) as a PNG at `path`. Used to
/// persist every stage of the source / warped / recovered chain so a
/// human can inspect the visual proof of the fisheye round-trip.
fn save_png(
    rgba: &[u8],
    width: u32,
    height: u32,
    path: &std::path::Path,
) -> std::result::Result<(), String> {
    image::save_buffer(path, rgba, width, height, image::ColorType::Rgba8)
        .map_err(|e| format!("save_png {}: {e}", path.display()))
}

/// `~/.cache/` on Linux. We avoid pulling the `dirs` crate for one
/// path lookup.
fn dirs_cache_path() -> std::result::Result<PathBuf, String> {
    if let Ok(v) = std::env::var("XDG_CACHE_HOME") {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is unset".to_string())?;
    Ok(PathBuf::from(home).join(".cache"))
}

/// Resolve a path under `~/.cache/streamlib-cuda-fisheye/<file>`. The
/// processor cache directory is created on demand.
fn cache_subpath(file: &str) -> std::result::Result<PathBuf, String> {
    let dir = dirs_cache_path()?.join("streamlib-cuda-fisheye");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create cache dir {}: {e}", dir.display()))?;
    Ok(dir.join(file))
}

/// Minimal HTTPS GET via `curl` shelling out — keeps the runner
/// dep-free of `reqwest` / `ureq` for one fixture download.
fn download_to_file(url: &str, dst: &std::path::Path) -> std::result::Result<(), String> {
    let status = std::process::Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location", "-o"])
        .arg(dst)
        .arg(url)
        .status()
        .map_err(|e| format!("spawn curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl exited non-zero: {status}"));
    }
    Ok(())
}

/// Apply a forward polynomial radial fisheye warp to a row-major
/// RGBA8 image. For each destination pixel `(xd, yd)`, samples the
/// source at a position scaled by `1 + k1*r^2 + k2*r^4` (normalized
/// radius). Negative coefficients pull samples toward the center —
/// the classic barrel/fisheye look. Bilinear sampling; pixels that
/// would sample outside the source bounds emit transparent black.
fn apply_fisheye_warp(source: &[u8], width: u32, height: u32, k1: f32, k2: f32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let cx = (width as f32 - 1.0) * 0.5;
    let cy = (height as f32 - 1.0) * 0.5;
    let r_max = cx.min(cy);
    let inv_r_max = 1.0 / r_max;

    let mut dst = vec![0u8; source.len()];
    for yd in 0..h {
        for xd in 0..w {
            let nx = (xd as f32 - cx) * inv_r_max;
            let ny = (yd as f32 - cy) * inv_r_max;
            let r2 = nx * nx + ny * ny;
            let scale = 1.0 + k1 * r2 + k2 * r2 * r2;
            let xs = cx + (xd as f32 - cx) * scale;
            let ys = cy + (yd as f32 - cy) * scale;
            let out_idx = (yd * w + xd) * 4;
            let pixel = bilinear_sample_rgba(source, w, h, xs, ys);
            dst[out_idx..out_idx + 4].copy_from_slice(&pixel);
        }
    }
    dst
}

/// Bilinearly sample an RGBA8 image at fractional coords. Returns
/// transparent black for samples outside `[0, w-1] x [0, h-1]`.
fn bilinear_sample_rgba(src: &[u8], w: usize, h: usize, x: f32, y: f32) -> [u8; 4] {
    if !x.is_finite() || !y.is_finite() {
        return [0; 4];
    }
    if x < 0.0 || y < 0.0 || x > (w as f32 - 1.0) || y > (h as f32 - 1.0) {
        return [0; 4];
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let dx = x - x0 as f32;
    let dy = y - y0 as f32;
    let mut out = [0u8; 4];
    for c in 0..4 {
        let a = src[(y0 * w + x0) * 4 + c] as f32;
        let b = src[(y0 * w + x1) * 4 + c] as f32;
        let cc = src[(y1 * w + x0) * 4 + c] as f32;
        let d = src[(y1 * w + x1) * 4 + c] as f32;
        let top = a + (b - a) * dx;
        let bot = cc + (d - cc) * dx;
        out[c] = (top + (bot - top) * dy).clamp(0.0, 255.0) as u8;
    }
    out
}

/// Write a minimal BGRA fixture file. `BgraFileSource` reads it
/// frame-by-frame; the resulting `Videoframes` are the trigger that
/// drives the polyglot processor's `process()` call. Frame contents
/// are unused — the processor works on the pre-registered cuda
/// OPAQUE_FD surface, not the trigger frame's pixel buffer.
fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;
    let path = std::env::temp_dir().join("cuda-fisheye-trigger.bgra");
    let mut f = File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}
