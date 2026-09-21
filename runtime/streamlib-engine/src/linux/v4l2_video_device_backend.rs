// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The V4L2 arm of the video device seam: enumeration, open, NV12/YUYV
//! negotiation, and the capture loop that lands every frame in a pooled
//! `Rgba32` pixel buffer before handing it off.
//!
//! Camera→GPU transport is zero-copy DMA-BUF import when the device exports
//! it, transparent CPU-upload (MMAP + memcpy) fallback otherwise, selected
//! automatically — no configuration dial.

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use v4l::FourCC;
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;

use crate::core::color::{ColorSpaceKind, H273ColorVui, RangeId};
use crate::core::context::captured_video_frame_to_pooled_rgba_conversion_stage::{
    CapturedVideoFrameBytesInAStorageBuffer, CapturedVideoFrameDeliveryTally,
    CapturedVideoFrameToPooledRgbaConversionStage,
};
use crate::core::context::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, DeviceReportedCaptureStamp,
    DeviceStreamFailureReason, DeviceStreamFailureRecorder, DeviceStreamLivenessReport,
    GpuContextLimitedAccess, VideoCaptureDevice, VideoCaptureInstantResolver, VideoCaptureStream,
    VideoCaptureStreamFormat, VideoDeviceBackend, VideoDeviceStreamRequest,
    refusal_for_a_named_camera_that_is_not_attached,
};
use crate::core::media_clock::MediaClock;
use crate::core::rhi::{PixelFormat, SourceLayoutInfo, StorageBuffer};
use crate::core::{Error, Result};
use crate::linux::v4l2_color::v4l2_color_to_h273_color_vui;

/// Number of V4L2 mmap buffers to request.
const V4L2_BUFFER_COUNT: u32 = 4;

/// The V4L2 backend. V4L2 is a kernel interface with nothing to load, so this
/// arm always opens; a machine with no camera enumerates no devices.
pub(crate) struct V4l2VideoDeviceBackend;

impl VideoDeviceBackend for V4l2VideoDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "v4l2"
    }

    fn list_capture_devices(&self) -> Result<Vec<VideoCaptureDevice>> {
        list_v4l2_capture_devices()
    }

    fn open_capture_stream(
        &self,
        request: &VideoDeviceStreamRequest,
    ) -> Result<Box<dyn VideoCaptureStream>> {
        Ok(Box::new(V4l2VideoCaptureStream::open(request)?))
    }
}

/// Every `/dev/video*` node that reports video capture, in node-index order.
fn list_v4l2_capture_devices() -> Result<Vec<VideoCaptureDevice>> {
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
        devices.push(VideoCaptureDevice {
            id: path.to_string_lossy().to_string(),
            name: caps.card,
        });
    }
    // `read_dir` order is unspecified; sort by the numeric node index so the
    // "first camera found" default is stable across runs.
    devices.sort_by_key(|device| {
        device
            .id
            .trim_start_matches("/dev/video")
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    Ok(devices)
}

/// Open the device `device_id` names, or the first capture device found.
///
/// A named device that cannot be opened is refused naming it — with the
/// attached cameras listed when it does not exist — and never replaced by a
/// different one.
fn open_requested_v4l2_device(device_id: Option<&str>) -> Result<(String, v4l::Device)> {
    let device_path = match device_id {
        Some(id) => id.to_string(),
        None => {
            let devices = list_v4l2_capture_devices()?;
            devices.first().map(|d| d.id.clone()).ok_or_else(|| {
                Error::Configuration(
                    "No camera found: nothing under /dev/video* reports video capture. \
                     Check the camera is plugged in (`ls /dev/video*`), or use \
                     TestPatternSource to run without one."
                        .into(),
                )
            })?
        }
    };

    let device = v4l::Device::with_path(&device_path).map_err(|e| {
        Error::Configuration(match e.kind() {
            std::io::ErrorKind::PermissionDenied => format!(
                "Camera '{}' exists but you don't have permission to open it. Add \
                 yourself to the `video` group — `sudo usermod -aG video $USER` — \
                 then log out and back in.",
                device_path
            ),
            std::io::ErrorKind::NotFound => refusal_for_a_named_camera_that_is_not_attached(
                &device_path,
                &list_v4l2_capture_devices().unwrap_or_default(),
                "Check the camera is plugged in (`ls /dev/video*`)",
            ),
            _ => format!("Failed to open V4L2 device '{}': {}", device_path, e),
        })
    })?;

    Ok((device_path, device))
}

/// A capture stream on one V4L2 device: opened and negotiated at open, and
/// streaming from its own capture thread between `start_delivering_to` and
/// `stop_delivering`.
struct V4l2VideoCaptureStream {
    device: v4l::Device,
    opened_device: VideoCaptureDevice,
    stream_format: VideoCaptureStreamFormat,
    capture_fourcc: FourCC,
    gpu_context: GpuContextLimitedAccess,
    failure_recorder: DeviceStreamFailureRecorder,
    liveness_report: DeviceStreamLivenessReport,
    capture_instant_resolver: Arc<VideoCaptureInstantResolver>,
    capture_thread: Option<V4l2CaptureThread>,
}

/// The thread one delivery runs on, with the flag that asks it to stop.
///
/// Minted per delivery, so a stopped thread is never revived by a later start,
/// and held until the thread is joined: one still running past its stop's
/// grace window keeps the stream from starting a second delivery beside it.
struct V4l2CaptureThread {
    is_capturing: Arc<AtomicBool>,
    join_handle: JoinHandle<()>,
}

impl V4l2VideoCaptureStream {
    fn open(request: &VideoDeviceStreamRequest) -> Result<Self> {
        let (device_path, mut dev) = open_requested_v4l2_device(request.device_id.as_deref())?;

        let caps = dev.query_caps().map_err(|e| {
            Error::Configuration(format!("Failed to query device capabilities: {}", e))
        })?;
        let camera_name = caps.card.clone();
        tracing::info!(
            "V4L2 camera: opened '{}' (driver: {}, bus: {})",
            caps.card,
            caps.driver,
            caps.bus
        );

        let current_fmt = dev
            .format()
            .map_err(|e| Error::Configuration(format!("Failed to read current format: {}", e)))?;

        // Negotiate format + resolution: enumerate frame sizes for NV12
        // (preferred) or YUYV and pick the highest resolution that fits the
        // requested cap. VIDIOC_S_FMT snaps to the nearest supported size —
        // which can be LARGER than a naive capped request — so the cap
        // constrains the enumeration, and a driver that still snaps above it
        // gets a warning.
        let max_width = request.max_width;
        let max_height = request.max_height;
        let fmt =
            negotiate_capture_format(&mut dev, current_fmt, &camera_name, max_width, max_height)?;
        if fmt.width > max_width || fmt.height > max_height {
            tracing::warn!(
                "V4L2 camera {}: driver snapped to {}x{}, above the configured cap {}x{}",
                camera_name,
                fmt.width,
                fmt.height,
                max_width,
                max_height
            );
        }

        tracing::info!(
            "V4L2 camera {}: capturing {}x{} {:?}",
            camera_name,
            fmt.width,
            fmt.height,
            fmt.fourcc
        );

        let capture_fps: Option<u32> = match dev.params() {
            Ok(params) if params.interval.numerator > 0 => {
                Some(params.interval.denominator / params.interval.numerator)
            }
            _ => None,
        };

        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
        let capture_instant_resolver =
            Arc::new(VideoCaptureInstantResolver::for_device(camera_name.clone()));
        Ok(Self {
            device: dev,
            opened_device: VideoCaptureDevice {
                id: device_path,
                name: camera_name,
            },
            stream_format: VideoCaptureStreamFormat {
                width: fmt.width,
                height: fmt.height,
                frames_per_second: capture_fps,
            },
            capture_fourcc: fmt.fourcc,
            gpu_context: request.gpu_context.clone(),
            failure_recorder,
            liveness_report,
            capture_instant_resolver,
            capture_thread: None,
        })
    }
}

impl VideoCaptureStream for V4l2VideoCaptureStream {
    fn stream_format(&self) -> VideoCaptureStreamFormat {
        self.stream_format
    }

    fn opened_device(&self) -> &VideoCaptureDevice {
        &self.opened_device
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn future_capture_stamps_clamped_to_dequeue(&self) -> u64 {
        self.capture_instant_resolver
            .future_capture_stamps_clamped_to_dequeue()
    }

    fn start_delivering_to(&mut self, hand_off: CapturedVideoFrameHandOff) -> Result<()> {
        self.stop_delivering()?;

        let mut stream = v4l::io::mmap::Stream::with_buffers(
            &self.device,
            Type::VideoCapture,
            V4L2_BUFFER_COUNT,
        )
        .map_err(|e| Error::Configuration(format!("Failed to create V4L2 mmap stream: {}", e)))?;

        // Poll timeout so the capture thread can check is_capturing.
        stream.set_timeout(std::time::Duration::from_secs(1));

        let is_capturing = Arc::new(AtomicBool::new(true));
        let is_capturing_in_the_thread = Arc::clone(&is_capturing);
        let gpu_context = self.gpu_context.clone();
        let camera_name = self.opened_device.name.clone();
        let VideoCaptureStreamFormat { width, height, .. } = self.stream_format;
        let capture_fourcc = self.capture_fourcc;
        let failure_recorder = self.failure_recorder.clone();
        let capture_instant_resolver = Arc::clone(&self.capture_instant_resolver);

        let join_handle = std::thread::Builder::new()
            .name(format!("v4l2-capture-{}", self.opened_device.id))
            .spawn(move || {
                capture_thread_loop(
                    stream,
                    is_capturing_in_the_thread,
                    hand_off,
                    gpu_context,
                    camera_name,
                    width,
                    height,
                    capture_fourcc,
                    failure_recorder,
                    capture_instant_resolver,
                );
            })
            .map_err(|e| Error::Configuration(format!("Failed to spawn capture thread: {}", e)))?;

        self.capture_thread = Some(V4l2CaptureThread {
            is_capturing,
            join_handle,
        });

        tracing::info!(
            "V4L2 camera {}: capture started ({}x{} {:?}, {} mmap buffers)",
            self.opened_device.name,
            width,
            height,
            capture_fourcc,
            V4L2_BUFFER_COUNT
        );
        Ok(())
    }

    fn stop_delivering(&mut self) -> Result<()> {
        let Some(capture_thread) = self.capture_thread.as_ref() else {
            return Ok(());
        };
        capture_thread.is_capturing.store(false, Ordering::Release);

        // Bounded wait: the capture thread can be inside a long timeline wait
        // or a V4L2 dequeue when stop arrives; both exit promptly under normal
        // conditions but a stalled GPU / driver state can stretch them out.
        // Giving up after a 2 s grace window keeps the runtime's shutdown
        // chain moving. The thread stays held until it is joined, so a later
        // start waits on it again rather than opening a delivery beside it; one
        // still running at process exit is reaped there.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !capture_thread.join_handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !capture_thread.join_handle.is_finished() {
            return Err(Error::Runtime(format!(
                "V4L2 camera {}: capture thread did not exit within 2s; it still holds the \
                 device's buffers, so this stream will not deliver again until it exits",
                self.opened_device.name
            )));
        }
        if let Some(finished_capture_thread) = self.capture_thread.take() {
            let _ = finished_capture_thread.join_handle.join();
        }
        Ok(())
    }
}

impl Drop for V4l2VideoCaptureStream {
    fn drop(&mut self) {
        if let Err(stop_error) = self.stop_delivering() {
            tracing::warn!(
                error = %stop_error,
                "V4L2 capture stream dropped while delivering"
            );
        }
    }
}

/// Pick NV12 (preferred) or YUYV at the highest enumerated resolution that
/// fits within `max_width` x `max_height`.
fn negotiate_capture_format(
    dev: &mut v4l::Device,
    current_fmt: v4l::format::Format,
    camera_name: &str,
    max_width: u32,
    max_height: u32,
) -> Result<v4l::format::Format> {
    let nv12_fourcc = FourCC::new(b"NV12");
    let yuyv_fourcc = FourCC::new(b"YUYV");

    let highest_resolution = |framesizes: &[v4l::framesize::FrameSize]| -> Option<(u32, u32)> {
        let mut best_pixels = 0u64;
        let mut best = None;
        for fs in framesizes {
            let (w, h) = match &fs.size {
                v4l::framesize::FrameSizeEnum::Discrete(d) => (d.width, d.height),
                // Stepwise ranges include every size up to the max; clamp the
                // candidate into the cap instead of discarding the range.
                v4l::framesize::FrameSizeEnum::Stepwise(s) => {
                    (s.max_width.min(max_width), s.max_height.min(max_height))
                }
            };
            if w > max_width || h > max_height {
                continue;
            }
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
        let mut try_fmt = current_fmt;
        try_fmt.fourcc = nv12_fourcc;
        try_fmt.width = best_w;
        try_fmt.height = best_h;
        if let Ok(f) = dev.set_format(&try_fmt)
            && f.fourcc == nv12_fourcc
        {
            tracing::info!(
                "V4L2 camera {}: NV12 available, highest resolution {}x{}",
                camera_name,
                f.width,
                f.height
            );
            return Ok(f);
        }
    }

    // Fall back to YUYV.
    tracing::info!(
        "V4L2 camera {}: NV12 not available, trying YUYV",
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
    conversion_stage: CapturedVideoFrameToPooledRgbaConversionStage,
    input_storage_buffers: Vec<StorageBuffer>,
    input_mapped_ptrs: [*mut u8; 2],
    use_dmabuf: bool,
    dmabuf_imported_buffers: Vec<StorageBuffer>,
    vulkan_device_name: String,
    probe_skipped: bool,
}

/// The two V4L2 capture formats the GPU converter has shaders for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureFormat {
    Nv12,
    Yuyv,
}

impl CaptureFormat {
    fn from_fourcc(fourcc: FourCC) -> Option<Self> {
        match &fourcc.repr {
            b"NV12" => Some(Self::Nv12),
            b"YUYV" => Some(Self::Yuyv),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_thread_loop(
    mut stream: v4l::io::mmap::Stream,
    is_capturing: Arc<AtomicBool>,
    hand_off: CapturedVideoFrameHandOff,
    gpu_context: GpuContextLimitedAccess,
    camera_name: String,
    width: u32,
    height: u32,
    fourcc: FourCC,
    failure_recorder: DeviceStreamFailureRecorder,
    capture_instant_resolver: Arc<VideoCaptureInstantResolver>,
) {
    let record_that_capture_ended_on_its_own = |reason: String| {
        failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(reason));
    };

    let Some(capture_format) = CaptureFormat::from_fourcc(fourcc) else {
        tracing::error!(
            camera = camera_name,
            ?fourcc,
            "unsupported format — no GPU compute shader available",
        );
        record_that_capture_ended_on_its_own(format!(
            "the device negotiated {fourcc:?}, which no GPU conversion kernel reads"
        ));
        return;
    };

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

    // Query V4L2 format once at start: (1) the colorspace 4-tuple, as the
    // H.273 description every frame carries, (2) `bytesperline` for the
    // source SSBO stride (vivid + some UVC drivers report stride > width even
    // for NV12), (3) `sizeimage` for the SSBO allocation (must hold the full
    // V4L2 frame including padding). V4L2 contract: all three stay constant
    // during streaming.
    let (cached_color, v4l2_bytes_per_line, v4l2_size_image): (H273ColorVui, u32, u32) = unsafe {
        let mut v4l2_fmt: v4l::v4l_sys::v4l2_format = std::mem::zeroed();
        v4l2_fmt.type_ = v4l::buffer::Type::VideoCapture as u32;
        if libc::ioctl(
            device_fd,
            v4l::v4l2::vidioc::VIDIOC_G_FMT as libc::c_ulong,
            &mut v4l2_fmt,
        ) == 0
        {
            let pix = v4l2_fmt.fmt.pix;
            let color = v4l2_color_to_h273_color_vui(
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
            let (tight_bytes_per_line, tight_size_image) = match capture_format {
                CaptureFormat::Nv12 => (width, width * height * 3 / 2),
                CaptureFormat::Yuyv => (width * 2, width * height * 2),
            };
            (
                H273ColorVui::default(),
                tight_bytes_per_line,
                tight_size_image,
            )
        }
    };

    // SSBO must hold the full V4L2 frame including driver-side row padding
    // (vivid reports 3840-byte stride for 1920-wide NV12). Truncating to
    // tight-pack size memcpys only half the Y plane and reads garbage UV.
    let input_byte_size = v4l2_size_image as usize;
    let input_alloc_size = input_byte_size.next_multiple_of(4) as u64;

    // Source-buffer layout for the converter's push constants. NV12 uses
    // `bytesperline` for both planes (V4L2 bi-planar convention); YUYV is a
    // single packed plane.
    let src_layout = match capture_format {
        CaptureFormat::Nv12 => SourceLayoutInfo::nv12(
            v4l2_bytes_per_line,
            v4l2_bytes_per_line,
            v4l2_bytes_per_line * height,
        ),
        CaptureFormat::Yuyv => SourceLayoutInfo::yuyv(v4l2_bytes_per_line),
    };
    tracing::info!(
        camera = camera_name,
        bytes_per_line = v4l2_bytes_per_line,
        size_image = v4l2_size_image,
        width,
        height,
        "V4L2 buffer layout"
    );

    // Resolve the device's colour to the fully-resolved description the
    // color converter's push constants use. Held for the life of the capture
    // thread — V4L2 colorspace doesn't change mid-stream.
    let resolved_color = cached_color.resolve_defaults(ColorSpaceKind::Yuv);

    // Map (fourcc, resolved range) to the canonical PixelFormat used as the
    // converter cache key. The push-constant matrix bakes the range
    // expansion in.
    let src_pixel_format = match (capture_format, &resolved_color.range) {
        (CaptureFormat::Nv12, RangeId::Full) => PixelFormat::Nv12FullRange,
        (CaptureFormat::Nv12, _) => PixelFormat::Nv12VideoRange,
        (CaptureFormat::Yuyv, _) => PixelFormat::Yuyv422,
    };

    let setup_result = gpu_context.escalate(|full| {
        let caps = full.gpu_capabilities()?;
        let vulkan_device_name = caps.device_name.clone();

        let conversion_stage = CapturedVideoFrameToPooledRgbaConversionStage::create(
            full,
            src_pixel_format,
            width,
            height,
        )?;

        // Double-buffered HOST_VISIBLE input SSBOs (MMAP+memcpy fallback path).
        let mut input_storage_buffers: Vec<StorageBuffer> = Vec::with_capacity(2);
        let mut input_mapped_ptrs: [*mut u8; 2] = [std::ptr::null_mut(); 2];
        for slot in &mut input_mapped_ptrs {
            let buf = full.acquire_storage_buffer(input_alloc_size)?;
            *slot = buf.mapped_ptr();
            input_storage_buffers.push(buf);
        }

        // DMA-BUF probe — VIDIOC_EXPBUF on each V4L2 buffer + Vulkan import.
        // The import side is privileged (allocates VkDeviceMemory + binds) so
        // it stays inside the escalation; failure falls through to MMAP.
        let probe_skipped = !caps.supports_cross_device_dma_buf_probe;
        let mut use_dmabuf = false;
        let mut dmabuf_imported_buffers: Vec<StorageBuffer> = Vec::new();
        if caps.supports_external_memory && !is_virtual_device && !probe_skipped {
            let mut imported: Vec<StorageBuffer> = Vec::with_capacity(V4L2_BUFFER_COUNT as usize);
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
                    break;
                }
                // SAFETY: VIDIOC_EXPBUF minted this fd for us and nothing
                // else holds it; the import owns it from here.
                let dma_buf_fd = unsafe { OwnedFd::from_raw_fd(fd) };
                match full.import_dma_buf_storage_buffer(dma_buf_fd, input_alloc_size) {
                    Ok(imported_buffer) => imported.push(imported_buffer),
                    Err(e) => {
                        if i == 0 {
                            if vulkan_device_name.to_lowercase().contains("nvidia") {
                                tracing::info!(
                                    "V4L2 camera {}: DMA-BUF import failed on NVIDIA GPU \
                                     (cross-device DMA-BUF limitation). Falling back to \
                                     MMAP + memcpy. This is expected and performant with \
                                     GPU compute.",
                                    camera_name
                                );
                            } else {
                                tracing::warn!(
                                    "V4L2 camera {}: DMA-BUF import failed (unexpected on {}): \
                                     {}. Falling back to MMAP + memcpy.",
                                    camera_name,
                                    vulkan_device_name,
                                    e
                                );
                            }
                        }
                        break;
                    }
                }
            }
            if imported.len() == V4L2_BUFFER_COUNT as usize {
                dmabuf_imported_buffers = imported;
                use_dmabuf = true;
            }
        }

        Ok(CameraGpuResources {
            conversion_stage,
            input_storage_buffers,
            input_mapped_ptrs,
            use_dmabuf,
            dmabuf_imported_buffers,
            vulkan_device_name,
            probe_skipped,
        })
    });

    let CameraGpuResources {
        mut conversion_stage,
        input_storage_buffers,
        input_mapped_ptrs,
        use_dmabuf,
        dmabuf_imported_buffers,
        vulkan_device_name,
        probe_skipped,
    } = match setup_result {
        Ok(resources) => resources,
        Err(e) => {
            tracing::error!(camera = camera_name, error = %e, "failed to set up GPU resources");
            record_that_capture_ended_on_its_own(format!(
                "the GPU resources the capture converts on could not be created: {e}"
            ));
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
                if libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                    &mut v4l2_buf,
                ) != 0
                {
                    tracing::error!(
                        camera = camera_name,
                        buffer_index = i,
                        errno = std::io::Error::last_os_error().raw_os_error(),
                        "initial VIDIOC_QBUF failed"
                    );
                }
            }
            let mut buf_type: u32 = v4l::buffer::Type::VideoCapture as u32;
            if libc::ioctl(
                device_fd,
                v4l::v4l2::vidioc::VIDIOC_STREAMON as libc::c_ulong,
                &mut buf_type,
            ) != 0
            {
                let streamon_error = std::io::Error::last_os_error();
                tracing::error!(
                    camera = camera_name,
                    errno = streamon_error.raw_os_error(),
                    "VIDIOC_STREAMON failed — camera produces no frames; stopping capture thread"
                );
                record_that_capture_ended_on_its_own(format!(
                    "VIDIOC_STREAMON failed: {streamon_error}"
                ));
                return;
            }
        }
    }

    let requeue = |buf: Option<v4l::v4l_sys::v4l2_buffer>| {
        if let Some(mut v4l2_buf) = buf {
            let result = unsafe {
                libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                    &mut v4l2_buf,
                )
            };
            if result != 0 {
                // Each failed requeue permanently removes one buffer from the
                // driver queue; after V4L2_BUFFER_COUNT of them the DMA-BUF
                // path starves silently.
                tracing::error!(
                    buffer_index = v4l2_buf.index,
                    errno = std::io::Error::last_os_error().raw_os_error(),
                    "VIDIOC_QBUF requeue failed — one capture buffer lost"
                );
            }
        }
    };

    let mut ping_pong_index: usize = 0;
    let mut delivery_tally = CapturedVideoFrameDeliveryTally::default();

    while is_capturing.load(Ordering::Acquire) {
        // ---- Step 1: Acquire frame and select input SSBO ----
        let mut v4l2_requeue_buf: Option<v4l::v4l_sys::v4l2_buffer> = None;
        let frame_sequence: u32;
        let input_ssbo_index: usize;
        let capture_timestamp_ns: i64;

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
                        let poll_error = std::io::Error::last_os_error();
                        tracing::error!(camera = camera_name, "V4L2 poll error");
                        record_that_capture_ended_on_its_own(format!(
                            "polling the device failed: {poll_error}"
                        ));
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

                capture_timestamp_ns = capture_instant_resolver.resolve_capture_timestamp_ns(
                    v4l2_buffer_capture_stamp(
                        v4l2_buf.flags,
                        v4l2_buf.timestamp.tv_sec,
                        v4l2_buf.timestamp.tv_usec,
                    ),
                    MediaClock::now().as_nanos() as i64,
                );
                input_ssbo_index = v4l2_buf.index as usize;
                frame_sequence = v4l2_buf.sequence;
                v4l2_requeue_buf = Some(v4l2_buf);
            }
        } else {
            // MMAP path: stream.next() issues VIDIOC_QBUF + VIDIOC_STREAMON
            // on its first call, then blocks on VIDIOC_DQBUF with the poll
            // timeout applied in start_delivering_to(). Do NOT poll the fd before
            // stream.next() — strict-conformance drivers (v4l2loopback) only
            // signal POLLIN after STREAMON, so an earlier poll hangs.
            let (buf, meta) = match stream.next() {
                Ok(frame) => frame,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    if is_capturing.load(Ordering::Acquire) {
                        tracing::error!(camera = camera_name, error = %e, "V4L2 stream error");
                        record_that_capture_ended_on_its_own(format!(
                            "the device's stream failed: {e}"
                        ));
                    }
                    break;
                }
            };
            if !is_capturing.load(Ordering::Acquire) {
                break;
            }
            capture_timestamp_ns = capture_instant_resolver.resolve_capture_timestamp_ns(
                v4l2_buffer_capture_stamp(
                    meta.flags.bits(),
                    meta.timestamp.sec,
                    meta.timestamp.usec,
                ),
                MediaClock::now().as_nanos() as i64,
            );
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

        let input_buffer = if use_dmabuf {
            &dmabuf_imported_buffers[input_ssbo_index]
        } else {
            &input_storage_buffers[input_ssbo_index]
        };
        let frame_result = conversion_stage.convert_into_pooled_pixel_buffer(
            &gpu_context,
            CapturedVideoFrameBytesInAStorageBuffer {
                storage_buffer: input_buffer,
                layout: src_layout,
                written_by_another_device: use_dmabuf,
            },
            &resolved_color,
        );

        // The V4L2 buffer goes back to the driver on success and failure
        // alike — a skipped requeue starves the DMA-BUF queue after
        // V4L2_BUFFER_COUNT drops.
        requeue(v4l2_requeue_buf);

        let (published_pixel_buffer_frame_id, pooled_buffer) = match frame_result {
            Ok(frame_surfaces) => frame_surfaces,
            Err(frame_error) => {
                delivery_tally.record_a_dropped_frame(&camera_name, &frame_error);
                continue;
            }
        };

        // A stop that arrived during this frame's GPU work ends delivery here,
        // so a stop that returned `Ok` is never followed by a hand-off.
        if !is_capturing.load(Ordering::Acquire) {
            break;
        }
        hand_off(CapturedVideoFrameFromDevice {
            published_pixel_buffer_frame_id: &published_pixel_buffer_frame_id,
            width,
            height,
            color: cached_color,
            capture_timestamp_ns,
        });

        // The pooled pixel buffer must stay alive until the hand-off has
        // written the frame out; the pool reclaims the slot when this handle
        // drops after the write has been delivered into the link's ring.
        drop(pooled_buffer);

        let delivered_frames = delivery_tally.record_a_delivered_frame();
        if delivered_frames == 1 {
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
        } else if delivered_frames.is_multiple_of(300) {
            tracing::debug!(
                camera = camera_name,
                frame = delivered_frames,
                "frame milestone"
            );
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

    // The imported fds are driver-owned (`vkImportMemoryFdInfoKHR` took
    // ownership); dropping the buffers frees them through Vulkan.
    drop(dmabuf_imported_buffers);
    drop(conversion_stage);
    drop(input_storage_buffers);
}

/// What a dequeued V4L2 buffer says about when its frame was captured.
///
/// The buffer's timestamp flags name the clock its stamp was taken on; only
/// `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC` is the machine's monotonic clock. The
/// others — `UNKNOWN`, and `COPY` from an output queue — say nothing the
/// engine can join to its own clock.
fn v4l2_buffer_capture_stamp(
    buffer_flags: u32,
    timestamp_seconds: i64,
    timestamp_microseconds: i64,
) -> DeviceReportedCaptureStamp {
    let timestamp_clock = buffer_flags & v4l::buffer::Flags::TIMESTAMP_MASK.bits();
    if timestamp_clock != v4l::buffer::Flags::TIMESTAMP_MONOTONIC.bits() {
        return DeviceReportedCaptureStamp::OffTheMachineMonotonicClock;
    }
    DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
        capture_timestamp_ns: timestamp_seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(timestamp_microseconds.saturating_mul(1_000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_flagged_monotonic_carries_its_stamp_in_nanoseconds() {
        let flags = v4l::buffer::Flags::TIMESTAMP_MONOTONIC.bits()
            | v4l::buffer::Flags::DONE.bits()
            | v4l::buffer::Flags::MAPPED.bits();
        assert_eq!(
            v4l2_buffer_capture_stamp(flags, 541_560, 123_456),
            DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
                capture_timestamp_ns: 541_560_123_456_000
            }
        );
    }

    #[test]
    fn a_buffer_whose_stamp_clock_is_unknown_or_copied_is_off_the_monotonic_clock() {
        for timestamp_clock in [
            v4l::buffer::Flags::TIMESTAMP_UNKNOWN,
            v4l::buffer::Flags::TIMESTAMP_COPY,
        ] {
            assert_eq!(
                v4l2_buffer_capture_stamp(timestamp_clock.bits(), 541_560, 123_456),
                DeviceReportedCaptureStamp::OffTheMachineMonotonicClock,
                "{timestamp_clock:?}"
            );
        }
    }

    #[test]
    fn list_devices_succeeds_with_or_without_cameras() {
        let devices = list_v4l2_capture_devices().expect("enumeration must not error");
        for device in &devices {
            assert!(device.id.starts_with("/dev/video"), "{}", device.id);
        }
    }

    /// A camera asked for by a name that does not exist is refused naming
    /// it, never replaced by whatever camera happens to be attached.
    #[test]
    fn a_named_device_that_does_not_exist_is_refused_naming_it() {
        let refusal = match open_requested_v4l2_device(Some("/dev/video-streamlib-absent")) {
            Ok(_) => panic!("a device node that does not exist cannot open"),
            Err(refusal) => refusal.to_string(),
        };
        assert!(
            refusal.contains("'/dev/video-streamlib-absent' does not exist"),
            "{refusal}"
        );
    }
}
