// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The V4L2 capture thread: it reads the layout the device negotiated, sets
//! up the GPU resources its frames convert on, streams them until stopped,
//! and turns the stream off.

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use v4l::FourCC;

use crate::core::color::{ColorSpaceKind, H273ColorVui, RangeId, ResolveH273ColorVuiDefaults};
use crate::core::context::{
    CapturedVideoFrameHandOff, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
    GpuContextLimitedAccess, VideoCaptureInstantResolver,
};
use crate::core::rhi::{PixelFormat, SourceLayoutInfo};
use crate::linux::v4l2_color::v4l2_color_to_h273_color_vui;

use super::V4L2_BUFFER_COUNT;
use super::v4l2_capture_dequeue_loop::{V4l2CaptureDequeueLoopInputs, run_the_v4l2_dequeue_loop};
use super::v4l2_capture_gpu_resources::{
    CameraGpuResources, log_the_v4l2_capture_buffer_path_to_the_gpu,
    set_up_the_camera_gpu_resources,
};

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

pub(super) fn capture_thread_loop(
    mut stream: v4l::io::mmap::Stream,
    is_capturing: Arc<AtomicBool>,
    hand_off: CapturedVideoFrameHandOff,
    gpu_context: GpuContextLimitedAccess,
    processor_id_of_the_capturing_node: String,
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
    let is_virtual_device = query_whether_the_v4l2_capture_device_is_virtual(device_fd);

    // Query V4L2 format once at start: (1) the colorspace 4-tuple, as the
    // H.273 description every frame carries, (2) `bytesperline` for the
    // source SSBO stride (vivid + some UVC drivers report stride > width even
    // for NV12), (3) `sizeimage` for the SSBO allocation (must hold the full
    // V4L2 frame including padding). V4L2 contract: all three stay constant
    // during streaming.
    let (cached_color, v4l2_bytes_per_line, v4l2_size_image) =
        query_the_v4l2_color_and_buffer_layout(device_fd, capture_format, width, height);

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
        set_up_the_camera_gpu_resources(
            full,
            device_fd,
            &camera_name,
            &processor_id_of_the_capturing_node,
            src_pixel_format,
            width,
            height,
            input_alloc_size,
            is_virtual_device,
        )
    });

    let CameraGpuResources {
        mut conversion_stage,
        input_storage_buffers,
        input_mapped_ptrs,
        use_dmabuf,
        dmabuf_imported_buffers,
        vulkan_device_name,
        vulkan_device_is_software,
        buffer_path_to_the_gpu,
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

    log_the_v4l2_capture_buffer_path_to_the_gpu(
        &camera_name,
        buffer_path_to_the_gpu,
        &vulkan_device_name,
        vulkan_device_is_software,
        use_dmabuf,
    );

    // DMA-BUF path drives DQBUF/QBUF per frame directly, so it QBUFs the
    // initial set + STREAMONs manually (the mmap stream does this internally
    // on first `next()`, which the MMAP path relies on).
    if use_dmabuf
        && let Err(streamon_error) =
            queue_the_initial_dma_buf_buffers_and_stream_on(device_fd, &camera_name)
    {
        record_that_capture_ended_on_its_own(format!("VIDIOC_STREAMON failed: {streamon_error}"));
        return;
    }

    run_the_v4l2_dequeue_loop(
        &mut stream,
        V4l2CaptureDequeueLoopInputs {
            device_fd,
            is_capturing: &is_capturing,
            hand_off: &hand_off,
            camera_name: &camera_name,
            width,
            height,
            fourcc,
            capture_instant_resolver: &capture_instant_resolver,
            conversion_stage: &mut conversion_stage,
            input_storage_buffers: &input_storage_buffers,
            input_mapped_ptrs,
            use_dmabuf,
            dmabuf_imported_buffers: &dmabuf_imported_buffers,
            input_byte_size,
            src_layout,
            resolved_color: &resolved_color,
            cached_color,
        },
        &record_that_capture_ended_on_its_own,
    );

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

/// Whether the capture driver is virtual (vivid, v4l2loopback, a platform
/// device) and keeps its buffers in CPU system memory.
fn query_whether_the_v4l2_capture_device_is_virtual(device_fd: RawFd) -> bool {
    unsafe {
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
    }
}

/// The device's colour, `bytesperline` and `sizeimage`, or no colour and
/// tight-packed sizes when the driver will not say.
fn query_the_v4l2_color_and_buffer_layout(
    device_fd: RawFd,
    capture_format: CaptureFormat,
    width: u32,
    height: u32,
) -> (H273ColorVui, u32, u32) {
    unsafe {
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
                // `capture_format` is never anything else). `__bindgen_anon_1` is
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
    }
}

/// Queue every driver buffer and turn streaming on, for the DMA-BUF path,
/// which drives DQBUF/QBUF itself. A failed STREAMON is returned; a failed
/// initial QBUF is logged and streaming starts with the buffers that queued.
fn queue_the_initial_dma_buf_buffers_and_stream_on(
    device_fd: RawFd,
    camera_name: &str,
) -> std::io::Result<()> {
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
            return Err(streamon_error);
        }
    }
    Ok(())
}
