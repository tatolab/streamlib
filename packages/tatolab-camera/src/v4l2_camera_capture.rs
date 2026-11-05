// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! V4L2 capture with no CPU copy: each driver buffer is exported as a DMA-BUF,
//! adopted once through the module host table, and converted on the GPU into
//! the node's output texture from a thread this wheel owns.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignDmaBuf, ModuleHostLogLevel,
    ModuleHostNodeOutputTextureRequest, ModuleHostOutputPort, ModuleHostSurface,
};
use v4l::FourCC;
use v4l::buffer::Type;
use v4l::memory::Memory;
use v4l::v4l_sys::{v4l2_buffer, v4l2_exportbuffer, v4l2_requestbuffers};
use v4l::v4l2;
use v4l::video::Capture;

use crate::camera_frame_publication::{
    CAMERA_LOG_TARGET, CONVERTED_FRAME_POOL_KEY, CONVERTED_FRAME_TEXTURE_FORMAT,
    CONVERTED_FRAME_TEXTURE_USAGE, CameraCaptureRefusal, CaptureLoopReportingWindow,
    FRAMES_BETWEEN_REPORTS,
};
use crate::capture_timestamp::{monotonic_now_ns, resolve_capture_timestamp};
use crate::captured_frame_colour_description::{
    CapturedFrameColourDescription, matrix_name_or_the_players_fallback,
};
use crate::v4l2_colour_description::V4l2CaptureFormatColourFields;
use crate::video_frame_record::PublishedVideoFrame;

/// How many driver buffers rotate: one being filled, one being converted, and
/// slack for the driver.
const V4L2_DRIVER_BUFFER_COUNT: u32 = 4;

/// How long one wait for a filled buffer lasts before the capture thread looks
/// at its stop flags again.
const FILLED_BUFFER_WAIT_TIMEOUT_MS: i32 = 200;

/// The byte layout of one captured frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturedPixelLayout {
    /// A luma plane, then a half-height plane of interleaved Cb/Cr, in one buffer.
    Nv12,
    /// Packed 4:2:2, `Y0 Cb Y1 Cr` per two pixels.
    Yuyv,
}

impl CapturedPixelLayout {
    /// The V4L2 fourcc, as text.
    pub fn fourcc_name(self) -> &'static str {
        match self {
            Self::Nv12 => "NV12",
            Self::Yuyv => "YUYV",
        }
    }

    /// How many rows of `bytes_per_line` one frame of `height` occupies.
    pub fn rows_in_one_frame(self, height: u32) -> u32 {
        match self {
            Self::Nv12 => height + height.div_ceil(2),
            Self::Yuyv => height,
        }
    }
}

/// What the device negotiated, fixed for the life of the capture.
#[derive(Debug, Clone)]
pub struct NegotiatedCaptureFormat {
    pub device_path: String,
    pub card_name: String,
    pub pixel_layout: CapturedPixelLayout,
    pub width: u32,
    pub height: u32,
    pub bytes_per_line: u32,
    pub colour: CapturedFrameColourDescription,
    pub nominal_frames_per_second: Option<u32>,
}

impl NegotiatedCaptureFormat {
    /// The rows a frame is delivered in, each `bytes_per_line` long.
    pub fn rows_in_one_frame(&self) -> u32 {
        self.pixel_layout.rows_in_one_frame(self.height)
    }

    /// The engine's pixel format wire name for what the device delivers.
    pub fn pixel_format_wire_name(&self) -> &'static str {
        match (self.pixel_layout, self.colour.range) {
            (CapturedPixelLayout::Nv12, Some("full")) => "nv12_full_range",
            (CapturedPixelLayout::Nv12, _) => "nv12_video_range",
            (CapturedPixelLayout::Yuyv, _) => "yuyv422",
        }
    }

    /// Where each plane starts inside a driver buffer, and its row stride.
    pub fn plane_offsets_and_strides_bytes(&self) -> (Vec<u64>, Vec<u64>) {
        let bytes_per_line = u64::from(self.bytes_per_line);
        match self.pixel_layout {
            CapturedPixelLayout::Nv12 => (
                vec![0, bytes_per_line * u64::from(self.height)],
                vec![bytes_per_line, bytes_per_line],
            ),
            CapturedPixelLayout::Yuyv => (vec![0], vec![bytes_per_line]),
        }
    }

    /// The colour the converter decodes with: the driver's, its matrix
    /// falling back the way a player's does.
    fn conversion_colour(&self) -> CapturedFrameColourDescription {
        CapturedFrameColourDescription {
            matrix: Some(matrix_name_or_the_players_fallback(
                self.colour.matrix,
                self.height,
            )),
            ..self.colour
        }
    }
}

/// A driver buffer exported as a DMA-BUF and adopted as a surface.
struct DriverBufferAdoptedAsSurface {
    adopted_surface: ModuleHostSurface,
    /// The exported fd, held for as long as the adoption names it.
    _exported_dma_buf_fd: OwnedFd,
}

/// What the capture thread owns: the device, its adopted buffers, and the
/// node's output.
struct ZeroCopyCaptureLoop {
    device: v4l::Device,
    format: NegotiatedCaptureFormat,
    module_host: ModuleHost,
    video_output_port: ModuleHostOutputPort,
    output_pool_rotation_depth: u32,
    adopted_driver_buffers: Vec<DriverBufferAdoptedAsSurface>,
    stop_requested: Arc<AtomicBool>,
}

/// The camera streaming from its own thread, each frame published as a
/// `VideoFrame` on the lent output port.
pub struct V4l2ZeroCopyCameraCapture {
    format: NegotiatedCaptureFormat,
    stop_requested: Arc<AtomicBool>,
    /// The opened device and its adopted buffers, until [`Self::start`].
    capture_loop_waiting_to_start: Option<ZeroCopyCaptureLoop>,
    capture_thread: Option<JoinHandle<()>>,
}

impl V4l2ZeroCopyCameraCapture {
    /// Open `device_path`, or the first capture device found, negotiate its
    /// format and adopt its buffers, publishing nothing until [`Self::start`].
    pub fn open(
        module_host: ModuleHost,
        video_output_port_name: &str,
        device_path: Option<&str>,
        max_width: u32,
        max_height: u32,
        output_pool_rotation_depth: u32,
    ) -> Result<Self, CameraCaptureRefusal> {
        let device_path = match device_path {
            Some(path) => path.to_string(),
            None => list_v4l2_capture_devices()
                .into_iter()
                .next()
                .map(|(path, _)| path)
                .ok_or_else(|| {
                    CameraCaptureRefusal(
                        "no camera found: nothing under /dev/video* reports video capture"
                            .to_string(),
                    )
                })?,
        };
        let device = v4l::Device::with_path(&device_path).map_err(|open_failure| {
            CameraCaptureRefusal(format!("could not open {device_path}: {open_failure}"))
        })?;
        let card_name = device
            .query_caps()
            .map(|capabilities| capabilities.card)
            .unwrap_or_default();
        let format =
            negotiate_nv12_or_yuyv(&device, &device_path, card_name, max_width, max_height)?;
        let video_output_port = module_host
            .open_output_port(video_output_port_name)
            .map_err(|refusal| CameraCaptureRefusal(refusal.to_string()))?;
        let adopted_driver_buffers =
            export_and_adopt_every_driver_buffer(&device, &format, &module_host)?;

        let stop_requested = Arc::new(AtomicBool::new(false));
        let capture_loop = ZeroCopyCaptureLoop {
            device,
            format: format.clone(),
            module_host,
            video_output_port,
            output_pool_rotation_depth,
            adopted_driver_buffers,
            stop_requested: Arc::clone(&stop_requested),
        };
        Ok(Self {
            format,
            stop_requested,
            capture_loop_waiting_to_start: Some(capture_loop),
            capture_thread: None,
        })
    }

    /// Queue the adopted buffers, stream on and publish every frame from the
    /// capture thread. A capture already started or stopped stays as it is.
    pub fn start(&mut self) -> Result<(), CameraCaptureRefusal> {
        let Some(capture_loop) = self.capture_loop_waiting_to_start.take() else {
            return Ok(());
        };
        queue_every_driver_buffer_and_stream_on(
            &capture_loop.device,
            capture_loop.adopted_driver_buffers.len() as u32,
        )?;
        let capture_thread = std::thread::Builder::new()
            .name("v4l2-zero-copy-camera".to_string())
            .spawn(move || capture_loop.run_until_stopped())
            .map_err(|spawn_failure| {
                CameraCaptureRefusal(format!(
                    "could not start the capture thread: {spawn_failure}"
                ))
            })?;
        self.capture_thread = Some(capture_thread);
        Ok(())
    }

    /// What the device negotiated.
    pub fn format(&self) -> &NegotiatedCaptureFormat {
        &self.format
    }

    /// Stop streaming, give the adopted buffers back and join the capture
    /// thread. Idempotent.
    pub fn stop(&mut self) {
        self.capture_loop_waiting_to_start = None;
        self.stop_requested.store(true, Ordering::Release);
        if let Some(capture_thread) = self.capture_thread.take() {
            let _ = capture_thread.join();
        }
    }
}

impl Drop for V4l2ZeroCopyCameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ZeroCopyCaptureLoop {
    fn run_until_stopped(self) {
        let conversion_colour = self.format.conversion_colour();
        let mut reporting_window = CaptureLoopReportingWindow::default();
        let mut frames_published_in_total = 0u64;
        let mut first_refusal_logged = false;
        while !self.stop_requested.load(Ordering::Acquire) && !self.module_host.stop_requested() {
            match self.wait_for_a_filled_buffer() {
                Ok(true) => {}
                Ok(false) => continue,
                Err(device_failure) => {
                    self.module_host.log(
                        ModuleHostLogLevel::Error,
                        CAMERA_LOG_TARGET,
                        &format!("the camera's stream failed: {device_failure}"),
                    );
                    break;
                }
            }
            let filled = match dequeue_a_filled_buffer(&self.device) {
                Ok(filled) => filled,
                Err(dequeue_failure) => {
                    self.module_host.log(
                        ModuleHostLogLevel::Error,
                        CAMERA_LOG_TARGET,
                        &format!("the camera's stream failed: {dequeue_failure}"),
                    );
                    break;
                }
            };
            let timestamp = resolve_capture_timestamp(
                filled.flags,
                filled.timestamp.tv_sec,
                filled.timestamp.tv_usec,
                monotonic_now_ns(),
            );
            let published =
                self.publish_one_frame(filled.index, timestamp.timestamp_ns(), conversion_colour);
            if let Err(requeue_failure) = queue_a_driver_buffer(&self.device, filled.index) {
                self.module_host.log(
                    ModuleHostLogLevel::Error,
                    CAMERA_LOG_TARGET,
                    &format!("the camera refused its buffer back: {requeue_failure}"),
                );
                break;
            }
            match published {
                Ok(()) => {
                    reporting_window.frames_published += 1;
                    frames_published_in_total += 1;
                    reporting_window
                        .capture_to_publish_ns
                        .push(monotonic_now_ns() - timestamp.timestamp_ns());
                    if frames_published_in_total == 1 {
                        self.module_host.log(
                            ModuleHostLogLevel::Info,
                            CAMERA_LOG_TARGET,
                            &format!("first frame published (timestamp {timestamp:?})"),
                        );
                    }
                }
                Err(refusal) => {
                    reporting_window.frames_dropped += 1;
                    if !first_refusal_logged {
                        first_refusal_logged = true;
                        self.module_host.log(
                            ModuleHostLogLevel::Warn,
                            CAMERA_LOG_TARGET,
                            &format!("dropping a frame: {refusal}"),
                        );
                    }
                }
            }
            if reporting_window.frames_published + reporting_window.frames_dropped
                >= FRAMES_BETWEEN_REPORTS
            {
                self.module_host.log(
                    ModuleHostLogLevel::Info,
                    CAMERA_LOG_TARGET,
                    &reporting_window.summary(),
                );
                reporting_window = CaptureLoopReportingWindow::default();
            }
        }
        if let Err(stream_off_failure) = stream_off(&self.device) {
            self.module_host.log(
                ModuleHostLogLevel::Warn,
                CAMERA_LOG_TARGET,
                &format!("the camera refused to stop streaming: {stream_off_failure}"),
            );
        }
    }

    /// Whether a filled buffer waits to be dequeued, `false` on a timeout or
    /// an interrupted wait, and an error once the device reports a failure or
    /// a hangup, which no later wait recovers from.
    fn wait_for_a_filled_buffer(&self) -> Result<bool, String> {
        let mut readable = libc::pollfd {
            fd: self.device.handle().fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd on the stack, for the length of the call.
        let ready = unsafe { libc::poll(&mut readable, 1, FILLED_BUFFER_WAIT_TIMEOUT_MS) };
        if ready < 0 {
            let poll_failure = std::io::Error::last_os_error();
            return match poll_failure.kind() {
                std::io::ErrorKind::Interrupted => Ok(false),
                _ => Err(format!("waiting on the device failed: {poll_failure}")),
            };
        }
        if ready > 0 && readable.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(format!(
                "the device reported poll events {:#x}",
                readable.revents
            ));
        }
        Ok(ready > 0 && readable.revents & libc::POLLIN != 0)
    }

    fn publish_one_frame(
        &self,
        driver_buffer_index: u32,
        capture_instant_ns: i64,
        conversion_colour: CapturedFrameColourDescription,
    ) -> Result<(), String> {
        let adopted = self
            .adopted_driver_buffers
            .get(driver_buffer_index as usize)
            .ok_or_else(|| {
                format!("the driver filled buffer {driver_buffer_index}, which it never exported")
            })?;
        let converted_frame_texture = self
            .module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: CONVERTED_FRAME_POOL_KEY,
                rotation_depth: self.output_pool_rotation_depth,
                width: self.format.width,
                height: self.format.height,
                texture_format: CONVERTED_FRAME_TEXTURE_FORMAT,
                usage: &CONVERTED_FRAME_TEXTURE_USAGE,
            })
            .map_err(|refusal| refusal.to_string())?;
        self.module_host
            .convert_surface_into(
                &adopted.adopted_surface,
                &converted_frame_texture,
                &ModuleHostColorInfo {
                    primaries: conversion_colour.primaries,
                    transfer: conversion_colour.transfer,
                    matrix: conversion_colour.matrix,
                    range: conversion_colour.range,
                },
            )
            .map_err(|refusal| refusal.to_string())?;
        let timestamp_ns = self
            .module_host
            .stream_time_ns_of_a_machine_instant(capture_instant_ns);
        let record = PublishedVideoFrame {
            surface_id: converted_frame_texture.surface_id(),
            width: self.format.width,
            height: self.format.height,
            timestamp_ns,
            nominal_frames_per_second: self.format.nominal_frames_per_second,
            colour: self.format.colour,
        }
        .to_msgpack();
        self.video_output_port
            .write(&record, Some(timestamp_ns), true)
            .map_err(|refusal| refusal.to_string())
    }
}

fn v4l2_capture_buffer_description(index: u32) -> v4l2_buffer {
    v4l2_buffer {
        index,
        type_: Type::VideoCapture as u32,
        memory: Memory::Mmap as u32,
        // SAFETY: an all-zero `v4l2_buffer` is a valid request once its type
        // and memory are named.
        ..unsafe { std::mem::zeroed() }
    }
}

/// Request the driver's buffers, export each as a DMA-BUF and adopt it as a
/// surface laid out the way the negotiated format lays the pixels out.
fn export_and_adopt_every_driver_buffer(
    device: &v4l::Device,
    format: &NegotiatedCaptureFormat,
    module_host: &ModuleHost,
) -> Result<Vec<DriverBufferAdoptedAsSurface>, CameraCaptureRefusal> {
    let fd = device.handle().fd();
    let mut requested_buffers = v4l2_requestbuffers {
        count: V4L2_DRIVER_BUFFER_COUNT,
        type_: Type::VideoCapture as u32,
        memory: Memory::Mmap as u32,
        // SAFETY: as in `v4l2_capture_buffer_description`.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a live request on the stack and the device's open fd.
    unsafe {
        v4l2::ioctl(
            fd,
            v4l2::vidioc::VIDIOC_REQBUFS,
            std::ptr::from_mut(&mut requested_buffers).cast(),
        )
    }
    .map_err(|request_failure| {
        CameraCaptureRefusal(format!(
            "{} refused {V4L2_DRIVER_BUFFER_COUNT} MMAP buffers: {request_failure}",
            format.device_path
        ))
    })?;
    let (plane_offsets_bytes, plane_strides_bytes) = format.plane_offsets_and_strides_bytes();
    (0..requested_buffers.count)
        .map(|index| {
            let mut described = v4l2_capture_buffer_description(index);
            // SAFETY: as above.
            unsafe {
                v4l2::ioctl(
                    fd,
                    v4l2::vidioc::VIDIOC_QUERYBUF,
                    std::ptr::from_mut(&mut described).cast(),
                )
            }
            .map_err(|query_failure| {
                CameraCaptureRefusal(format!("VIDIOC_QUERYBUF {index} failed: {query_failure}"))
            })?;
            let mut export = v4l2_exportbuffer {
                type_: Type::VideoCapture as u32,
                index,
                flags: (libc::O_CLOEXEC | libc::O_RDWR) as u32,
                // SAFETY: an all-zero export request is valid once its fields
                // above are named.
                ..unsafe { std::mem::zeroed() }
            };
            // SAFETY: as above.
            unsafe {
                v4l2::ioctl(
                    fd,
                    v4l2::vidioc::VIDIOC_EXPBUF,
                    std::ptr::from_mut(&mut export).cast(),
                )
            }
            .map_err(|export_failure| {
                CameraCaptureRefusal(format!(
                    "{} cannot export buffer {index} as a DMA-BUF (VIDIOC_EXPBUF): \
                     {export_failure}",
                    format.device_path
                ))
            })?;
            // SAFETY: VIDIOC_EXPBUF handed this process a new fd, ours alone.
            let exported_dma_buf_fd = unsafe { OwnedFd::from_raw_fd(export.fd) };
            let adopted_surface = module_host
                .import_dma_buf(&ModuleHostForeignDmaBuf {
                    fd: exported_dma_buf_fd.as_raw_fd(),
                    width: format.width,
                    height: format.height,
                    pixel_format: format.pixel_format_wire_name(),
                    allocation_byte_size: u64::from(described.length),
                    plane_offsets_bytes: &plane_offsets_bytes,
                    plane_strides_bytes: &plane_strides_bytes,
                    drm_format_modifier: 0,
                })
                .map_err(|refusal| {
                    CameraCaptureRefusal(format!(
                        "the runtime could not adopt buffer {index}: {refusal}"
                    ))
                })?;
            Ok(DriverBufferAdoptedAsSurface {
                adopted_surface,
                _exported_dma_buf_fd: exported_dma_buf_fd,
            })
        })
        .collect()
}

fn queue_a_driver_buffer(device: &v4l::Device, index: u32) -> std::io::Result<()> {
    let mut queued = v4l2_capture_buffer_description(index);
    // SAFETY: a live request on the stack and the device's open fd.
    unsafe {
        v4l2::ioctl(
            device.handle().fd(),
            v4l2::vidioc::VIDIOC_QBUF,
            std::ptr::from_mut(&mut queued).cast(),
        )
    }
}

fn queue_every_driver_buffer_and_stream_on(
    device: &v4l::Device,
    buffer_count: u32,
) -> Result<(), CameraCaptureRefusal> {
    for index in 0..buffer_count {
        queue_a_driver_buffer(device, index).map_err(|queue_failure| {
            CameraCaptureRefusal(format!("VIDIOC_QBUF {index} failed: {queue_failure}"))
        })?;
    }
    let mut buffer_type = Type::VideoCapture as u32;
    // SAFETY: as in `queue_a_driver_buffer`.
    unsafe {
        v4l2::ioctl(
            device.handle().fd(),
            v4l2::vidioc::VIDIOC_STREAMON,
            std::ptr::from_mut(&mut buffer_type).cast(),
        )
    }
    .map_err(|stream_on_failure| {
        CameraCaptureRefusal(format!("VIDIOC_STREAMON failed: {stream_on_failure}"))
    })
}

fn stream_off(device: &v4l::Device) -> std::io::Result<()> {
    let mut buffer_type = Type::VideoCapture as u32;
    // SAFETY: as in `queue_a_driver_buffer`.
    unsafe {
        v4l2::ioctl(
            device.handle().fd(),
            v4l2::vidioc::VIDIOC_STREAMOFF,
            std::ptr::from_mut(&mut buffer_type).cast(),
        )
    }
}

/// A buffer the driver filled: its index, flags and stamp.
struct FilledDriverBuffer {
    index: u32,
    flags: u32,
    timestamp: libc::timeval,
}

fn dequeue_a_filled_buffer(device: &v4l::Device) -> std::io::Result<FilledDriverBuffer> {
    let mut dequeued = v4l2_capture_buffer_description(0);
    // SAFETY: as in `queue_a_driver_buffer`.
    unsafe {
        v4l2::ioctl(
            device.handle().fd(),
            v4l2::vidioc::VIDIOC_DQBUF,
            std::ptr::from_mut(&mut dequeued).cast(),
        )
    }?;
    Ok(FilledDriverBuffer {
        index: dequeued.index,
        flags: dequeued.flags,
        timestamp: libc::timeval {
            tv_sec: dequeued.timestamp.tv_sec,
            tv_usec: dequeued.timestamp.tv_usec,
        },
    })
}

/// Every `/dev/video*` node that reports video capture, as `(path, card name)`, by node index.
pub fn list_v4l2_capture_devices() -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut devices: Vec<(u32, String, String)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_name = entry.file_name().into_string().ok()?;
            let node_index = file_name.strip_prefix("video")?.parse::<u32>().ok()?;
            let path = entry.path().to_string_lossy().into_owned();
            let capabilities = v4l::Device::with_path(&path).ok()?.query_caps().ok()?;
            capabilities
                .capabilities
                .contains(v4l::capability::Flags::VIDEO_CAPTURE)
                .then_some((node_index, path, capabilities.card))
        })
        .collect();
    devices.sort_by_key(|(node_index, _, _)| *node_index);
    devices
        .into_iter()
        .map(|(_, path, card_name)| (path, card_name))
        .collect()
}

/// NV12 first, then YUYV, each at the largest frame size within the cap.
fn negotiate_nv12_or_yuyv(
    device: &v4l::Device,
    device_path: &str,
    card_name: String,
    max_width: u32,
    max_height: u32,
) -> Result<NegotiatedCaptureFormat, CameraCaptureRefusal> {
    let current_format = device.format().map_err(|query_failure| {
        CameraCaptureRefusal(format!(
            "{device_path} refused VIDIOC_G_FMT: {query_failure}"
        ))
    })?;
    for pixel_layout in [CapturedPixelLayout::Nv12, CapturedPixelLayout::Yuyv] {
        let fourcc = FourCC::new(match pixel_layout {
            CapturedPixelLayout::Nv12 => b"NV12",
            CapturedPixelLayout::Yuyv => b"YUYV",
        });
        let Some((width, height)) = device
            .enum_framesizes(fourcc)
            .ok()
            .and_then(|frame_sizes| largest_frame_size_within(&frame_sizes, max_width, max_height))
        else {
            continue;
        };
        let mut requested_format = current_format;
        requested_format.fourcc = fourcc;
        requested_format.width = width;
        requested_format.height = height;
        let Ok(negotiated) = device.set_format(&requested_format) else {
            continue;
        };
        if negotiated.fourcc != fourcc {
            continue;
        }
        let nominal_frames_per_second = device.params().ok().and_then(|parameters| {
            let interval = parameters.interval;
            (interval.numerator > 0).then(|| interval.denominator / interval.numerator)
        });
        return Ok(NegotiatedCaptureFormat {
            device_path: device_path.to_string(),
            card_name,
            pixel_layout,
            width: negotiated.width,
            height: negotiated.height,
            bytes_per_line: negotiated.stride,
            colour: query_colour_fields(device)?.colour_description(),
            nominal_frames_per_second,
        });
    }
    Err(CameraCaptureRefusal(format!(
        "{device_path} offers neither NV12 nor YUYV within {max_width}x{max_height}; the \
         conversion kernel reads only those two"
    )))
}

fn largest_frame_size_within(
    frame_sizes: &[v4l::framesize::FrameSize],
    max_width: u32,
    max_height: u32,
) -> Option<(u32, u32)> {
    frame_sizes
        .iter()
        .map(|frame_size| match &frame_size.size {
            v4l::framesize::FrameSizeEnum::Discrete(discrete) => (discrete.width, discrete.height),
            v4l::framesize::FrameSizeEnum::Stepwise(stepwise) => (
                stepwise.max_width.min(max_width),
                stepwise.max_height.min(max_height),
            ),
        })
        .filter(|(width, height)| *width <= max_width && *height <= max_height)
        .max_by_key(|(width, height)| u64::from(*width) * u64::from(*height))
}

/// The colour fields `v4l::format::Format` leaves out: it carries no `ycbcr_enc`.
fn query_colour_fields(
    device: &v4l::Device,
) -> Result<V4l2CaptureFormatColourFields, CameraCaptureRefusal> {
    // SAFETY: an all-zero `v4l2_format` is a valid request once `type_` names
    // the capture queue, and the handle's fd is open for the device's life.
    unsafe {
        let mut v4l2_format: v4l::v4l_sys::v4l2_format = std::mem::zeroed();
        v4l2_format.type_ = Type::VideoCapture as u32;
        if libc::ioctl(
            device.handle().fd(),
            v4l::v4l2::vidioc::VIDIOC_G_FMT as libc::c_ulong,
            &mut v4l2_format,
        ) != 0
        {
            return Err(CameraCaptureRefusal(format!(
                "VIDIOC_G_FMT failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let pix = v4l2_format.fmt.pix;
        Ok(V4l2CaptureFormatColourFields {
            colorspace: pix.colorspace,
            xfer_func: pix.xfer_func,
            ycbcr_enc: pix.__bindgen_anon_1.ycbcr_enc,
            quantization: pix.quantization,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_nv12_frame_carries_half_again_as_many_rows_as_its_height() {
        assert_eq!(CapturedPixelLayout::Nv12.rows_in_one_frame(1080), 1620);
        assert_eq!(CapturedPixelLayout::Yuyv.rows_in_one_frame(1080), 1080);
    }

    #[test]
    fn the_largest_discrete_size_within_the_cap_is_chosen() {
        let discrete = |width, height| v4l::framesize::FrameSize {
            index: 0,
            fourcc: FourCC::new(b"YUYV"),
            typ: 0,
            size: v4l::framesize::FrameSizeEnum::Discrete(v4l::framesize::Discrete {
                width,
                height,
            }),
        };
        let offered = [
            discrete(640, 480),
            discrete(3840, 2160),
            discrete(1920, 1080),
        ];
        assert_eq!(
            largest_frame_size_within(&offered, 1920, 1080),
            Some((1920, 1080))
        );
        assert_eq!(largest_frame_size_within(&offered, 320, 240), None);
    }
}
