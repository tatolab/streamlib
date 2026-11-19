// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The driver's capture queue, driven buffer by buffer: every buffer is
//! queued once at the start, each filled one is dequeued, and it is queued
//! again only after its frame was read. A wait that ends with no frame queues
//! nothing, so a frame that comes late, however late, never re-queues a
//! buffer the driver already holds.

#[cfg(test)]
mod tests;

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use v4l::v4l_sys::v4l2_buffer;

/// How long one wait for a filled buffer lasts before the capture thread
/// looks again at whether it was asked to stop.
pub(super) const WAIT_FOR_A_FILLED_BUFFER_BEFORE_LOOKING_FOR_A_STOP: Duration =
    Duration::from_secs(1);

/// What the capture thread asks of a driver's capture queue.
pub(super) trait V4l2CaptureBufferQueue {
    /// Wait up to `wait` for a filled buffer; `Ok(false)` when none came.
    fn wait_for_a_filled_buffer(&mut self, wait: Duration) -> io::Result<bool>;

    /// Take a filled buffer from the driver.
    fn dequeue_a_filled_buffer(&mut self) -> io::Result<v4l2_buffer>;

    /// Give a dequeued buffer back to the driver to fill again.
    fn queue_a_dequeued_buffer(&mut self, buffer: v4l2_buffer) -> io::Result<()>;
}

/// The next frame's buffer, or why there is none.
pub(super) enum NextFilledV4l2Buffer {
    Filled(v4l2_buffer),
    CaptureStopped,
    DeviceFailed(String),
}

impl std::fmt::Debug for NextFilledV4l2Buffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Filled(buffer) => formatter
                .debug_struct("Filled")
                .field("index", &buffer.index)
                .field("sequence", &buffer.sequence)
                .finish(),
            Self::CaptureStopped => formatter.write_str("CaptureStopped"),
            Self::DeviceFailed(reason) => {
                formatter.debug_tuple("DeviceFailed").field(reason).finish()
            }
        }
    }
}

/// Wait for the next filled buffer through every wait that ends empty, until
/// a frame comes, the capture is asked to stop, or the device fails.
pub(super) fn wait_for_the_next_filled_v4l2_buffer(
    queue: &mut impl V4l2CaptureBufferQueue,
    is_capturing: &AtomicBool,
) -> NextFilledV4l2Buffer {
    while is_capturing.load(Ordering::Acquire) {
        match queue.wait_for_a_filled_buffer(WAIT_FOR_A_FILLED_BUFFER_BEFORE_LOOKING_FOR_A_STOP) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(wait_error) if wait_error.kind() == io::ErrorKind::Interrupted => continue,
            Err(wait_error) => {
                return NextFilledV4l2Buffer::DeviceFailed(format!(
                    "polling the device failed: {wait_error}"
                ));
            }
        }
        match queue.dequeue_a_filled_buffer() {
            Ok(filled) => return NextFilledV4l2Buffer::Filled(filled),
            Err(dequeue_error)
                if matches!(
                    dequeue_error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(dequeue_error) => {
                return NextFilledV4l2Buffer::DeviceFailed(format!(
                    "the device's stream failed: {dequeue_error}"
                ));
            }
        }
    }
    NextFilledV4l2Buffer::CaptureStopped
}

/// Give a buffer whose frame was read back to the driver. A failure is
/// logged: it takes that buffer out of the queue for good, and the capture
/// starves once every buffer is gone.
pub(super) fn requeue_a_read_v4l2_buffer(
    queue: &mut impl V4l2CaptureBufferQueue,
    camera_name: &str,
    buffer: v4l2_buffer,
) {
    let buffer_index = buffer.index;
    if let Err(queue_error) = queue.queue_a_dequeued_buffer(buffer) {
        tracing::error!(
            camera = camera_name,
            buffer_index,
            error = %queue_error,
            "VIDIOC_QBUF requeue failed — one capture buffer lost"
        );
    }
}

/// The capture queue of an open V4L2 device whose buffers are memory-mapped.
pub(super) struct V4l2DeviceCaptureBufferQueue {
    device_fd: RawFd,
}

impl V4l2DeviceCaptureBufferQueue {
    pub(super) fn of_the_device(device_fd: RawFd) -> Self {
        Self { device_fd }
    }

    /// Queue every one of the driver's `buffer_count` buffers and turn
    /// streaming on. A failed STREAMON is returned; a failed initial QBUF is
    /// logged and streaming starts with the buffers that queued.
    pub(super) fn queue_every_buffer_and_stream_on(
        &mut self,
        buffer_count: u32,
        camera_name: &str,
    ) -> io::Result<()> {
        for buffer_index in 0..buffer_count {
            let mut buffer = memory_mapped_capture_buffer();
            buffer.index = buffer_index;
            if let Err(queue_error) = self.queue_a_dequeued_buffer(buffer) {
                tracing::error!(
                    camera = camera_name,
                    buffer_index,
                    error = %queue_error,
                    "initial VIDIOC_QBUF failed"
                );
            }
        }
        let mut buffer_type: u32 = v4l::buffer::Type::VideoCapture as u32;
        // SAFETY: `device_fd` is the open device and STREAMON reads one u32.
        let result = unsafe {
            libc::ioctl(
                self.device_fd,
                v4l::v4l2::vidioc::VIDIOC_STREAMON as libc::c_ulong,
                &mut buffer_type,
            )
        };
        if result != 0 {
            let streamon_error = io::Error::last_os_error();
            tracing::error!(
                camera = camera_name,
                error = %streamon_error,
                "VIDIOC_STREAMON failed — camera produces no frames; stopping capture thread"
            );
            return Err(streamon_error);
        }
        Ok(())
    }

    /// Turn streaming off, which hands every buffer back to the capture.
    pub(super) fn stream_off(&mut self) {
        let mut buffer_type: u32 = v4l::buffer::Type::VideoCapture as u32;
        // SAFETY: `device_fd` is the open device and STREAMOFF reads one u32.
        unsafe {
            libc::ioctl(
                self.device_fd,
                v4l::v4l2::vidioc::VIDIOC_STREAMOFF as libc::c_ulong,
                &mut buffer_type,
            );
        }
    }
}

impl V4l2CaptureBufferQueue for V4l2DeviceCaptureBufferQueue {
    fn wait_for_a_filled_buffer(&mut self, wait: Duration) -> io::Result<bool> {
        let mut readiness = libc::pollfd {
            fd: self.device_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait_milliseconds = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `readiness` is one live pollfd for the duration of the call.
        match unsafe { libc::poll(&mut readiness, 1, wait_milliseconds) } {
            0 => Ok(false),
            ready if ready > 0 => Ok(true),
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn dequeue_a_filled_buffer(&mut self) -> io::Result<v4l2_buffer> {
        let mut buffer = memory_mapped_capture_buffer();
        // SAFETY: `buffer` is a capture buffer descriptor DQBUF fills in.
        let result = unsafe {
            libc::ioctl(
                self.device_fd,
                v4l::v4l2::vidioc::VIDIOC_DQBUF as libc::c_ulong,
                &mut buffer,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buffer)
    }

    fn queue_a_dequeued_buffer(&mut self, mut buffer: v4l2_buffer) -> io::Result<()> {
        // SAFETY: `buffer` names one of the device's memory-mapped buffers.
        let result = unsafe {
            libc::ioctl(
                self.device_fd,
                v4l::v4l2::vidioc::VIDIOC_QBUF as libc::c_ulong,
                &mut buffer,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// An empty descriptor of one of the device's memory-mapped capture buffers.
pub(super) fn memory_mapped_capture_buffer() -> v4l2_buffer {
    // SAFETY: `v4l2_buffer` is a plain C struct for which all-zero is valid.
    let mut buffer: v4l2_buffer = unsafe { std::mem::zeroed() };
    buffer.type_ = v4l::buffer::Type::VideoCapture as u32;
    buffer.memory = v4l::memory::Memory::Mmap as u32;
    buffer
}
