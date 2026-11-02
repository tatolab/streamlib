// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! V4L2 MMAP capture on a thread this wheel owns, keeping only the newest frame.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use v4l::FourCC;
use v4l::buffer::Type;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;

use crate::capture_timestamp::{
    ResolvedCaptureTimestamp, monotonic_now_ns, resolve_capture_timestamp,
};
use crate::v4l2_colour_description::{
    CapturedFrameColourDescription, V4l2CaptureFormatColourFields,
};

const V4L2_MMAP_BUFFER_COUNT: u32 = 4;

/// How long one dequeue waits before the capture thread looks at its stop flag again.
const DEQUEUE_TIMEOUT: Duration = Duration::from_millis(200);

/// Why a camera could not be opened, or why its capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraCaptureRefusal(pub String);

impl fmt::Display for CameraCaptureRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CameraCaptureRefusal {}

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

    /// The bytes a delivered frame carries: every row, padding included.
    pub fn delivered_frame_byte_size(&self) -> usize {
        self.bytes_per_line as usize * self.rows_in_one_frame() as usize
    }
}

/// One frame handed from the capture thread to its reader.
#[derive(Debug, Clone, Copy)]
pub struct CapturedFrameReadout {
    pub timestamp: ResolvedCaptureTimestamp,
    pub driver_sequence: u32,
    pub frames_captured: u64,
    pub frames_replaced_before_they_were_read: u64,
}

struct NewestCapturedFrameSlot {
    frame_bytes: Vec<u8>,
    readout: Option<CapturedFrameReadout>,
    unread: bool,
    frames_captured: u64,
    frames_replaced_before_they_were_read: u64,
    capture_ended_because: Option<String>,
}

struct CaptureThreadSharedState {
    newest_frame: Mutex<NewestCapturedFrameSlot>,
    newest_frame_arrived: Condvar,
    stop_requested: AtomicBool,
}

impl CaptureThreadSharedState {
    fn lock_newest_frame(&self) -> MutexGuard<'_, NewestCapturedFrameSlot> {
        self.newest_frame
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A camera streaming from its own thread; the reader always gets the newest frame.
pub struct V4l2CameraCapture {
    format: NegotiatedCaptureFormat,
    shared_state: Arc<CaptureThreadSharedState>,
    capture_thread: Option<JoinHandle<()>>,
}

impl V4l2CameraCapture {
    /// Open `device_path`, or the first capture device found, and start streaming.
    pub fn open(
        device_path: Option<&str>,
        max_width: u32,
        max_height: u32,
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

        let delivered_frame_byte_size = format.delivered_frame_byte_size();
        let shared_state = Arc::new(CaptureThreadSharedState {
            newest_frame: Mutex::new(NewestCapturedFrameSlot {
                frame_bytes: Vec::with_capacity(delivered_frame_byte_size),
                readout: None,
                unread: false,
                frames_captured: 0,
                frames_replaced_before_they_were_read: 0,
                capture_ended_because: None,
            }),
            newest_frame_arrived: Condvar::new(),
            stop_requested: AtomicBool::new(false),
        });

        let (stream_started_sender, stream_started_receiver) = mpsc::channel();
        let thread_shared_state = Arc::clone(&shared_state);
        let capture_thread = std::thread::Builder::new()
            .name("v4l2-camera-capture".to_string())
            .spawn(move || {
                run_capture_thread(
                    device,
                    delivered_frame_byte_size,
                    thread_shared_state,
                    stream_started_sender,
                )
            })
            .map_err(|spawn_failure| {
                CameraCaptureRefusal(format!(
                    "could not start the capture thread: {spawn_failure}"
                ))
            })?;
        match stream_started_receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(refusal)) => {
                let _ = capture_thread.join();
                return Err(refusal);
            }
            Err(_) => {
                let _ = capture_thread.join();
                return Err(CameraCaptureRefusal(
                    "the capture thread ended before its stream started".to_string(),
                ));
            }
        }

        Ok(Self {
            format,
            shared_state,
            capture_thread: Some(capture_thread),
        })
    }

    /// What the device negotiated.
    pub fn format(&self) -> &NegotiatedCaptureFormat {
        &self.format
    }

    /// Wait up to `timeout` for a frame newer than the last one read, swapping its bytes into `frame_bytes`.
    ///
    /// `Ok(None)` is a timeout. `Err` names why capture ended on its own.
    pub fn wait_for_newest_frame(
        &self,
        timeout: Duration,
        frame_bytes: &mut Vec<u8>,
    ) -> Result<Option<CapturedFrameReadout>, CameraCaptureRefusal> {
        let slot = self.shared_state.lock_newest_frame();
        let (mut slot, _) = self
            .shared_state
            .newest_frame_arrived
            .wait_timeout_while(slot, timeout, |slot| {
                !slot.unread && slot.capture_ended_because.is_none()
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !slot.unread {
            return match &slot.capture_ended_because {
                Some(reason) => Err(CameraCaptureRefusal(reason.clone())),
                None => Ok(None),
            };
        }
        slot.unread = false;
        std::mem::swap(&mut slot.frame_bytes, frame_bytes);
        Ok(slot.readout)
    }

    /// Stop streaming and join the capture thread. Idempotent.
    pub fn stop(&mut self) {
        self.shared_state
            .stop_requested
            .store(true, Ordering::Release);
        if let Some(capture_thread) = self.capture_thread.take() {
            let _ = capture_thread.join();
        }
    }
}

impl Drop for V4l2CameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
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

fn run_capture_thread(
    device: v4l::Device,
    delivered_frame_byte_size: usize,
    shared_state: Arc<CaptureThreadSharedState>,
    stream_started_sender: mpsc::Sender<Result<(), CameraCaptureRefusal>>,
) {
    let mut stream =
        match MmapStream::with_buffers(&device, Type::VideoCapture, V4L2_MMAP_BUFFER_COUNT) {
            Ok(stream) => stream,
            Err(request_failure) => {
                let _ = stream_started_sender.send(Err(CameraCaptureRefusal(format!(
                    "the device refused {V4L2_MMAP_BUFFER_COUNT} MMAP buffers: {request_failure}"
                ))));
                return;
            }
        };
    stream.set_timeout(DEQUEUE_TIMEOUT);
    let _ = stream_started_sender.send(Ok(()));

    let mut filling_frame_bytes = Vec::with_capacity(delivered_frame_byte_size);
    while !shared_state.stop_requested.load(Ordering::Acquire) {
        let (driver_buffer, metadata) = match stream.next() {
            Ok(frame) => frame,
            Err(dequeue_failure) if dequeue_failure.kind() == std::io::ErrorKind::TimedOut => {
                continue;
            }
            Err(dequeue_failure) => {
                shared_state.lock_newest_frame().capture_ended_because =
                    Some(format!("the device's stream failed: {dequeue_failure}"));
                shared_state.newest_frame_arrived.notify_all();
                return;
            }
        };
        let timestamp = resolve_capture_timestamp(
            metadata.flags.bits(),
            metadata.timestamp.sec,
            metadata.timestamp.usec,
            monotonic_now_ns(),
        );
        filling_frame_bytes.clear();
        let copied_byte_count = driver_buffer.len().min(delivered_frame_byte_size);
        filling_frame_bytes.extend_from_slice(&driver_buffer[..copied_byte_count]);
        filling_frame_bytes.resize(delivered_frame_byte_size, 0);

        let mut slot = shared_state.lock_newest_frame();
        std::mem::swap(&mut slot.frame_bytes, &mut filling_frame_bytes);
        if slot.unread {
            slot.frames_replaced_before_they_were_read += 1;
        }
        slot.frames_captured += 1;
        slot.unread = true;
        slot.readout = Some(CapturedFrameReadout {
            timestamp,
            driver_sequence: metadata.sequence,
            frames_captured: slot.frames_captured,
            frames_replaced_before_they_were_read: slot.frames_replaced_before_they_were_read,
        });
        drop(slot);
        shared_state.newest_frame_arrived.notify_all();
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
