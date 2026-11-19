// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The V4L2 arm of the video device seam: enumeration, open, NV12/YUYV
//! negotiation, and the capture loop that lands every frame in a pooled
//! `Rgba32` pixel buffer before handing it off.
//!
//! Camera→GPU transport is zero-copy DMA-BUF import when the device exports
//! it, transparent CPU-upload (MMAP + memcpy) fallback otherwise, selected
//! automatically — no configuration dial.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use v4l::FourCC;
use v4l::buffer::Type;
use v4l::video::Capture;

use crate::core::context::{
    CapturedVideoFrameHandOff, DeviceStreamFailureRecorder, DeviceStreamLivenessReport,
    GpuContextLimitedAccess, VideoCaptureDevice, VideoCaptureInstantResolver, VideoCaptureStream,
    VideoCaptureStreamFormat, VideoDeviceBackend, VideoDeviceStreamRequest,
    refusal_for_a_named_camera_that_is_not_attached,
};
use crate::core::{Error, Result};
use v4l2_capture_thread::capture_thread_loop;

#[cfg(test)]
mod tests;
mod v4l2_capture_buffer_queue;
mod v4l2_capture_dequeue_loop;
mod v4l2_capture_gpu_resources;
mod v4l2_capture_thread;
mod v4l2_driver_buffers_mapped_for_reading;

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
    node_run_id_of_the_capturing_node: String,
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
            node_run_id_of_the_capturing_node: request.node_run_id_of_the_capturing_node.clone(),
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

        let stream = v4l::io::mmap::Stream::with_buffers(
            &self.device,
            Type::VideoCapture,
            V4L2_BUFFER_COUNT,
        )
        .map_err(|e| Error::Configuration(format!("Failed to create V4L2 mmap stream: {}", e)))?;

        let is_capturing = Arc::new(AtomicBool::new(true));
        let is_capturing_in_the_thread = Arc::clone(&is_capturing);
        let gpu_context = self.gpu_context.clone();
        let node_run_id_of_the_capturing_node = self.node_run_id_of_the_capturing_node.clone();
        let camera_name = self.opened_device.name.clone();
        let VideoCaptureStreamFormat { width, height, .. } = self.stream_format;
        let capture_fourcc = self.capture_fourcc;
        let failure_recorder = self.failure_recorder.clone();
        let capture_instant_resolver = Arc::clone(&self.capture_instant_resolver);

        let join_handle = std::thread::Builder::new()
            .name(format!("v4l2-capture-{}", self.opened_device.id))
            .spawn(
                crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                    capture_thread_loop(
                        stream,
                        is_capturing_in_the_thread,
                        hand_off,
                        gpu_context,
                        node_run_id_of_the_capturing_node,
                        camera_name,
                        width,
                        height,
                        capture_fourcc,
                        failure_recorder,
                        capture_instant_resolver,
                    );
                }),
            )
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
