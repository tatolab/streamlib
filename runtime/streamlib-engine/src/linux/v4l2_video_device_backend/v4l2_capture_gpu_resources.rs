// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The GPU resources a V4L2 capture converts on: the conversion stage, the
//! input storage buffers, and the DMA-BUF imports of the driver's buffers
//! when the path to the GPU allows them.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use crate::core::Result;
use crate::core::context::captured_video_frame_to_pooled_rgba_conversion_stage::CapturedVideoFrameToPooledRgbaConversionStage;
use crate::core::context::{
    GpuCapabilitiesSnapshot, GpuContextFullAccess, OneOffAcquisitionCountedForItsNode,
};
use crate::core::rhi::{PixelFormat, StorageBuffer};

use super::V4L2_BUFFER_COUNT;

pub(super) struct CameraGpuResources {
    pub(super) conversion_stage: CapturedVideoFrameToPooledRgbaConversionStage,
    pub(super) input_storage_buffers: Vec<OneOffAcquisitionCountedForItsNode<StorageBuffer>>,
    pub(super) input_mapped_ptrs: [*mut u8; 2],
    pub(super) use_dmabuf: bool,
    pub(super) dmabuf_imported_buffers: Vec<StorageBuffer>,
    pub(super) vulkan_device_name: String,
    pub(super) vulkan_device_is_software: bool,
    pub(super) buffer_path_to_the_gpu: V4l2CaptureBufferPathToTheGpu,
}

/// How a V4L2 capture's buffers reach the GPU converter, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum V4l2CaptureBufferPathToTheGpu {
    /// Export each driver buffer with `VIDIOC_EXPBUF` and try importing it as
    /// a DMA-BUF; a refused export or import still falls back to MMAP.
    ProbeTheDmaBufImportOfEveryExportedDriverBuffer,
    /// MMAP + memcpy: the Vulkan device has no external-memory extensions.
    MmapBecauseTheVulkanDeviceHasNoExternalMemory,
    /// MMAP + memcpy: the Vulkan device exports no DMA-BUF — never on a
    /// software device, whatever its extensions say.
    MmapBecauseTheVulkanDeviceExportsNoDmaBuf,
    /// MMAP + memcpy: the capture driver is virtual and keeps its buffers in
    /// system memory, where an import can succeed and read incoherent bytes.
    MmapBecauseTheCaptureDeviceIsVirtual,
    /// MMAP + memcpy: the Vulkan driver is blocklisted for cross-device
    /// DMA-BUF imports.
    MmapBecauseTheVulkanDriverIsBlocklistedForCrossDeviceImports,
}

/// Choose how a V4L2 capture's buffers reach the GPU converter.
pub(super) fn choose_v4l2_capture_buffer_path_to_the_gpu(
    capabilities_of_the_vulkan_device: &GpuCapabilitiesSnapshot,
    capture_device_is_virtual: bool,
) -> V4l2CaptureBufferPathToTheGpu {
    if !capabilities_of_the_vulkan_device.supports_external_memory {
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDeviceHasNoExternalMemory
    } else if !capabilities_of_the_vulkan_device.supports_dma_buf_export {
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDeviceExportsNoDmaBuf
    } else if capture_device_is_virtual {
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheCaptureDeviceIsVirtual
    } else if !capabilities_of_the_vulkan_device.supports_cross_device_dma_buf_probe {
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDriverIsBlocklistedForCrossDeviceImports
    } else {
        V4l2CaptureBufferPathToTheGpu::ProbeTheDmaBufImportOfEveryExportedDriverBuffer
    }
}

/// Create the conversion stage and the input storage buffers, and import
/// every driver buffer as a DMA-BUF when the path to the GPU allows it.
#[allow(clippy::too_many_arguments)]
pub(super) fn set_up_the_camera_gpu_resources(
    full: &GpuContextFullAccess,
    device_fd: RawFd,
    camera_name: &str,
    processor_id_of_the_capturing_node: &str,
    src_pixel_format: PixelFormat,
    width: u32,
    height: u32,
    input_alloc_size: u64,
    is_virtual_device: bool,
) -> Result<CameraGpuResources> {
    let caps = full.gpu_capabilities()?;
    let vulkan_device_name = caps.device_name.clone();

    let conversion_stage = CapturedVideoFrameToPooledRgbaConversionStage::create(
        full,
        src_pixel_format,
        width,
        height,
        processor_id_of_the_capturing_node,
    )?;

    // Double-buffered HOST_VISIBLE input SSBOs (MMAP+memcpy fallback path).
    let mut input_storage_buffers = Vec::with_capacity(2);
    let mut input_mapped_ptrs: [*mut u8; 2] = [std::ptr::null_mut(); 2];
    for slot in &mut input_mapped_ptrs {
        let buf = full.acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(input_alloc_size)?,
        )?;
        *slot = buf.mapped_ptr();
        input_storage_buffers.push(buf);
    }

    // DMA-BUF probe — VIDIOC_EXPBUF on each V4L2 buffer + Vulkan import.
    // The import side is privileged (allocates VkDeviceMemory + binds) so
    // it stays inside the escalation; failure falls through to MMAP.
    let buffer_path_to_the_gpu =
        choose_v4l2_capture_buffer_path_to_the_gpu(&caps, is_virtual_device);
    let mut use_dmabuf = false;
    let mut dmabuf_imported_buffers: Vec<StorageBuffer> = Vec::new();
    if buffer_path_to_the_gpu
        == V4l2CaptureBufferPathToTheGpu::ProbeTheDmaBufImportOfEveryExportedDriverBuffer
    {
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
        vulkan_device_is_software: caps.device_is_software,
        buffer_path_to_the_gpu,
    })
}

/// Log which path the capture's buffers take to the GPU, and why.
pub(super) fn log_the_v4l2_capture_buffer_path_to_the_gpu(
    camera_name: &str,
    buffer_path_to_the_gpu: V4l2CaptureBufferPathToTheGpu,
    vulkan_device_name: &str,
    vulkan_device_is_software: bool,
    use_dmabuf: bool,
) {
    match buffer_path_to_the_gpu {
        V4l2CaptureBufferPathToTheGpu::ProbeTheDmaBufImportOfEveryExportedDriverBuffer => {}
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDeviceHasNoExternalMemory => {
            tracing::info!(
                camera = camera_name,
                device = %vulkan_device_name,
                "the Vulkan device {vulkan_device_name} has no external-memory extensions, so \
                 the capture's buffers cannot be imported — using MMAP + memcpy"
            );
        }
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDeviceExportsNoDmaBuf => {
            tracing::info!(
                camera = camera_name,
                device = %vulkan_device_name,
                device_is_software = vulkan_device_is_software,
                "the Vulkan device {vulkan_device_name} exports no DMA-BUF, so the capture's \
                 buffers cannot be imported — using MMAP + memcpy"
            );
        }
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheCaptureDeviceIsVirtual => {
            tracing::info!(
                camera = camera_name,
                device = %vulkan_device_name,
                "the capture driver is virtual and keeps its buffers in system memory, where \
                 an import can read incoherent bytes — using MMAP + memcpy"
            );
        }
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDriverIsBlocklistedForCrossDeviceImports => {
            tracing::info!(
                camera = camera_name,
                device = %vulkan_device_name,
                "DMA-BUF probe skipped — driver blocklisted for cross-device imports. \
                 Using MMAP + memcpy."
            );
        }
    }
    if use_dmabuf {
        tracing::info!(
            camera = camera_name,
            buffers_imported = V4L2_BUFFER_COUNT,
            "DMA-BUF zero-copy enabled",
        );
    }
}
