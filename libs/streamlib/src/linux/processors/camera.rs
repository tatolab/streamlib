// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// TODO(#166): V4L2 camera implementation
// Blocked by:
//   - #178 (Cross-platform PixelFormat) — Linux PixelFormat is currently { Unknown }
//   - VulkanFormatConverter::convert() returns NotSupported — NV12→RGBA needed
//
// Implementation plan:
//   1. Open /dev/video0 (configurable device path via config.device_id)
//   2. Set format via VIDIOC_S_FMT (YUYV, MJPEG, or H.264 raw)
//   3. Request buffers via VIDIOC_REQBUFS (mmap or DMA-BUF export)
//   4. Start streaming via VIDIOC_STREAMON
//   5. Poll for frames via epoll on the V4L2 fd
//   6. On frame: convert to RGBA if needed, write to iceoryx2 OutputWriter
//   7. For DMA-BUF zero-copy: request V4L2_MEMORY_DMABUF buffers, pass fd directly
//
// Dependencies needed in Cargo.toml:
//   v4l = "0.14"  (or raw libc ioctl)
//
// Hardware testing required — cannot validate without a real V4L2 device.

use crate::core::{Result, RuntimeContext, StreamError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct LinuxCameraDevice {
    pub id: String,
    pub name: String,
}

#[crate::processor("com.tatolab.camera")]
pub struct LinuxCameraProcessor {
    camera_name: String,
    gpu_context: Option<crate::core::GpuContext>,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
}

impl crate::core::ManualProcessor for LinuxCameraProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        self.gpu_context = Some(ctx.gpu.clone());
        tracing::info!("Camera: setup() complete");
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let frame_count = self.frame_counter.load(Ordering::Relaxed);
        tracing::info!(
            "Camera {}: Teardown (generated {} frames)",
            self.camera_name,
            frame_count
        );
        self.is_capturing.store(false, Ordering::Relaxed);
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        // TODO(#166): Implement V4L2 capture session initialization
        // Steps:
        //   1. Open V4L2 device (config.device_id or /dev/video0)
        //   2. Query and set pixel format (VIDIOC_S_FMT)
        //   3. Request mmap buffers (VIDIOC_REQBUFS)
        //   4. Queue buffers (VIDIOC_QBUF)
        //   5. Start streaming (VIDIOC_STREAMON)
        //   6. Spawn polling thread with epoll on V4L2 fd
        //   7. In poll callback: dequeue buffer, convert format, write to OutputWriter
        //
        // Blocked by #178 (Cross-platform PixelFormat)
        Err(StreamError::Configuration(
            "Linux V4L2 camera not yet implemented — blocked by #178 (Cross-platform PixelFormat)".into(),
        ))
    }

    fn stop(&mut self) -> Result<()> {
        self.is_capturing.store(false, Ordering::Relaxed);
        tracing::info!(
            "Camera {}: Stopped ({} frames)",
            self.camera_name,
            self.frame_counter.load(Ordering::Relaxed)
        );
        Ok(())
    }
}

impl LinuxCameraProcessor::Processor {
    pub fn list_devices() -> Result<Vec<LinuxCameraDevice>> {
        // TODO(#166): Enumerate V4L2 devices from /dev/video*
        // For each device:
        //   1. Open /dev/videoN
        //   2. VIDIOC_QUERYCAP to get device name
        //   3. Check V4L2_CAP_VIDEO_CAPTURE capability
        //   4. Close device
        tracing::warn!("Linux camera device enumeration not yet implemented");
        Ok(Vec::new())
    }
}
