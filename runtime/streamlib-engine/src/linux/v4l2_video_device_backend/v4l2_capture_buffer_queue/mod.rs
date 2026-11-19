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
use std::marker::PhantomData;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use v4l::v4l_sys::v4l2_buffer;
use v4l::v4l2::vidioc;

/// The most buffers a V4L2 queue holds (`VIDEO_MAX_FRAME`).
const VIDEO_MAX_FRAME: u32 = 64;

/// How long one wait for a filled buffer lasts before the capture thread
/// looks again at whether it was asked to stop.
pub(super) const WAIT_FOR_A_FILLED_BUFFER_BEFORE_LOOKING_FOR_A_STOP: Duration =
    Duration::from_secs(1);

/// A buffer this side holds, out of the driver's queue: dequeued, or not yet
/// queued at the start. Queuing it gives it up, so nothing can read a buffer
/// the driver is filling.
pub(super) struct HeldV4l2Buffer(v4l2_buffer);

impl HeldV4l2Buffer {
    /// The buffer at `index`, before it was first queued.
    pub(super) fn not_yet_queued(index: u32) -> Self {
        Self(memory_mapped_capture_buffer(index))
    }

    pub(super) fn index(&self) -> u32 {
        self.0.index
    }

    pub(super) fn sequence(&self) -> u32 {
        self.0.sequence
    }

    pub(super) fn flags(&self) -> u32 {
        self.0.flags
    }

    pub(super) fn timestamp(&self) -> v4l::v4l_sys::timeval {
        self.0.timestamp
    }

    /// Whether the driver marked the frame in it as damaged, which it still
    /// hands back through a successful dequeue.
    fn frame_is_damaged(&self) -> bool {
        self.0.flags & v4l::buffer::Flags::ERROR.bits() != 0
    }
}

impl std::fmt::Debug for HeldV4l2Buffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeldV4l2Buffer")
            .field("index", &self.0.index)
            .field("sequence", &self.0.sequence)
            .finish()
    }
}

/// The next frame's buffer, or why there is none.
#[derive(Debug)]
pub(super) enum NextFilledV4l2Buffer {
    Filled(HeldV4l2Buffer),
    CaptureStopped,
    DeviceFailed(String),
}

/// What the capture thread asks of a driver's capture queue.
pub(super) trait V4l2CaptureBufferQueue {
    /// Wait up to `wait` for a filled buffer; `Ok(false)` when none came.
    fn wait_for_a_filled_buffer(&mut self, wait: Duration) -> io::Result<bool>;

    /// Take a filled buffer from the driver.
    fn dequeue_a_filled_buffer(&mut self) -> io::Result<HeldV4l2Buffer>;

    /// Give a held buffer to the driver to fill.
    fn queue_a_held_buffer(&mut self, buffer: HeldV4l2Buffer) -> io::Result<()>;

    /// Wait for the next filled buffer through every wait that ends empty and
    /// every damaged frame, until a frame comes, the capture is asked to stop,
    /// or the device fails.
    fn wait_for_the_next_filled_buffer(
        &mut self,
        camera_name: &str,
        is_capturing: &AtomicBool,
    ) -> NextFilledV4l2Buffer {
        while is_capturing.load(Ordering::Acquire) {
            match self.wait_for_a_filled_buffer(WAIT_FOR_A_FILLED_BUFFER_BEFORE_LOOKING_FOR_A_STOP)
            {
                Ok(false) => continue,
                Ok(true) => {}
                Err(wait_error) if wait_error.kind() == io::ErrorKind::Interrupted => continue,
                Err(wait_error) => {
                    return NextFilledV4l2Buffer::DeviceFailed(format!(
                        "polling the device failed: {wait_error}"
                    ));
                }
            }
            match self.dequeue_a_filled_buffer() {
                Ok(damaged) if damaged.frame_is_damaged() => {
                    tracing::debug!(
                        camera = camera_name,
                        sequence = damaged.sequence(),
                        "the driver marked a frame damaged; dropping it"
                    );
                    self.requeue_a_read_buffer(camera_name, damaged);
                }
                Ok(filled) => return NextFilledV4l2Buffer::Filled(filled),
                Err(dequeue_error)
                    if matches!(
                        dequeue_error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
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
    fn requeue_a_read_buffer(&mut self, camera_name: &str, buffer: HeldV4l2Buffer) {
        let buffer_index = buffer.index();
        if let Err(queue_error) = self.queue_a_held_buffer(buffer) {
            tracing::error!(
                camera = camera_name,
                buffer_index,
                error = %queue_error,
                "VIDIOC_QBUF requeue failed — one capture buffer lost"
            );
        }
    }
}

/// The capture queue of the device a `v4l` stream requested its buffers on,
/// borrowing that stream so the queue cannot outlive the buffers.
pub(super) struct V4l2DeviceCaptureBufferQueue<'stream> {
    device_fd: RawFd,
    buffer_count: u32,
    stream: PhantomData<&'stream v4l::io::mmap::Stream<'stream>>,
}

impl<'stream> V4l2DeviceCaptureBufferQueue<'stream> {
    /// The queue of `stream`'s device, holding as many buffers as the driver
    /// granted, which may differ from the count the stream asked for.
    pub(super) fn of_the_stream(stream: &'stream v4l::io::mmap::Stream<'stream>) -> Self {
        let device_fd = stream.handle().fd();
        let buffer_count = (0..VIDEO_MAX_FRAME)
            .take_while(|&index| query_a_driver_buffer(device_fd, index).is_ok())
            .count();
        Self {
            device_fd,
            buffer_count: u32::try_from(buffer_count).unwrap_or(u32::MAX),
            stream: PhantomData,
        }
    }

    pub(super) fn device_fd(&self) -> RawFd {
        self.device_fd
    }

    pub(super) fn buffer_count(&self) -> u32 {
        self.buffer_count
    }

    /// Queue every buffer the driver granted and turn streaming on. A failed
    /// initial QBUF is logged and streaming starts with the buffers that
    /// queued; with none of them queued, or a failed STREAMON, capture
    /// cannot start. The first wait comes after this: a strict driver
    /// (v4l2loopback) signals no buffer as filled before STREAMON.
    pub(super) fn queue_every_buffer_and_stream_on(&mut self, camera_name: &str) -> io::Result<()> {
        let mut buffers_queued = 0;
        for buffer_index in 0..self.buffer_count {
            match self.queue_a_held_buffer(HeldV4l2Buffer::not_yet_queued(buffer_index)) {
                Ok(()) => buffers_queued += 1,
                Err(queue_error) => tracing::error!(
                    camera = camera_name,
                    buffer_index,
                    error = %queue_error,
                    "initial VIDIOC_QBUF failed"
                ),
            }
        }
        if buffers_queued == 0 {
            return Err(io::Error::other(format!(
                "none of the driver's {} capture buffers could be queued",
                self.buffer_count
            )));
        }
        self.set_streaming(vidioc::VIDIOC_STREAMON)
    }

    /// Turn streaming off, which hands every buffer back to the capture.
    pub(super) fn stream_off(&mut self) {
        let _ = self.set_streaming(vidioc::VIDIOC_STREAMOFF);
    }

    fn set_streaming(&mut self, request: vidioc::_IOC_TYPE) -> io::Result<()> {
        let mut buffer_type = v4l::buffer::Type::VideoCapture as u32;
        // SAFETY: STREAMON and STREAMOFF read one u32 buffer type.
        unsafe { v4l::v4l2::ioctl(self.device_fd, request, (&raw mut buffer_type).cast()) }
    }
}

impl V4l2CaptureBufferQueue for V4l2DeviceCaptureBufferQueue<'_> {
    fn wait_for_a_filled_buffer(&mut self, wait: Duration) -> io::Result<bool> {
        let mut readiness = libc::pollfd {
            fd: self.device_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait_milliseconds = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `readiness` is one live pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut readiness, 1, wait_milliseconds) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        // An error or hangup is never followed by a frame, so it ends the
        // capture rather than counting as a filled buffer.
        if ready > 0 && readiness.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::other(format!(
                "the device reported poll events {:#x}",
                readiness.revents
            )));
        }
        Ok(ready > 0 && readiness.revents & libc::POLLIN != 0)
    }

    fn dequeue_a_filled_buffer(&mut self) -> io::Result<HeldV4l2Buffer> {
        let mut buffer = memory_mapped_capture_buffer(0);
        // SAFETY: DQBUF fills in one capture buffer descriptor.
        unsafe {
            v4l::v4l2::ioctl(
                self.device_fd,
                vidioc::VIDIOC_DQBUF,
                (&raw mut buffer).cast(),
            )?;
        }
        Ok(HeldV4l2Buffer(buffer))
    }

    fn queue_a_held_buffer(&mut self, buffer: HeldV4l2Buffer) -> io::Result<()> {
        let mut buffer = buffer.0;
        // SAFETY: QBUF reads one descriptor naming one of the device's buffers.
        unsafe {
            v4l::v4l2::ioctl(
                self.device_fd,
                vidioc::VIDIOC_QBUF,
                (&raw mut buffer).cast(),
            )
        }
    }
}

/// What the driver says about its buffer at `index`: its offset and length.
pub(super) fn query_a_driver_buffer(device_fd: RawFd, index: u32) -> io::Result<v4l2_buffer> {
    let mut buffer = memory_mapped_capture_buffer(index);
    // SAFETY: QUERYBUF fills in one capture buffer descriptor.
    unsafe {
        v4l::v4l2::ioctl(device_fd, vidioc::VIDIOC_QUERYBUF, (&raw mut buffer).cast())?;
    }
    Ok(buffer)
}

/// A descriptor of the device's memory-mapped capture buffer at `index`.
fn memory_mapped_capture_buffer(index: u32) -> v4l2_buffer {
    // SAFETY: `v4l2_buffer` is a plain C struct for which all-zero is valid.
    let mut buffer: v4l2_buffer = unsafe { std::mem::zeroed() };
    buffer.type_ = v4l::buffer::Type::VideoCapture as u32;
    buffer.memory = v4l::memory::Memory::Mmap as u32;
    buffer.index = index;
    buffer
}
