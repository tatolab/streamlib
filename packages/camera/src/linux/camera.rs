// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! V4L2 camera capture processor.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use streamlib_plugin_sdk::sdk::color::{
    resolve_color_defaults, ColorSpaceKind, MatrixId, PrimariesId, RangeId, TransferId,
};
use streamlib_plugin_sdk::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::iceoryx2::OutputWriter;
use streamlib_plugin_sdk::sdk::rhi::{
    HostTimelineSemaphore, ImageCopyRegion, PixelFormat, RhiColorConverter, RhiCommandRecorder,
    SourceLayoutInfo, StorageBuffer, Texture, TextureFormat, VulkanAccess, VulkanLayout,
    VulkanStage,
};

use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::FourCC;

/// Number of ring textures for GPU-resident pipeline (matches MAX_FRAMES_IN_FLIGHT).
const RING_TEXTURE_COUNT: usize = 2;

/// Number of V4L2 mmap buffers to request.
const V4L2_BUFFER_COUNT: u32 = 4;

#[derive(Debug, Clone)]
pub struct LinuxCameraDevice {
    pub id: String,
    pub name: String,
}

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/camera/Camera",
    execution = manual,
    scheduling = high,
    config = crate::_generated_::CameraConfig,
    output("video", "@tatolab/core/VideoFrame"),
)]
pub struct LinuxCameraProcessor {
    camera_name: String,
    gpu_context: Option<GpuContextLimitedAccess>,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    capture_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl streamlib_plugin_sdk::sdk::processors::ManualProcessor for LinuxCameraProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        tracing::info!("Camera: setup() complete");
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let frame_count = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!(
            "Camera {}: Teardown (generated {} frames)",
            self.camera_name,
            frame_count
        );
        self.is_capturing.store(false, Ordering::Release);
        if let Some(handle) = self.capture_thread_handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let gpu_context = self.gpu_context.clone().ok_or_else(|| {
            Error::Configuration("GPU context not initialized. Call setup() first.".into())
        })?;

        // The capture thread holds only `GpuContextLimitedAccess` and
        // upgrades to FullAccess exactly once, at thread start, via
        // `gpu_context.escalate(|full| ...)` for the one-shot privileged
        // resource construction (compute kernel, command recorder, timeline,
        // ring textures, DMA-BUF imports). Per-frame work runs on
        // LimitedAccess.
        let _ = ctx; // FullAccess ctx is kept in setup() and not extracted here.

        let device_path = match &self.config.device_id {
            Some(id) => id.clone(),
            None => {
                let devices = Self::list_devices()?;
                devices.first().map(|d| d.id.clone()).ok_or_else(|| {
                    Error::Configuration(
                        "No V4L2 capture devices found. Check that a camera is connected.".into(),
                    )
                })?
            }
        };

        // Open the V4L2 device
        let mut dev = v4l::Device::with_path(&device_path).map_err(|e| {
            Error::Configuration(format!("Failed to open V4L2 device '{}': {}", device_path, e))
        })?;

        // Query device capabilities
        let caps = dev.query_caps().map_err(|e| {
            Error::Configuration(format!("Failed to query device capabilities: {}", e))
        })?;
        self.camera_name = caps.card.clone();
        tracing::info!(
            "Camera: opened '{}' (driver: {}, bus: {})",
            caps.card,
            caps.driver,
            caps.bus
        );

        // Read the device's current format as a fallback baseline
        let current_fmt = dev.format().map_err(|e| {
            Error::Configuration(format!("Failed to read current format: {}", e))
        })?;

        // Negotiate format + resolution: enumerate frame sizes for NV12 (preferred) or
        // YUYV, pick the highest resolution, then set_format with those parameters.
        let fmt = {
            let nv12_fourcc = FourCC::new(b"NV12");
            let yuyv_fourcc = FourCC::new(b"YUYV");

            let mut negotiated: Option<v4l::format::Format> = None;

            // Try NV12 first — enumerate available frame sizes and pick largest
            if let Ok(framesizes) = dev.enum_framesizes(nv12_fourcc) {
                let mut best_pixels = 0u64;
                let mut best_w = current_fmt.width;
                let mut best_h = current_fmt.height;
                for fs in &framesizes {
                    match &fs.size {
                        v4l::framesize::FrameSizeEnum::Discrete(d) => {
                            let pixels = d.width as u64 * d.height as u64;
                            if pixels > best_pixels {
                                best_pixels = pixels;
                                best_w = d.width;
                                best_h = d.height;
                            }
                        }
                        v4l::framesize::FrameSizeEnum::Stepwise(s) => {
                            let pixels = s.max_width as u64 * s.max_height as u64;
                            if pixels > best_pixels {
                                best_pixels = pixels;
                                best_w = s.max_width;
                                best_h = s.max_height;
                            }
                        }
                    }
                }
                if best_pixels > 0 {
                    let mut try_fmt = current_fmt.clone();
                    try_fmt.fourcc = nv12_fourcc;
                    try_fmt.width = best_w;
                    try_fmt.height = best_h;
                    if let Ok(f) = dev.set_format(&try_fmt) {
                        if f.fourcc == nv12_fourcc {
                            tracing::info!(
                                "Camera {}: NV12 available, highest resolution {}x{}",
                                self.camera_name,
                                f.width,
                                f.height
                            );
                            negotiated = Some(f);
                        }
                    }
                }
            }

            // If NV12 didn't work, try YUYV with highest available resolution
            if negotiated.is_none() {
                tracing::info!("Camera {}: NV12 not available, trying YUYV", self.camera_name);

                let (best_w, best_h) = if let Ok(framesizes) = dev.enum_framesizes(yuyv_fourcc) {
                    let mut best_pixels = 0u64;
                    let mut w = current_fmt.width;
                    let mut h = current_fmt.height;
                    for fs in &framesizes {
                        match &fs.size {
                            v4l::framesize::FrameSizeEnum::Discrete(d) => {
                                let pixels = d.width as u64 * d.height as u64;
                                if pixels > best_pixels {
                                    best_pixels = pixels;
                                    w = d.width;
                                    h = d.height;
                                }
                            }
                            v4l::framesize::FrameSizeEnum::Stepwise(s) => {
                                let pixels = s.max_width as u64 * s.max_height as u64;
                                if pixels > best_pixels {
                                    best_pixels = pixels;
                                    w = s.max_width;
                                    h = s.max_height;
                                }
                            }
                        }
                    }
                    (w, h)
                } else {
                    (current_fmt.width, current_fmt.height)
                };

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
                negotiated = Some(f);
            }

            negotiated.unwrap()
        };

        // Cap capture resolution at config.max_width / max_height (defaults
        // 1920x1080 preserve the real-time-encoding guardrail; drone-racing
        // and similar high-resolution use cases opt in by raising the cap).
        let max_width = self.config.max_width.unwrap_or(1920);
        let max_height = self.config.max_height.unwrap_or(1080);
        let fmt = if fmt.width > max_width || fmt.height > max_height {
            let mut capped = fmt.clone();
            capped.width = max_width;
            capped.height = max_height;
            match dev.set_format(&capped) {
                Ok(f) => {
                    tracing::info!(
                        "Camera {}: capped resolution from {}x{} to {}x{}",
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
                        "Camera {}: failed to cap resolution to {}x{} ({}), using {}x{}",
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
            "Camera {}: capturing {}x{} {:?}",
            self.camera_name,
            capture_width,
            capture_height,
            capture_fourcc
        );

        // Create mmap stream with V4L2_BUFFER_COUNT buffers
        let mut stream =
            v4l::io::mmap::Stream::with_buffers(&mut dev, Type::VideoCapture, V4L2_BUFFER_COUNT)
                .map_err(|e| {
                    Error::Configuration(format!("Failed to create V4L2 mmap stream: {}", e))
                })?;

        // Set a poll timeout so the capture thread can check is_capturing periodically.
        stream.set_timeout(std::time::Duration::from_secs(1));

        // Query V4L2 capture parameters for frame rate.
        let capture_fps: Option<u32> = match dev.params() {
            Ok(params) if params.interval.numerator > 0 => {
                let fps = params.interval.denominator / params.interval.numerator;
                tracing::info!(
                    "Camera {}: V4L2 frame interval {}/{} → {}fps",
                    self.camera_name,
                    params.interval.numerator,
                    params.interval.denominator,
                    fps
                );
                Some(fps)
            }
            Ok(_) => {
                tracing::warn!(
                    "Camera {}: V4L2 frame interval numerator is 0, fps unknown",
                    self.camera_name
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    "Camera {}: failed to query V4L2 capture params: {}, fps unknown",
                    self.camera_name,
                    e
                );
                None
            }
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
            .map_err(|e| {
                Error::Configuration(format!("Failed to spawn capture thread: {}", e))
            })?;

        self.capture_thread_handle = Some(handle);

        tracing::info!(
            "Camera {}: V4L2 capture started ({}x{} {:?}, {} mmap buffers)",
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

        // Bounded wait — same shape as `LinuxDisplayProcessor::stop`. The
        // capture thread can be inside a long timeline wait or a V4L2 dequeue
        // when stop arrives; both exit promptly under normal conditions but a
        // stalled GPU / driver state can stretch them out indefinitely.
        // Detaching after a 2 s grace window keeps the runtime's shutdown
        // chain moving so downstream processors (display, etc.) can also tear
        // down — without this, a stuck camera thread freezes the window with
        // the last rendered frame on screen. The detached thread is reaped
        // when the parent process exits.
        if let Some(handle) = self.capture_thread_handle.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                tracing::warn!(
                    "Camera {}: capture thread did not exit within 2s, detaching",
                    self.camera_name
                );
            }
        }

        tracing::info!(
            "Camera {}: Stopped ({} frames)",
            self.camera_name,
            self.frame_counter.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

struct CameraGpuResources {
    color_converter: RhiColorConverter,
    recorder: RhiCommandRecorder,
    timeline: HostTimelineSemaphore,
    // Per-ring-slot single-writer-per-edge exportable timeline pairs
    // — `produce_done` signaled by the camera capture path,
    // `consume_done` signaled by cross-process consumers. See
    // `docs/architecture/adapter-timeline-single-writer.md`.
    ring_produce_done: Vec<HostTimelineSemaphore>,
    ring_consume_done: Vec<HostTimelineSemaphore>,
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

    // Verify the FourCC is one of the YUV formats we support before
    // querying V4L2 details.
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
    // coherency). Skip DMA-BUF probe for those — MMAP + memcpy is correct.
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

    // Query V4L2 format once at processor start. We need three things
    // from this: (1) the colorspace 4-tuple for `ColorInfo`,
    // (2) `bytesperline` for the source SSBO stride (vivid + some UVC
    // drivers report stride > width even for NV12), (3) `sizeimage`
    // for the SSBO allocation (must hold the full V4L2 frame including
    // padding). V4L2 contract is that all three stay constant during
    // streaming.
    let (cached_color_info, v4l2_bytes_per_line, v4l2_size_image): (
        crate::_generated_::ColorInfo,
        u32,
        u32,
    ) = unsafe {
        let mut v4l2_fmt: v4l::v4l_sys::v4l2_format = std::mem::zeroed();
        v4l2_fmt.type_ = v4l::buffer::Type::VideoCapture as u32;
        if libc::ioctl(
            device_fd,
            v4l::v4l2::vidioc::VIDIOC_G_FMT as libc::c_ulong,
            &mut v4l2_fmt,
        ) == 0
        {
            let pix = v4l2_fmt.fmt.pix;
            let color = crate::linux::v4l2_color::v4l2_color_to_color_info(
                pix.colorspace,
                pix.xfer_func,
                // ycbcr_enc shares an anonymous union with hsv_enc;
                // use the YCbCr field since this code path is YUV-only
                // (NV12 / YUYV — guarded by the FourCC match above).
                //
                // `__bindgen_anon_1` is the bindgen-generated name for
                // the inner `union { ycbcr_enc; hsv_enc }`. Stable on
                // v4l2-sys-mit 0.3.x as long as the C struct keeps a
                // single anonymous union at this position; an upstream
                // bump that adds a second anonymous union to the parent
                // struct would shift the suffix to `_2` and this access
                // would stop compiling — caught at build time, not
                // runtime.
                pix.__bindgen_anon_1.ycbcr_enc,
                pix.quantization,
            );
            (color, pix.bytesperline, pix.sizeimage)
        } else {
            // ioctl failed — emit "all unknown" colors and fall back
            // to tight-packed buffer sizing. `ColorInfo::default()` is
            // structurally `{ primaries: None, transfer: None, matrix:
            // None, range: None }` per the schema's `optionalProperties`
            // shape.
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
            (
                crate::_generated_::ColorInfo::default(),
                tight_bytes_per_line,
                tight_size_image,
            )
        }
    };

    // SSBO must hold the full V4L2 frame including driver-side row
    // padding (vivid reports 3840-byte stride for 1920-wide NV12).
    // Truncating to tight-pack size (the pre-#815 behavior) memcpys
    // only the first half of the Y plane and reads garbage for UV,
    // producing the all-green "green vivid" symptom.
    let input_byte_size = v4l2_size_image as usize;
    let input_alloc_size = ((input_byte_size + 3) / 4 * 4) as u64;

    // Source-buffer layout passed to the converter's push constants
    // every frame. NV12 uses `bytesperline` for both Y and UV plane
    // strides (V4L2 convention; both planes share stride for bi-planar
    // formats). YUYV has a single packed plane.
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
    {
        // Render unspecified axes as the literal string "unspecified"
        // rather than `None` so the structured log reads cleanly for
        // operators (and matches the H.273 wire-level term).
        fn axis<T: std::fmt::Debug>(v: &Option<T>) -> String {
            v.as_ref().map(|v| format!("{:?}", v)).unwrap_or_else(|| "unspecified".to_string())
        }
        tracing::info!(
            camera = camera_name,
            primaries = %axis(&cached_color_info.primaries),
            transfer = %axis(&cached_color_info.transfer),
            matrix = %axis(&cached_color_info.matrix),
            range = %axis(&cached_color_info.range),
            "V4L2 colorspace detected",
        );
    }

    // Resolve V4L2 ColorInfo to a fully-resolved description used by
    // the color converter's per-frame push constants. Held for the
    // life of the capture thread — V4L2 colorspace doesn't change
    // mid-stream. Translate this package's `_generated_::ColorInfo`
    // axes into engine IDs locally; engine APIs take engine-owned
    // primitive types so each consumer maps its own generated
    // schema flavor here.
    let resolved_color = resolve_color_defaults(
        cached_color_info.primaries.as_ref().map(primaries_id),
        cached_color_info.transfer.as_ref().map(transfer_id),
        cached_color_info.matrix.as_ref().map(matrix_id),
        cached_color_info.range.as_ref().map(range_id),
        ColorSpaceKind::Yuv,
    );

    // Map (fourcc, resolved range) to the canonical PixelFormat used
    // as the converter cache key. The push-constant matrix bakes the
    // range expansion in, so NV12 full vs limited end up in two
    // converter instances sharing the same SPIR-V — a few KB of
    // duplicated load, no correctness impact.
    let src_pixel_format = match (&fourcc_bytes, &resolved_color.range) {
        (b"NV12", RangeId::Full) => PixelFormat::Nv12FullRange,
        (b"NV12", _) => PixelFormat::Nv12VideoRange,
        (b"YUYV", _) => PixelFormat::Yuyv422,
        _ => unreachable!("input_byte_size match above rejects other fourccs"),
    };

    let setup_result = gpu_context.escalate(|full| {
        // Read-once capability snapshot — the FullAccess device handle must
        // not cross the plugin ABI into this cdylib-loaded processor.
        let caps = full.gpu_capabilities()?;
        let vulkan_device_name = caps.device_name.clone();

        let color_converter = full.color_converter(src_pixel_format, PixelFormat::Rgba32)?;

        let recorder = full.create_command_recorder("camera_capture")?;

        // Host-readback / display-wait timeline. Exportable is the only
        // engine-free timeline primitive; the camera only ever waits on it
        // host-side, so the extra OPAQUE_FD capability is harmless.
        let timeline = full.create_exportable_timeline_semaphore(0)?;

        // Double-buffered HOST_VISIBLE input SSBOs (MMAP+memcpy fallback path).
        let mut input_storage_buffers: Vec<StorageBuffer> = Vec::with_capacity(2);
        let mut input_mapped_ptrs: [*mut u8; 2] = [std::ptr::null_mut(); 2];
        for i in 0..2 {
            let buf = full.acquire_storage_buffer(input_alloc_size)?;
            input_mapped_ptrs[i] = buf.mapped_ptr();
            input_storage_buffers.push(buf);
        }

        // 2-texture DEVICE_LOCAL ring via the FullAccess render-target
        // DMA-BUF allocation slot. Picks an EGL-probe
        // tiled DRM modifier; the resulting Texture carries
        // STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC | COPY_DST |
        // RENDER_ATTACHMENT — a superset of the camera's
        // STORAGE_BINDING|TEXTURE_BINDING|COPY_SRC needs. The extra
        // RENDER_ATTACHMENT|COPY_DST bits are additive and harmless
        // (the camera writes via storage-image, never as a render
        // target). The engine's host RHI texture constructor rides the
        // pre-warmed VMA pool from `HostVulkanDevice::new()`, so the
        // NVIDIA post-swapchain export cap doesn't bite (see
        // `docs/learnings/nvidia-dma-buf-after-swapchain.md`).
        let mut ring_textures: Vec<Texture> = Vec::with_capacity(RING_TEXTURE_COUNT);
        let mut ring_texture_ids: Vec<String> = Vec::with_capacity(RING_TEXTURE_COUNT);
        let mut ring_produce_done: Vec<HostTimelineSemaphore> =
            Vec::with_capacity(RING_TEXTURE_COUNT);
        let mut ring_consume_done: Vec<HostTimelineSemaphore> =
            Vec::with_capacity(RING_TEXTURE_COUNT);
        // Per-ring-slot exportable timelines for single-writer-per-edge
        // surface-share registration (see
        // `docs/architecture/adapter-timeline-single-writer.md`).
        for _ in 0..RING_TEXTURE_COUNT {
            let stream_texture = full.acquire_render_target_dma_buf_image(
                width,
                height,
                TextureFormat::Rgba8Unorm,
            )?;
            let texture_id = uuid::Uuid::new_v4().to_string();
            let produce_done = full.create_exportable_timeline_semaphore(0)?;
            let consume_done = full.create_exportable_timeline_semaphore(0)?;
            ring_texture_ids.push(texture_id);
            ring_textures.push(stream_texture);
            ring_produce_done.push(produce_done);
            ring_consume_done.push(consume_done);
        }

        // DMA-BUF probe — VIDIOC_EXPBUF on each V4L2 buffer + Vulkan
        // import. The import side is privileged (allocates VkDeviceMemory
        // + binds), so it stays inside the escalation. Failure falls
        // through to the HOST_VISIBLE MMAP path above.
        let supports_cross_device_dma_buf_probe =
            caps.supports_cross_device_dma_buf_probe;
        let probe_skipped = !supports_cross_device_dma_buf_probe;
        let mut use_dmabuf = false;
        let mut dmabuf_fds: [i32; V4L2_BUFFER_COUNT as usize] =
            [-1; V4L2_BUFFER_COUNT as usize];
        let mut dmabuf_imported_buffers: Vec<StorageBuffer> = Vec::new();
        if caps.supports_external_memory
            && !is_virtual_device
            && supports_cross_device_dma_buf_probe
        {
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
                    if r != 0 {
                        -1
                    } else {
                        expbuf.fd
                    }
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
                            if vulkan_device_name.contains("NVIDIA")
                                || vulkan_device_name.contains("nvidia")
                            {
                                tracing::info!(
                                    "Camera {}: DMA-BUF import failed on NVIDIA GPU \
                                     (cross-device DMA-BUF limitation). Falling back to \
                                     MMAP + memcpy. This is expected and performant with \
                                     GPU compute.",
                                    camera_name
                                );
                            } else {
                                tracing::warn!(
                                    "Camera {}: DMA-BUF import failed (unexpected on {}): \
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
                dmabuf_imported_buffers =
                    imported.into_iter().map(|o| o.unwrap()).collect();
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
    })
    // `escalate` wraps the closure's own `Result` — flatten the
    // `Result<Result<_>>` (the SDK does not auto-flatten a fallible closure).
    .and_then(std::convert::identity);

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
    } else if !is_virtual_device && !probe_skipped {
        tracing::debug!(
            camera = camera_name,
            "DMA-BUF probe declined — running on MMAP + memcpy path",
        );
    }
    tracing::info!(
        camera = camera_name,
        count = RING_TEXTURE_COUNT,
        width,
        height,
        "ring textures created (RGBA8 DEVICE_LOCAL DMA-BUF exportable, STORAGE | SAMPLED)",
    );

    // Each ring slot carries a per-slot single-writer-per-edge
    // exportable timeline pair (`produce_done` + `consume_done`); the
    // post-compute barrier transitions the ring to
    // `SHADER_READ_ONLY_OPTIMAL` before IPC publish, so the registered
    // layout matches contents by the time any consumer dereferences
    // `surface_id`. See
    // `docs/architecture/adapter-runtime-integration.md` →
    // Dual-registration for the Path-1 / Path-2 contract, and
    // `docs/architecture/adapter-timeline-single-writer.md` for the
    // timeline pair semantics.
    for (i, (texture_id, stream_texture)) in
        ring_texture_ids.iter().zip(ring_textures.iter()).enumerate()
    {
        let store = gpu_context.surface_store();
        if !store.is_none() {
            if let Err(e) = store.register_texture(
                texture_id,
                stream_texture,
                Some(&ring_produce_done[i]),
                Some(&ring_consume_done[i]),
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            ) {
                tracing::warn!(
                    camera = camera_name,
                    ring_index = i,
                    error = %e,
                    "failed to register ring texture with the surface-share service — cross-process GPU sharing unavailable, same-process still works",
                );
            }
        }
        gpu_context.register_texture_with_layout(
            texture_id,
            stream_texture.clone(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );
    }

    let mut timeline_signal_value: u64;

    let dispatch_x = (width + 15) / 16;
    let dispatch_y = (height + 15) / 16;

    // DMA-BUF path drives DQBUF/QBUF per frame directly, so it has to
    // QBUF the initial set + STREAMON manually (the v4l crate's mmap
    // stream does this internally on first `next()`, which the MMAP path
    // relies on).
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

    let mut ping_pong_index: usize = 0;

    while is_capturing.load(Ordering::Acquire) {
        // ---- Step 1: Acquire frame and select input SSBO ----
        // For the DMA-BUF path, the V4L2 buffer index picks the imported
        // SSBO directly. For the MMAP path, we toggle a ping-pong index.
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
            // on its first call, then blocks on VIDIOC_DQBUF. set_timeout()
            // (applied in start()) caps that wait so the thread can observe
            // is_capturing during shutdown. Do NOT poll the fd before
            // stream.next() — strict-conformance drivers (v4l2loopback) only
            // signal POLLIN after STREAMON, so an earlier poll hangs the
            // loop.
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

            // Upload raw V4L2 frame data to current input SSBO (the memcpy).
            let copy_len = buf.len().min(input_byte_size);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buf.as_ptr(),
                    input_mapped_ptrs[input_ssbo_index],
                    copy_len,
                );
            }
        }

        // Wait for previous use of this ring texture slot to complete.
        // Frame N uses ring slot N % RING_TEXTURE_COUNT; the previous use was
        // frame N - RING_TEXTURE_COUNT which signaled timeline value
        // (N - RING_TEXTURE_COUNT + 1). First RING_TEXTURE_COUNT frames skip
        // (initial timeline value 0).
        let frame_num_peek = frame_counter.load(Ordering::Relaxed);
        if frame_num_peek >= RING_TEXTURE_COUNT as u64 {
            let wait_value = frame_num_peek - (RING_TEXTURE_COUNT as u64 - 1);
            if let Err(e) = camera_timeline.wait(wait_value, u64::MAX) {
                tracing::warn!(camera = camera_name, error = %e, "timeline wait failed");
            }
        }

        let frame_num = frame_counter.fetch_add(1, Ordering::Relaxed);
        let _ = frame_sequence; // surfaced via log only on first frame; kept for parity

        // ---- Step 2: Select ring texture + acquire pixel buffer for IPC ----
        let ring_index = (frame_num as usize) % RING_TEXTURE_COUNT;

        let (pool_id, pooled_buffer) =
            match gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Rgba32) {
                Ok(result) => result,
                Err(e) => {
                    if frame_num == 0 {
                        tracing::error!(camera = camera_name, error = %e, "failed to acquire pixel buffer");
                    }
                    if let Some(mut v4l2_buf) = v4l2_requeue_buf {
                        unsafe {
                            libc::ioctl(
                                device_fd,
                                v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                                &mut v4l2_buf,
                            );
                        }
                    }
                    continue;
                }
            };

        // Register ring texture in cache under the pixel buffer's pool_id so
        // display resolves the texture via the same surface_id used for
        // pixel-buffer IPC. The same Arc<HostVulkanTexture> registered up
        // front with SHADER_READ_ONLY_OPTIMAL is published here under a
        // fresh pool_id — re-declare the layout so the registration record
        // under this pool_id matches the steady-state contract.
        gpu_context.register_texture_with_layout(
            &pool_id.to_string(),
            ring_textures[ring_index].clone(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        // ---- Step 3: Bind kernel via color converter ----
        // `prepare_buffer_to_image` stages source SSBO, destination
        // storage image, and the 96-byte `ColorConverterPushConstants`
        // derived from `resolved_color`. Returns the underlying
        // compute kernel for the recorder to dispatch. Range
        // expansion + matrix coefficients are computed CPU-side from
        // the resolved color; the shader applies them as a single
        // 3×3 multiply.
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
            // Display path consumes RGBA8_UNORM treated as sRGB-
            // encoded by the swapchain; #817 will replace this
            // hardcode with the negotiated VkColorSpaceKHR.
            TransferId::Srgb,
        ) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(camera = camera_name, error = %e, "color_converter prepare failed");
                if let Some(mut v4l2_buf) = v4l2_requeue_buf {
                    unsafe {
                        libc::ioctl(
                            device_fd,
                            v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                            &mut v4l2_buf,
                        );
                    }
                }
                continue;
            }
        };

        // ---- Step 4: Record + submit via RhiCommandRecorder ----
        if let Err(e) = recorder.begin() {
            tracing::error!(camera = camera_name, error = %e, "recorder.begin failed");
            if let Some(mut v4l2_buf) = v4l2_requeue_buf {
                unsafe {
                    libc::ioctl(
                        device_fd,
                        v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                        &mut v4l2_buf,
                    );
                }
            }
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

        // pre-compute: imported DMA-BUF SSBO needs an explicit read-availability
        // barrier (V4L2 driver wrote to it before we got the fd). HOST_VISIBLE
        // SSBOs don't — coherent host writes don't require GPU-side
        // synchronization beyond the implicit submit-time barrier.
        if use_dmabuf {
            if let Err(e) = recorder.record_buffer_barrier(
                &dmabuf_imported_buffers[input_ssbo_index],
                VulkanStage::NONE,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::NONE,
                VulkanAccess::SHADER_READ,
            ) {
                tracing::error!(camera = camera_name, error = %e, "pre-compute buffer barrier failed");
                continue;
            }
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

        // Copy ring → pooled pixel buffer (cross-process IPC).
        let copy_region = ImageCopyRegion::tightly_packed(width, height);
        if let Err(e) = recorder.record_copy_image_to_pixel_buffer(
            &ring_textures[ring_index],
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            &pooled_buffer,
            copy_region,
        ) {
            tracing::error!(camera = camera_name, error = %e, "record_copy_image_to_pixel_buffer failed");
            continue;
        }

        // post-copy: ring texture TRANSFER_SRC → SHADER_READ_ONLY (consumed
        // by display); pixel buffer TRANSFER_WRITE → HOST_READ (read by
        // IPC consumer).
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
        if let Err(e) = recorder.record_pixel_buffer_barrier(
            &pooled_buffer,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::HOST,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::HOST_READ,
        ) {
            tracing::error!(camera = camera_name, error = %e, "pixel-buffer host-read barrier failed");
            continue;
        }

        // Submit + signal timeline value (= frame_num + 1 so display can
        // wait on a monotonically advancing counter), then wait so the
        // pixel buffer is host-readable for the IPC write below.
        timeline_signal_value = frame_num + 1;
        if let Err(e) =
            recorder.submit_signaling_timeline(&camera_timeline, timeline_signal_value)
        {
            if frame_num == 0 {
                tracing::error!(camera = camera_name, error = %e, "failed to submit compute dispatch");
            }
            if let Some(mut v4l2_buf) = v4l2_requeue_buf {
                unsafe {
                    libc::ioctl(
                        device_fd,
                        v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                        &mut v4l2_buf,
                    );
                }
            }
            continue;
        }
        if let Err(e) = camera_timeline.wait(timeline_signal_value, u64::MAX) {
            tracing::warn!(camera = camera_name, error = %e, "host-readback timeline wait failed");
        }

        // ---- Step 5: Re-queue V4L2 buffer in DMA-BUF mode ----
        if let Some(mut v4l2_buf) = v4l2_requeue_buf {
            unsafe {
                libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                    &mut v4l2_buf,
                );
            }
        }

        // ---- Step 6: Publish frame via IPC ----
        // Use pixel-buffer pool_id as surface_id — the universal key:
        // - Same-process: texture cache resolves ring texture (registered above)
        // - Cross-process GPU: surface-share service has ring texture DMA-BUF fd (registered at startup)
        // - Cross-process CPU: surface-share service has pixel buffer DMA-BUF fd (registered by acquire)
        // - PNG sampling: resolves pixel buffer for CPU readback
        let surface_id = pool_id.to_string();
        let timestamp_ns =
            streamlib_plugin_sdk::sdk::media_clock::MediaClock::now().as_nanos() as i64;

        let ipc_frame = crate::_generated_::VideoFrame {
            surface_id,
            width,
            height,
            timestamp_ns: timestamp_ns.to_string(),
            fps: capture_fps,
            // Per-frame override is opt-in; per-surface
            // `current_image_layout` from surface-share is the default.
            texture_layout: None,
            color_info: Some(cached_color_info.clone()),
            // HDR static metadata: V4L2 doesn't surface ST.2086 / CLLI;
            // populated by HDR-aware sources only.
            mastering_display: None,
            content_light: None,
        };

        if let Err(e) = outputs.write("video", &ipc_frame) {
            tracing::error!(camera = camera_name, error = %e, "failed to write frame");
            continue;
        }

        if frame_num == 0 {
            let mode = if use_dmabuf { "DMA-BUF zero-copy" } else { "MMAP + memcpy" };
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

        // Toggle ping-pong index for next frame (MMAP path only).
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

    // The VIDIOC_EXPBUF fds were dup'd into Vulkan imports above; the
    // V4L2-side fds are ours to close.
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

impl LinuxCameraProcessor::Processor {
    /// Enumerate available V4L2 camera devices.
    pub fn list_devices() -> Result<Vec<LinuxCameraDevice>> {
        let mut devices = Vec::new();

        // Scan /dev/video* devices
        for entry in std::fs::read_dir("/dev").map_err(|e| {
            Error::Configuration(format!("Failed to read /dev: {}", e))
        })? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            if !name.starts_with("video") {
                continue;
            }

            let dev = match v4l::Device::with_path(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };

            let caps = match dev.query_caps() {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Only include devices with video capture capability
            if !caps
                .capabilities
                .contains(v4l::capability::Flags::VIDEO_CAPTURE)
            {
                continue;
            }

            devices.push(LinuxCameraDevice {
                id: path.to_string_lossy().to_string(),
                name: caps.card,
            });
        }

        Ok(devices)
    }
}

/// Per-axis maps from this package's `_generated_::ColorInfo` enums to the
/// engine's color IDs. The engine accepts only its own primitive types in
/// public method signatures, so each consumer translates its own generated
/// schema flavor at the boundary; the wire format is the contract across
/// packages, not Rust type equality.

fn primaries_id(p: &crate::_generated_::tatolab__core::color_info::Primaries) -> PrimariesId {
    use crate::_generated_::tatolab__core::color_info::Primaries;
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

fn transfer_id(t: &crate::_generated_::tatolab__core::color_info::Transfer) -> TransferId {
    use crate::_generated_::tatolab__core::color_info::Transfer;
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

fn matrix_id(m: &crate::_generated_::tatolab__core::color_info::Matrix) -> MatrixId {
    use crate::_generated_::tatolab__core::color_info::Matrix;
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

fn range_id(r: &crate::_generated_::tatolab__core::color_info::Range) -> RangeId {
    use crate::_generated_::tatolab__core::color_info::Range;
    match r {
        Range::Limited => RangeId::Limited,
        Range::Full => RangeId::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::_generated_::CameraConfig;
    use streamlib_plugin_sdk::sdk::processors::GeneratedProcessor;

    #[test]
    fn test_list_devices() {
        let devices = LinuxCameraProcessor::Processor::list_devices();
        assert!(devices.is_ok());

        if let Ok(devices) = devices {
            println!("Found {} V4L2 camera devices:", devices.len());
            for device in &devices {
                println!("  [{}] {}", device.id, device.name);
            }
        }
    }

    #[test]
    fn test_create_default_processor() {
        let config = CameraConfig {
            device_id: None,
            min_fps: None,
            max_fps: None,
            max_width: None,
            max_height: None,
        };

        let result = LinuxCameraProcessor::Processor::from_config(config);

        match result {
            Ok(_processor) => {
                println!("Successfully created camera processor from config");
            }
            Err(e) => {
                println!(
                    "Note: Could not create camera processor (may require permissions): {}",
                    e
                );
            }
        }
    }
}
