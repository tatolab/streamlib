// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in V4L2 camera source: capture → GPU color-convert → publish.
//!
//! Camera→GPU transport is zero-copy DMA-BUF import when the device exports
//! it, transparent CPU-upload (MMAP + memcpy) fallback otherwise, selected
//! automatically — no configuration dial.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use streamlib::sdk::color::{
    ColorSpaceKind, MatrixId, PrimariesId, RangeId, TransferId, resolve_color_defaults,
};
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::host_rhi::{
    HostSurfaceStoreExt, HostVulkanTimelineSemaphore, ImageCopyRegion, RhiCommandRecorder,
    VulkanAccess, VulkanStage,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{
    PixelFormat, RhiColorConverter, SourceLayoutInfo, StorageBuffer, Texture, TextureFormat,
    VulkanLayout,
};

use v4l::FourCC;
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;

use crate::video_frame::{ColorInfo, Matrix, Primaries, Range, Transfer, VideoFrame};

/// Number of ring textures for the GPU-resident pipeline (matches
/// MAX_FRAMES_IN_FLIGHT).
const RING_TEXTURE_COUNT: usize = 2;

/// Number of V4L2 mmap buffers to request.
const V4L2_BUFFER_COUNT: u32 = 4;

/// Configuration for [`CameraSource`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CameraSourceConfig {
    /// V4L2 device path (`/dev/video0`). Absent: the first capture-capable
    /// device found.
    #[serde(default)]
    pub device_id: Option<String>,
    /// Resolution cap; the negotiated format is clamped to fit. Default 1920.
    #[serde(default)]
    pub max_width: Option<u32>,
    /// Resolution cap; the negotiated format is clamped to fit. Default 1080.
    #[serde(default)]
    pub max_height: Option<u32>,
}

/// A V4L2 capture device, as enumerated from `/dev/video*`.
#[derive(Debug, Clone)]
pub struct CameraCaptureDevice {
    /// Device path (`/dev/video0`).
    pub id: String,
    /// Driver-reported card name.
    pub name: String,
}

/// Enumerate available V4L2 capture devices.
pub fn list_camera_capture_devices() -> Result<Vec<CameraCaptureDevice>> {
    let mut devices = Vec::new();
    for entry in std::fs::read_dir("/dev")
        .map_err(|e| Error::Configuration(format!("Failed to read /dev: {}", e)))?
    {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("video") {
            continue;
        }
        let Ok(dev) = v4l::Device::with_path(&path) else {
            continue;
        };
        let Ok(caps) = dev.query_caps() else { continue };
        if !caps
            .capabilities
            .contains(v4l::capability::Flags::VIDEO_CAPTURE)
        {
            continue;
        }
        devices.push(CameraCaptureDevice {
            id: path.to_string_lossy().to_string(),
            name: caps.card,
        });
    }
    Ok(devices)
}

#[streamlib::sdk::processor(
    "@tatolab/media-builtins/CameraSource",
    description = "Captures live video from a V4L2 camera (zero-copy DMA-BUF when the device exports it, CPU upload otherwise)",
    execution = manual,
    scheduling = high,
    config = crate::camera_source::CameraSourceConfig,
    output("video", any, description = "Live camera video frames"),
)]
pub struct CameraSource {
    camera_name: String,
    gpu_context: Option<GpuContextLimitedAccess>,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    capture_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl ManualProcessor for CameraSource::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The capture thread needs an owned handle that escapes into the
        // thread closure, so the clone at setup is load-bearing here.
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let frame_count = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!(
            "CameraSource {}: teardown ({} frames)",
            self.camera_name,
            frame_count
        );
        self.is_capturing.store(false, Ordering::Release);
        if let Some(handle) = self.capture_thread_handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            Error::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        let device_path = match &self.config.device_id {
            Some(id) => id.clone(),
            None => {
                let devices = list_camera_capture_devices()?;
                devices.first().map(|d| d.id.clone()).ok_or_else(|| {
                    Error::Configuration(
                        "No V4L2 capture devices found. Check that a camera is connected.".into(),
                    )
                })?
            }
        };

        let mut dev = v4l::Device::with_path(&device_path).map_err(|e| {
            Error::Configuration(format!(
                "Failed to open V4L2 device '{}': {}",
                device_path, e
            ))
        })?;

        let caps = dev.query_caps().map_err(|e| {
            Error::Configuration(format!("Failed to query device capabilities: {}", e))
        })?;
        self.camera_name = caps.card.clone();
        tracing::info!(
            "CameraSource: opened '{}' (driver: {}, bus: {})",
            caps.card,
            caps.driver,
            caps.bus
        );

        let current_fmt = dev
            .format()
            .map_err(|e| Error::Configuration(format!("Failed to read current format: {}", e)))?;

        // Negotiate format + resolution: enumerate frame sizes for NV12
        // (preferred) or YUYV, pick the highest resolution, then set_format.
        let fmt = negotiate_capture_format(&mut dev, current_fmt, &self.camera_name)?;

        // Cap capture resolution at config.max_width / max_height (defaults
        // 1920x1080 preserve the real-time-encoding guardrail; high-resolution
        // use cases opt in by raising the cap).
        let max_width = self.config.max_width.unwrap_or(1920);
        let max_height = self.config.max_height.unwrap_or(1080);
        let fmt = if fmt.width > max_width || fmt.height > max_height {
            let mut capped = fmt.clone();
            capped.width = max_width;
            capped.height = max_height;
            match dev.set_format(&capped) {
                Ok(f) => {
                    tracing::info!(
                        "CameraSource {}: capped resolution from {}x{} to {}x{}",
                        self.camera_name,
                        fmt.width,
                        fmt.height,
                        f.width,
                        f.height
                    );
                    f
                }
                Err(e) => {
                    tracing::warn!(
                        "CameraSource {}: failed to cap resolution to {}x{} ({}), using {}x{}",
                        self.camera_name,
                        max_width,
                        max_height,
                        e,
                        fmt.width,
                        fmt.height
                    );
                    fmt
                }
            }
        } else {
            fmt
        };

        let capture_width = fmt.width;
        let capture_height = fmt.height;
        let capture_fourcc = fmt.fourcc;

        tracing::info!(
            "CameraSource {}: capturing {}x{} {:?}",
            self.camera_name,
            capture_width,
            capture_height,
            capture_fourcc
        );

        let mut stream =
            v4l::io::mmap::Stream::with_buffers(&mut dev, Type::VideoCapture, V4L2_BUFFER_COUNT)
                .map_err(|e| {
                    Error::Configuration(format!("Failed to create V4L2 mmap stream: {}", e))
                })?;

        // Poll timeout so the capture thread can check is_capturing.
        stream.set_timeout(std::time::Duration::from_secs(1));

        let capture_fps: Option<u32> = match dev.params() {
            Ok(params) if params.interval.numerator > 0 => {
                Some(params.interval.denominator / params.interval.numerator)
            }
            _ => None,
        };

        self.is_capturing.store(true, Ordering::Release);

        let is_capturing = Arc::clone(&self.is_capturing);
        let frame_counter = Arc::clone(&self.frame_counter);
        let outputs: OutputWriter = self.outputs.clone();
        let camera_name = self.camera_name.clone();

        let handle = std::thread::Builder::new()
            .name(format!("v4l2-capture-{}", device_path))
            .spawn(move || {
                capture_thread_loop(
                    stream,
                    is_capturing,
                    frame_counter,
                    outputs,
                    gpu_context,
                    camera_name,
                    capture_width,
                    capture_height,
                    capture_fourcc,
                    capture_fps,
                );
            })
            .map_err(|e| Error::Configuration(format!("Failed to spawn capture thread: {}", e)))?;

        self.capture_thread_handle = Some(handle);

        tracing::info!(
            "CameraSource {}: V4L2 capture started ({}x{} {:?}, {} mmap buffers)",
            self.camera_name,
            capture_width,
            capture_height,
            capture_fourcc,
            V4L2_BUFFER_COUNT
        );
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.is_capturing.store(false, Ordering::Release);

        // Bounded wait: the capture thread can be inside a long timeline wait
        // or a V4L2 dequeue when stop arrives; both exit promptly under normal
        // conditions but a stalled GPU / driver state can stretch them out.
        // Detaching after a 2 s grace window keeps the runtime's shutdown
        // chain moving; the detached thread is reaped at process exit.
        if let Some(handle) = self.capture_thread_handle.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                tracing::warn!(
                    "CameraSource {}: capture thread did not exit within 2s, detaching",
                    self.camera_name
                );
            }
        }

        tracing::info!(
            "CameraSource {}: stopped ({} frames)",
            self.camera_name,
            self.frame_counter.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

/// Pick NV12 (preferred) or YUYV at the highest resolution the device offers.
fn negotiate_capture_format(
    dev: &mut v4l::Device,
    current_fmt: v4l::format::Format,
    camera_name: &str,
) -> Result<v4l::format::Format> {
    let nv12_fourcc = FourCC::new(b"NV12");
    let yuyv_fourcc = FourCC::new(b"YUYV");

    let highest_resolution = |framesizes: &[v4l::framesize::FrameSize]| -> Option<(u32, u32)> {
        let mut best_pixels = 0u64;
        let mut best = None;
        for fs in framesizes {
            let (w, h) = match &fs.size {
                v4l::framesize::FrameSizeEnum::Discrete(d) => (d.width, d.height),
                v4l::framesize::FrameSizeEnum::Stepwise(s) => (s.max_width, s.max_height),
            };
            let pixels = w as u64 * h as u64;
            if pixels > best_pixels {
                best_pixels = pixels;
                best = Some((w, h));
            }
        }
        best
    };

    // Try NV12 first.
    if let Ok(framesizes) = dev.enum_framesizes(nv12_fourcc)
        && let Some((best_w, best_h)) = highest_resolution(&framesizes)
    {
        let mut try_fmt = current_fmt.clone();
        try_fmt.fourcc = nv12_fourcc;
        try_fmt.width = best_w;
        try_fmt.height = best_h;
        if let Ok(f) = dev.set_format(&try_fmt)
            && f.fourcc == nv12_fourcc
        {
            tracing::info!(
                "CameraSource {}: NV12 available, highest resolution {}x{}",
                camera_name,
                f.width,
                f.height
            );
            return Ok(f);
        }
    }

    // Fall back to YUYV.
    tracing::info!(
        "CameraSource {}: NV12 not available, trying YUYV",
        camera_name
    );
    let (best_w, best_h) = dev
        .enum_framesizes(yuyv_fourcc)
        .ok()
        .and_then(|fs| highest_resolution(&fs))
        .unwrap_or((current_fmt.width, current_fmt.height));

    let mut try_fmt = current_fmt;
    try_fmt.fourcc = yuyv_fourcc;
    try_fmt.width = best_w;
    try_fmt.height = best_h;
    let f = dev.set_format(&try_fmt).map_err(|e| {
        Error::Configuration(format!(
            "Failed to set camera format (tried NV12, YUYV): {}",
            e
        ))
    })?;
    if f.fourcc != yuyv_fourcc {
        return Err(Error::Configuration(format!(
            "Camera does not support NV12 or YUYV (driver negotiated {:?})",
            f.fourcc
        )));
    }
    Ok(f)
}

struct CameraGpuResources {
    color_converter: RhiColorConverter,
    recorder: RhiCommandRecorder,
    timeline: Arc<HostVulkanTimelineSemaphore>,
    // Per-ring-slot single-writer-per-edge exportable timeline pairs —
    // `produce_done` signaled by the camera capture path, `consume_done` by
    // cross-process consumers. See
    // `docs/architecture/adapter-timeline-single-writer.md`.
    ring_produce_done: Vec<Arc<HostVulkanTimelineSemaphore>>,
    ring_consume_done: Vec<Arc<HostVulkanTimelineSemaphore>>,
    input_storage_buffers: Vec<StorageBuffer>,
    input_mapped_ptrs: [*mut u8; 2],
    ring_textures: Vec<Texture>,
    ring_texture_ids: Vec<String>,
    use_dmabuf: bool,
    dmabuf_imported_buffers: Vec<StorageBuffer>,
    dmabuf_fds: [i32; V4L2_BUFFER_COUNT as usize],
    vulkan_device_name: String,
    probe_skipped: bool,
}

#[allow(clippy::too_many_arguments)]
fn capture_thread_loop(
    mut stream: v4l::io::mmap::Stream,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    outputs: OutputWriter,
    gpu_context: GpuContextLimitedAccess,
    camera_name: String,
    width: u32,
    height: u32,
    fourcc: FourCC,
    capture_fps: Option<u32>,
) {
    let fourcc_bytes = fourcc.repr;

    match &fourcc_bytes {
        b"NV12" | b"YUYV" => {}
        _ => {
            tracing::error!(
                camera = camera_name,
                ?fourcc,
                "unsupported format — no GPU compute shader available",
            );
            return;
        }
    }

    let device_fd = stream.handle().fd();

    // V4L2 driver classification — virtual devices (vivid, v4l2loopback)
    // allocate buffers in CPU system memory, so DMA-BUF import into the GPU
    // may succeed at the API level but produce garbage data (cross-device
    // coherency). Skip the DMA-BUF probe for those — MMAP + memcpy is correct.
    let is_virtual_device = unsafe {
        let mut cap: v4l::v4l_sys::v4l2_capability = std::mem::zeroed();
        let result = libc::ioctl(
            device_fd,
            v4l::v4l2::vidioc::VIDIOC_QUERYCAP as libc::c_ulong,
            &mut cap,
        );
        if result == 0 {
            let driver = std::ffi::CStr::from_ptr(cap.driver.as_ptr().cast())
                .to_str()
                .unwrap_or("");
            let bus = std::ffi::CStr::from_ptr(cap.bus_info.as_ptr().cast())
                .to_str()
                .unwrap_or("");
            driver == "vivid" || driver == "v4l2 loopback" || bus.starts_with("platform:")
        } else {
            false
        }
    };

    // Query V4L2 format once at start: (1) the colorspace 4-tuple for
    // `ColorInfo`, (2) `bytesperline` for the source SSBO stride (vivid +
    // some UVC drivers report stride > width even for NV12), (3) `sizeimage`
    // for the SSBO allocation (must hold the full V4L2 frame including
    // padding). V4L2 contract: all three stay constant during streaming.
    let (cached_color_info, v4l2_bytes_per_line, v4l2_size_image): (ColorInfo, u32, u32) = unsafe {
        let mut v4l2_fmt: v4l::v4l_sys::v4l2_format = std::mem::zeroed();
        v4l2_fmt.type_ = v4l::buffer::Type::VideoCapture as u32;
        if libc::ioctl(
            device_fd,
            v4l::v4l2::vidioc::VIDIOC_G_FMT as libc::c_ulong,
            &mut v4l2_fmt,
        ) == 0
        {
            let pix = v4l2_fmt.fmt.pix;
            let color = crate::v4l2_color::v4l2_color_to_color_info(
                pix.colorspace,
                pix.xfer_func,
                // ycbcr_enc shares an anonymous union with hsv_enc; use the
                // YCbCr field since this path is YUV-only (NV12 / YUYV —
                // guarded by the FourCC match above). `__bindgen_anon_1` is
                // bindgen's name for the inner `union { ycbcr_enc; hsv_enc }`
                // — stable on v4l2-sys-mit 0.3.x; an upstream bump that adds
                // a second anonymous union would shift the suffix and stop
                // compiling, caught at build time.
                pix.__bindgen_anon_1.ycbcr_enc,
                pix.quantization,
            );
            (color, pix.bytesperline, pix.sizeimage)
        } else {
            // ioctl failed — emit "all unknown" colors and fall back to
            // tight-packed buffer sizing.
            let tight_bytes_per_line = match &fourcc_bytes {
                b"NV12" => width,
                b"YUYV" => width * 2,
                _ => unreachable!("guarded by FourCC match above"),
            };
            let tight_size_image = match &fourcc_bytes {
                b"NV12" => width * height * 3 / 2,
                b"YUYV" => width * height * 2,
                _ => unreachable!(),
            };
            (ColorInfo::default(), tight_bytes_per_line, tight_size_image)
        }
    };

    // SSBO must hold the full V4L2 frame including driver-side row padding
    // (vivid reports 3840-byte stride for 1920-wide NV12). Truncating to
    // tight-pack size memcpys only half the Y plane and reads garbage UV.
    let input_byte_size = v4l2_size_image as usize;
    let input_alloc_size = input_byte_size.div_ceil(4).wrapping_mul(4) as u64;

    // Source-buffer layout for the converter's push constants. NV12 uses
    // `bytesperline` for both planes (V4L2 bi-planar convention); YUYV is a
    // single packed plane.
    let src_layout = match &fourcc_bytes {
        b"NV12" => SourceLayoutInfo::nv12(
            v4l2_bytes_per_line,
            v4l2_bytes_per_line,
            v4l2_bytes_per_line * height,
        ),
        b"YUYV" => SourceLayoutInfo::yuyv(v4l2_bytes_per_line),
        _ => unreachable!("guarded by FourCC match above"),
    };
    tracing::info!(
        camera = camera_name,
        bytes_per_line = v4l2_bytes_per_line,
        size_image = v4l2_size_image,
        width,
        height,
        "V4L2 buffer layout"
    );

    // Resolve V4L2 ColorInfo to the fully-resolved description the color
    // converter's push constants use. Held for the life of the capture
    // thread — V4L2 colorspace doesn't change mid-stream.
    let resolved_color = resolve_color_defaults(
        cached_color_info.primaries.as_ref().map(primaries_id),
        cached_color_info.transfer.as_ref().map(transfer_id),
        cached_color_info.matrix.as_ref().map(matrix_id),
        cached_color_info.range.as_ref().map(range_id),
        ColorSpaceKind::Yuv,
    );

    // Map (fourcc, resolved range) to the canonical PixelFormat used as the
    // converter cache key. The push-constant matrix bakes the range
    // expansion in.
    let src_pixel_format = match (&fourcc_bytes, &resolved_color.range) {
        (b"NV12", RangeId::Full) => PixelFormat::Nv12FullRange,
        (b"NV12", _) => PixelFormat::Nv12VideoRange,
        (b"YUYV", _) => PixelFormat::Yuyv422,
        _ => unreachable!("guarded by FourCC match above"),
    };

    let setup_result = gpu_context.escalate(|full| {
        let caps = full.gpu_capabilities()?;
        let vulkan_device_name = caps.device_name.clone();

        let color_converter = full.color_converter(src_pixel_format, PixelFormat::Rgba32)?;
        let recorder = full.create_command_recorder("camera_capture")?;

        // Host-readback / display-wait timeline. Exportable so cross-process
        // consumers can wait on it; the camera only waits host-side.
        let timeline = full.create_exportable_timeline_semaphore(0)?;

        // Double-buffered HOST_VISIBLE input SSBOs (MMAP+memcpy fallback path).
        let mut input_storage_buffers: Vec<StorageBuffer> = Vec::with_capacity(2);
        let mut input_mapped_ptrs: [*mut u8; 2] = [std::ptr::null_mut(); 2];
        for slot in &mut input_mapped_ptrs {
            let buf = full.acquire_storage_buffer(input_alloc_size)?;
            *slot = buf.mapped_ptr();
            input_storage_buffers.push(buf);
        }

        // 2-texture DEVICE_LOCAL ring via the render-target DMA-BUF
        // allocation slot (tiled DRM modifier; usage superset is harmless).
        let mut ring_textures: Vec<Texture> = Vec::with_capacity(RING_TEXTURE_COUNT);
        let mut ring_texture_ids: Vec<String> = Vec::with_capacity(RING_TEXTURE_COUNT);
        let mut ring_produce_done = Vec::with_capacity(RING_TEXTURE_COUNT);
        let mut ring_consume_done = Vec::with_capacity(RING_TEXTURE_COUNT);
        for _ in 0..RING_TEXTURE_COUNT {
            let stream_texture =
                full.acquire_render_target_dma_buf_image(width, height, TextureFormat::Rgba8Unorm)?;
            ring_texture_ids.push(uuid::Uuid::new_v4().to_string());
            ring_textures.push(stream_texture);
            ring_produce_done.push(full.create_exportable_timeline_semaphore(0)?);
            ring_consume_done.push(full.create_exportable_timeline_semaphore(0)?);
        }

        // DMA-BUF probe — VIDIOC_EXPBUF on each V4L2 buffer + Vulkan import.
        // The import side is privileged (allocates VkDeviceMemory + binds) so
        // it stays inside the escalation; failure falls through to MMAP.
        let probe_skipped = !caps.supports_cross_device_dma_buf_probe;
        let mut use_dmabuf = false;
        let mut dmabuf_fds: [i32; V4L2_BUFFER_COUNT as usize] = [-1; V4L2_BUFFER_COUNT as usize];
        let mut dmabuf_imported_buffers: Vec<StorageBuffer> = Vec::new();
        if caps.supports_external_memory && !is_virtual_device && !probe_skipped {
            let mut imported: Vec<Option<StorageBuffer>> =
                (0..V4L2_BUFFER_COUNT as usize).map(|_| None).collect();
            let mut all_imported = true;
            for i in 0..V4L2_BUFFER_COUNT as usize {
                let fd: i32 = unsafe {
                    let mut expbuf: v4l::v4l_sys::v4l2_exportbuffer = std::mem::zeroed();
                    expbuf.type_ = v4l::buffer::Type::VideoCapture as u32;
                    expbuf.index = i as u32;
                    expbuf.flags = libc::O_CLOEXEC as u32;
                    let r = libc::ioctl(
                        device_fd,
                        v4l::v4l2::vidioc::VIDIOC_EXPBUF as libc::c_ulong,
                        &mut expbuf,
                    );
                    if r != 0 { -1 } else { expbuf.fd }
                };
                if fd < 0 {
                    if i == 0 {
                        tracing::info!(
                            camera = camera_name,
                            "VIDIOC_EXPBUF not supported — using MMAP path"
                        );
                    }
                    all_imported = false;
                    break;
                }
                match full.import_dma_buf_storage_buffer(fd, input_alloc_size) {
                    Ok(buf) => {
                        dmabuf_fds[i] = fd;
                        imported[i] = Some(buf);
                    }
                    Err(e) => {
                        if i == 0 {
                            if vulkan_device_name.to_lowercase().contains("nvidia") {
                                tracing::info!(
                                    "CameraSource {}: DMA-BUF import failed on NVIDIA GPU \
                                     (cross-device DMA-BUF limitation). Falling back to \
                                     MMAP + memcpy. This is expected and performant with \
                                     GPU compute.",
                                    camera_name
                                );
                            } else {
                                tracing::warn!(
                                    "CameraSource {}: DMA-BUF import failed (unexpected on {}): \
                                     {}. Falling back to MMAP + memcpy.",
                                    camera_name,
                                    vulkan_device_name,
                                    e
                                );
                            }
                        }
                        unsafe { libc::close(fd) };
                        all_imported = false;
                        break;
                    }
                }
            }
            if all_imported {
                dmabuf_imported_buffers = imported.into_iter().map(|o| o.unwrap()).collect();
                use_dmabuf = true;
            } else {
                for fd in &mut dmabuf_fds {
                    if *fd >= 0 {
                        unsafe { libc::close(*fd) };
                        *fd = -1;
                    }
                }
            }
        }

        Ok(CameraGpuResources {
            color_converter,
            recorder,
            timeline,
            ring_produce_done,
            ring_consume_done,
            input_storage_buffers,
            input_mapped_ptrs,
            ring_textures,
            ring_texture_ids,
            use_dmabuf,
            dmabuf_imported_buffers,
            dmabuf_fds,
            vulkan_device_name,
            probe_skipped,
        })
    });

    let CameraGpuResources {
        color_converter,
        mut recorder,
        timeline: camera_timeline,
        ring_produce_done,
        ring_consume_done,
        input_storage_buffers,
        input_mapped_ptrs,
        ring_textures,
        ring_texture_ids,
        use_dmabuf,
        dmabuf_imported_buffers,
        mut dmabuf_fds,
        vulkan_device_name,
        probe_skipped,
    } = match setup_result {
        Ok(resources) => resources,
        Err(e) => {
            tracing::error!(camera = camera_name, error = %e, "failed to set up GPU resources");
            return;
        }
    };

    if probe_skipped {
        tracing::info!(
            camera = camera_name,
            device = %vulkan_device_name,
            "DMA-BUF probe skipped — driver blocklisted for cross-device imports (#638). \
             Using MMAP + memcpy."
        );
    }
    if use_dmabuf {
        tracing::info!(
            camera = camera_name,
            buffers_imported = V4L2_BUFFER_COUNT,
            "DMA-BUF zero-copy enabled",
        );
    }

    // Each ring slot carries a per-slot single-writer-per-edge exportable
    // timeline pair; the post-compute barrier transitions the ring to
    // `SHADER_READ_ONLY_OPTIMAL` before publish, so the registered layout
    // matches contents by the time any consumer dereferences `surface_id`.
    for (i, (texture_id, stream_texture)) in ring_texture_ids
        .iter()
        .zip(ring_textures.iter())
        .enumerate()
    {
        if let Some(store) = gpu_context.surface_store()
            && let Err(e) = store.register_texture(
                texture_id,
                stream_texture,
                Some(&ring_produce_done[i]),
                Some(&ring_consume_done[i]),
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            )
        {
            tracing::warn!(
                camera = camera_name,
                ring_index = i,
                error = %e,
                "failed to register ring texture with the surface-share service — \
                 cross-process GPU sharing unavailable, same-process still works",
            );
        }
        gpu_context.register_texture_with_layout(
            texture_id,
            stream_texture.clone(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );
    }

    let dispatch_x = width.div_ceil(16);
    let dispatch_y = height.div_ceil(16);

    // DMA-BUF path drives DQBUF/QBUF per frame directly, so it QBUFs the
    // initial set + STREAMONs manually (the mmap stream does this internally
    // on first `next()`, which the MMAP path relies on).
    if use_dmabuf {
        unsafe {
            for i in 0..V4L2_BUFFER_COUNT {
                let mut v4l2_buf: v4l::v4l_sys::v4l2_buffer = std::mem::zeroed();
                v4l2_buf.type_ = v4l::buffer::Type::VideoCapture as u32;
                v4l2_buf.memory = v4l::memory::Memory::Mmap as u32;
                v4l2_buf.index = i;
                libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                    &mut v4l2_buf,
                );
            }
            let mut buf_type: u32 = v4l::buffer::Type::VideoCapture as u32;
            libc::ioctl(
                device_fd,
                v4l::v4l2::vidioc::VIDIOC_STREAMON as libc::c_ulong,
                &mut buf_type,
            );
        }
    }

    let requeue = |buf: Option<v4l::v4l_sys::v4l2_buffer>| {
        if let Some(mut v4l2_buf) = buf {
            unsafe {
                libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                    &mut v4l2_buf,
                );
            }
        }
    };

    let mut ping_pong_index: usize = 0;

    while is_capturing.load(Ordering::Acquire) {
        // ---- Step 1: Acquire frame and select input SSBO ----
        let mut v4l2_requeue_buf: Option<v4l::v4l_sys::v4l2_buffer> = None;
        let frame_sequence: u32;
        let input_ssbo_index: usize;

        if use_dmabuf {
            unsafe {
                let mut pollfd = libc::pollfd {
                    fd: device_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let poll_result = libc::poll(&mut pollfd, 1, 1000);
                if poll_result == 0 {
                    continue;
                }
                if poll_result < 0 {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, "V4L2 poll error");
                    }
                    break;
                }

                let mut v4l2_buf: v4l::v4l_sys::v4l2_buffer = std::mem::zeroed();
                v4l2_buf.type_ = v4l::buffer::Type::VideoCapture as u32;
                v4l2_buf.memory = v4l::memory::Memory::Mmap as u32;

                if libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_DQBUF as libc::c_ulong,
                    &mut v4l2_buf,
                ) != 0
                {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, "DQBUF failed");
                    }
                    continue;
                }

                input_ssbo_index = v4l2_buf.index as usize;
                frame_sequence = v4l2_buf.sequence;
                v4l2_requeue_buf = Some(v4l2_buf);
            }
        } else {
            // MMAP path: stream.next() issues VIDIOC_QBUF + VIDIOC_STREAMON
            // on its first call, then blocks on VIDIOC_DQBUF with the poll
            // timeout applied in start(). Do NOT poll the fd before
            // stream.next() — strict-conformance drivers (v4l2loopback) only
            // signal POLLIN after STREAMON, so an earlier poll hangs.
            let (buf, meta) = match stream.next() {
                Ok(frame) => frame,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, error = %e, "V4L2 stream error");
                    }
                    break;
                }
            };
            if !is_capturing.load(Ordering::Acquire) {
                break;
            }
            frame_sequence = meta.sequence;
            input_ssbo_index = ping_pong_index;

            let copy_len = buf.len().min(input_byte_size);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buf.as_ptr(),
                    input_mapped_ptrs[input_ssbo_index],
                    copy_len,
                );
            }
        }

        // Wait for the previous use of this ring texture slot to complete.
        // Frame N uses slot N % RING_TEXTURE_COUNT; the previous use was
        // frame N - RING_TEXTURE_COUNT which signaled timeline value
        // (N - RING_TEXTURE_COUNT + 1). The first RING_TEXTURE_COUNT frames
        // skip (initial timeline value 0).
        let frame_num_peek = frame_counter.load(Ordering::Relaxed);
        if frame_num_peek >= RING_TEXTURE_COUNT as u64 {
            let wait_value = frame_num_peek - (RING_TEXTURE_COUNT as u64 - 1);
            if let Err(e) = camera_timeline.wait(wait_value, u64::MAX) {
                tracing::warn!(camera = camera_name, error = %e, "timeline wait failed");
            }
        }

        let frame_num = frame_counter.fetch_add(1, Ordering::Relaxed);

        // ---- Step 2: Select ring texture + acquire pixel buffer for IPC ----
        let ring_index = (frame_num as usize) % RING_TEXTURE_COUNT;

        let (pool_id, pooled_buffer) = match gpu_context.acquire_pixel_buffer(
            width,
            height,
            PixelFormat::Rgba32,
        ) {
            Ok(result) => result,
            Err(e) => {
                if frame_num == 0 {
                    tracing::error!(camera = camera_name, error = %e, "failed to acquire pixel buffer");
                }
                requeue(v4l2_requeue_buf);
                continue;
            }
        };

        // Register the ring texture under the pixel buffer's pool_id so a
        // same-process display resolves the texture via the same surface_id
        // used for pixel-buffer IPC.
        gpu_context.register_texture_with_layout(
            &pool_id.to_string(),
            ring_textures[ring_index].clone(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        // ---- Step 3: Bind kernel via the color converter ----
        let input_buffer = if use_dmabuf {
            &dmabuf_imported_buffers[input_ssbo_index]
        } else {
            &input_storage_buffers[input_ssbo_index]
        };
        let kernel = match color_converter.prepare_buffer_to_image_storage(
            input_buffer,
            src_layout,
            &ring_textures[ring_index],
            &resolved_color,
            // Display path consumes RGBA8_UNORM treated as sRGB-encoded by
            // the swapchain; #817 will replace this hardcode with the
            // negotiated VkColorSpaceKHR.
            TransferId::Srgb,
        ) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(camera = camera_name, error = %e, "color_converter prepare failed");
                requeue(v4l2_requeue_buf);
                continue;
            }
        };

        // ---- Step 4: Record + submit ----
        if let Err(e) = recorder.begin() {
            tracing::error!(camera = camera_name, error = %e, "recorder.begin failed");
            requeue(v4l2_requeue_buf);
            continue;
        }

        // pre-compute: ring texture UNDEFINED → GENERAL.
        if let Err(e) = recorder.record_image_barrier(
            &ring_textures[ring_index],
            VulkanLayout::UNDEFINED,
            VulkanLayout::GENERAL,
            VulkanStage::NONE,
            VulkanStage::COMPUTE_SHADER,
            VulkanAccess::NONE,
            VulkanAccess::SHADER_WRITE,
        ) {
            tracing::error!(camera = camera_name, error = %e, "pre-compute image barrier failed");
            continue;
        }

        // pre-compute: imported DMA-BUF SSBO needs an explicit
        // read-availability barrier (the V4L2 driver wrote it before we got
        // the fd). HOST_VISIBLE SSBOs don't — coherent host writes need no
        // GPU-side sync beyond the implicit submit-time barrier.
        if use_dmabuf
            && let Err(e) = recorder.record_buffer_barrier(
                &dmabuf_imported_buffers[input_ssbo_index],
                VulkanStage::NONE,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::NONE,
                VulkanAccess::SHADER_READ,
            )
        {
            tracing::error!(camera = camera_name, error = %e, "pre-compute buffer barrier failed");
            continue;
        }

        if let Err(e) = recorder.record_dispatch(&kernel, dispatch_x, dispatch_y, 1) {
            tracing::error!(camera = camera_name, error = %e, "record_dispatch failed");
            continue;
        }

        // post-compute: ring texture GENERAL → TRANSFER_SRC for the host
        // pixel-buffer copy.
        if let Err(e) = recorder.record_image_barrier(
            &ring_textures[ring_index],
            VulkanLayout::GENERAL,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::TRANSFER_READ,
        ) {
            tracing::error!(camera = camera_name, error = %e, "post-compute image barrier failed");
            continue;
        }

        // Copy ring → pooled pixel buffer (cross-process IPC + CPU readback).
        let copy_region = ImageCopyRegion::tightly_packed(width, height);
        if let Err(e) = recorder.record_copy_image_to_buffer(
            &ring_textures[ring_index],
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            &pooled_buffer,
            copy_region,
        ) {
            tracing::error!(camera = camera_name, error = %e, "record_copy_image_to_buffer failed");
            continue;
        }

        // post-copy: ring texture TRANSFER_SRC → SHADER_READ_ONLY (consumed
        // by display); pixel buffer TRANSFER_WRITE → HOST_READ.
        if let Err(e) = recorder.record_image_barrier(
            &ring_textures[ring_index],
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::FRAGMENT_SHADER,
            VulkanAccess::TRANSFER_READ,
            VulkanAccess::SHADER_READ,
        ) {
            tracing::error!(camera = camera_name, error = %e, "post-copy image barrier failed");
            continue;
        }
        if let Err(e) = recorder.record_buffer_barrier(
            &pooled_buffer,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::HOST,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::HOST_READ,
        ) {
            tracing::error!(camera = camera_name, error = %e, "pixel-buffer host-read barrier failed");
            continue;
        }

        // Submit + signal timeline value (= frame_num + 1 so consumers can
        // wait on a monotonically advancing counter), then wait so the pixel
        // buffer is host-readable before the IPC write below.
        let timeline_signal_value = frame_num + 1;
        if let Err(e) = recorder.submit_signaling_timeline(&camera_timeline, timeline_signal_value)
        {
            if frame_num == 0 {
                tracing::error!(camera = camera_name, error = %e, "failed to submit compute dispatch");
            }
            requeue(v4l2_requeue_buf);
            continue;
        }
        if let Err(e) = camera_timeline.wait(timeline_signal_value, u64::MAX) {
            tracing::warn!(camera = camera_name, error = %e, "host-readback timeline wait failed");
        }

        // ---- Step 5: Re-queue V4L2 buffer in DMA-BUF mode ----
        requeue(v4l2_requeue_buf);

        // ---- Step 6: Publish frame ----
        // The pixel-buffer pool_id is the surface_id — the universal key:
        // same-process texture cache, cross-process surface-share, and CPU
        // readback all resolve through it.
        let frame = VideoFrame {
            surface_id: pool_id.to_string(),
            width,
            height,
            timestamp_ns: MediaClock::now().as_nanos() as i64,
            fps: capture_fps,
            color_info: Some(cached_color_info.clone()),
            // V4L2 doesn't surface ST.2086 / CLLI; HDR-aware sources only.
            mastering_display: None,
            content_light: None,
            texture_layout: None,
        };
        if let Err(e) = outputs.write("video", &frame) {
            tracing::error!(camera = camera_name, error = %e, "failed to write frame");
            continue;
        }

        // The pooled pixel buffer must stay alive until the consumer reads
        // it; the pool reclaims the slot when this handle drops after the
        // write has been delivered into the link's ring.
        drop(pooled_buffer);

        if frame_num == 0 {
            let mode = if use_dmabuf {
                "DMA-BUF zero-copy"
            } else {
                "MMAP + memcpy"
            };
            tracing::info!(
                camera = camera_name,
                mode,
                seq = frame_sequence,
                width,
                height,
                ?fourcc,
                "first frame captured via GPU compute",
            );
        } else if frame_num % 300 == 0 {
            tracing::debug!(camera = camera_name, frame = frame_num, "frame milestone");
        }

        if !use_dmabuf {
            ping_pong_index = 1 - ping_pong_index;
        }
    }

    // STREAMOFF in DMA-BUF mode (the mmap stream's Drop handles MMAP mode).
    if use_dmabuf {
        unsafe {
            let mut buf_type: u32 = v4l::buffer::Type::VideoCapture as u32;
            libc::ioctl(
                device_fd,
                v4l::v4l2::vidioc::VIDIOC_STREAMOFF as libc::c_ulong,
                &mut buf_type,
            );
        }
    }

    // The VIDIOC_EXPBUF fds were dup'd into Vulkan imports; the V4L2-side
    // fds are ours to close.
    for fd in &mut dmabuf_fds {
        if *fd >= 0 {
            unsafe { libc::close(*fd) };
            *fd = -1;
        }
    }

    drop(dmabuf_imported_buffers);
    drop(ring_textures);
    drop(ring_produce_done);
    drop(ring_consume_done);
    drop(input_storage_buffers);
    drop(recorder);
    drop(color_converter);
    drop(camera_timeline);
}

// Per-axis maps from the bag's H.273 vocabulary to the engine's color IDs.
// The engine accepts only its own primitive types in public signatures, so
// each consumer translates at the boundary.

fn primaries_id(p: &Primaries) -> PrimariesId {
    match p {
        Primaries::Bt709 => PrimariesId::Bt709,
        Primaries::Bt470M => PrimariesId::Bt470M,
        Primaries::Bt470Bg => PrimariesId::Bt470Bg,
        Primaries::Smpte170m => PrimariesId::Smpte170m,
        Primaries::Smpte240m => PrimariesId::Smpte240m,
        Primaries::Film => PrimariesId::Film,
        Primaries::Bt2020 => PrimariesId::Bt2020,
        Primaries::Smpte428 => PrimariesId::Smpte428,
        Primaries::Smpte431 => PrimariesId::Smpte431,
        Primaries::Smpte432 => PrimariesId::Smpte432,
        Primaries::Ebu3213 => PrimariesId::Ebu3213,
    }
}

fn transfer_id(t: &Transfer) -> TransferId {
    match t {
        Transfer::Srgb => TransferId::Srgb,
        Transfer::Bt709
        | Transfer::Smpte170m
        | Transfer::Bt2020TenBit
        | Transfer::Bt2020TwelveBit => TransferId::Bt709,
        Transfer::Smpte2084 => TransferId::Pq,
        Transfer::AribStdB67 => TransferId::Hlg,
        Transfer::Linear => TransferId::Linear,
        // Gamma22 / Gamma28 / Smpte240m / Log* / Xvycc / Bt1361 / Smpte428
        // are uncommon end-to-end; map to Linear (no transform).
        _ => TransferId::Linear,
    }
}

fn matrix_id(m: &Matrix) -> MatrixId {
    match m {
        Matrix::Identity => MatrixId::Identity,
        Matrix::Bt709 => MatrixId::Bt709,
        Matrix::Fcc => MatrixId::Fcc,
        Matrix::Bt470Bg => MatrixId::Bt470Bg,
        Matrix::Smpte170m => MatrixId::Smpte170m,
        Matrix::Smpte240m => MatrixId::Smpte240m,
        Matrix::Ycgco => MatrixId::Ycgco,
        Matrix::Bt2020Ncl => MatrixId::Bt2020Ncl,
        Matrix::Bt2020Cl => MatrixId::Bt2020Cl,
        Matrix::Smpte2085 => MatrixId::Smpte2085,
        Matrix::ChromaNcl => MatrixId::ChromaNcl,
        Matrix::ChromaCl => MatrixId::ChromaCl,
        Matrix::Ictcp => MatrixId::Ictcp,
    }
}

fn range_id(r: &Range) -> RangeId {
    match r {
        Range::Limited => RangeId::Limited,
        Range::Full => RangeId::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_devices_succeeds_with_or_without_cameras() {
        let devices = list_camera_capture_devices().expect("enumeration must not error");
        for device in &devices {
            assert!(device.id.starts_with("/dev/video"), "{}", device.id);
        }
    }

    #[test]
    fn config_defaults_to_no_device_and_uncapped() {
        let config: CameraSourceConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!(config.device_id, None);
        assert_eq!((config.max_width, config.max_height), (None, None));
    }

    /// Every bag-vocabulary color axis maps to an engine ID without panicking.
    #[test]
    fn color_axis_maps_are_total() {
        use crate::video_frame::{Matrix, Primaries, Range, Transfer};
        let _ = primaries_id(&Primaries::Bt709);
        let _ = transfer_id(&Transfer::Xvycc);
        let _ = matrix_id(&Matrix::Ictcp);
        let _ = range_id(&Range::Full);
    }
}
