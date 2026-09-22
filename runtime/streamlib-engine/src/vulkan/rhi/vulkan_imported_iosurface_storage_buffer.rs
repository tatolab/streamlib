// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An IOSurface's memory imported as a storage buffer, zero-copy, through
//! `VK_EXT_external_memory_host`.

use std::sync::Arc;

use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;

use crate::core::rhi::{PixelFormat, SourceLayoutInfo, StorageBuffer};
use crate::core::{Error, Result};

use super::{HostVulkanBuffer, HostVulkanDevice};
use crate::apple::iosurface::{
    RetainedIOSurfaceSharedAcrossThreads, create_iosurface_mach_send_right,
};

/// An IOSurface's memory imported as a storage buffer, zero-copy.
///
/// The buffer aliases the surface's pages: what the producer writes, a
/// kernel reading the buffer sees, with no copy in between. The buffer
/// itself keeps the surface alive — this value and every clone of its
/// [`StorageBuffer`] hold it — so the pages outlive the memory that aliases
/// them. As with any buffer, the last clone must outlive every submission
/// that reads it.
pub struct ImportedIOSurfaceStorageBuffer {
    storage_buffer: StorageBuffer,
    iosurface: RetainedIOSurfaceSharedAcrossThreads,
    imported_base_address: usize,
}

impl ImportedIOSurfaceStorageBuffer {
    /// Import `iosurface`'s whole allocation — its base address for its
    /// allocation size rounded up to the device's host-pointer import
    /// alignment — with the buffer retaining the surface.
    ///
    /// Refused, naming the reason, when `VK_EXT_external_memory_host` is not
    /// enabled, when the surface's base address is not on the import
    /// alignment, or when the driver declines the import.
    #[tracing::instrument(level = "debug", skip(vulkan_device, iosurface), fields(
        surface = %describe_iosurface(iosurface),
    ))]
    pub(crate) fn import(
        vulkan_device: &Arc<HostVulkanDevice>,
        iosurface: &IOSurfaceRef,
    ) -> Result<Self> {
        let buffer = HostVulkanBuffer::from_iosurface_pages(vulkan_device, iosurface, None)?;
        Ok(Self {
            storage_buffer: StorageBuffer::from_host_vulkan_buffer(Arc::new(buffer)),
            iosurface: RetainedIOSurfaceSharedAcrossThreads::new(CFRetained::from(iosurface)),
            imported_base_address: iosurface.base_address().as_ptr() as usize,
        })
    }

    /// The imported memory as a storage buffer, from the surface's base
    /// address.
    pub fn storage_buffer(&self) -> &StorageBuffer {
        &self.storage_buffer
    }

    /// Where the surface's NV12 planes sit in [`Self::storage_buffer`], at
    /// the surface's own strides, for the NV12 buffer kernel.
    ///
    /// Refused, naming the surface, unless it is biplanar 4:2:0 8-bit
    /// (`420v` / `420f`) with exactly two planes, or when an offset or
    /// stride does not fit the layout's `u32`s.
    pub fn nv12_source_layout(&self) -> Result<SourceLayoutInfo> {
        const OPERATION: &str = "ImportedIOSurfaceStorageBuffer::nv12_source_layout";
        let iosurface: &IOSurfaceRef = &self.iosurface;
        let plane_count = iosurface.plane_count();
        if plane_count != 2
            || !matches!(
                PixelFormat::from_cv_pixel_format_type(iosurface.pixel_format()),
                PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange
            )
        {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: the {} has {plane_count} plane(s) and is not biplanar 4:2:0 \
                 8-bit NV12 ('420v' / '420f' with 2 planes)",
                describe_iosurface(iosurface)
            )));
        }
        let offset_of_plane = |plane_index: usize| {
            (iosurface.base_address_of_plane(plane_index).as_ptr() as usize)
                .checked_sub(self.imported_base_address)
        };
        let fits_in_u32 = |what: &str, value: Option<usize>| {
            value
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    Error::Configuration(format!(
                        "{OPERATION}: the {}'s {what} ({value:?}) does not fit the source \
                         layout's u32",
                        describe_iosurface(iosurface)
                    ))
                })
        };
        let luma_offset = offset_of_plane(0);
        let chroma_offset_from_luma = offset_of_plane(1)
            .zip(luma_offset)
            .and_then(|(chroma, luma)| chroma.checked_sub(luma));
        Ok(SourceLayoutInfo::nv12_starting_at(
            fits_in_u32("luma plane offset", luma_offset)?,
            fits_in_u32("luma stride", Some(iosurface.bytes_per_row_of_plane(0)))?,
            fits_in_u32("chroma stride", Some(iosurface.bytes_per_row_of_plane(1)))?,
            fits_in_u32("chroma offset from the luma plane", chroma_offset_from_luma)?,
        ))
    }
}

impl HostVulkanBuffer {
    /// Import `iosurface`'s pages as a buffer through
    /// `VK_EXT_external_memory_host`, zero-copy: its base address for its
    /// allocation size rounded up to the device's import alignment. The
    /// buffer spans `buffer_byte_len` bytes of that, or all of it for
    /// `None`, and retains the surface until its memory is freed.
    ///
    /// Refused, naming the reason, when the extension is not enabled, when
    /// the surface's base address is not on the import alignment, or when
    /// the driver declines the import.
    pub fn from_iosurface_pages(
        vulkan_device: &Arc<HostVulkanDevice>,
        iosurface: &IOSurfaceRef,
        buffer_byte_len: Option<u64>,
    ) -> Result<Self> {
        const OPERATION: &str = "import_iosurface_pages";
        let surface_description = describe_iosurface(iosurface);
        if !vulkan_device.supports_host_pointer_import() {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: the {surface_description} cannot be imported — \
                 VK_EXT_external_memory_host is not enabled on this device"
            )));
        }
        let import_alignment = vulkan_device.min_imported_host_pointer_alignment();
        let base_address = iosurface.base_address().as_ptr().cast::<u8>();
        let allocation_byte_size = iosurface.alloc_size() as u64;
        if allocation_byte_size == 0 {
            return Err(Error::Configuration(format!(
                "{OPERATION}: the {surface_description} has no allocation to import"
            )));
        }
        if import_alignment == 0 || !(base_address as u64).is_multiple_of(import_alignment) {
            return Err(Error::NotSupported(format!(
                "{OPERATION}: the {surface_description}'s base address {base_address:p} is not \
                 on the driver's {import_alignment}-byte host-pointer import alignment"
            )));
        }
        let imported_byte_size = allocation_byte_size.next_multiple_of(import_alignment);

        let buffer = HostVulkanBuffer::from_imported_host_range_as_buffer_of_size(
            vulkan_device,
            base_address,
            imported_byte_size,
            buffer_byte_len.unwrap_or(imported_byte_size),
            None,
            "HostVulkanBuffer::from_iosurface_pages",
        )
        .map_err(|refusal| match refusal {
            Error::GpuError(driver_refusal) => Error::NotSupported(format!(
                "{OPERATION}: the driver declined to import the {surface_description}'s \
                 {imported_byte_size} bytes as a storage buffer (MoltenVK before 1.4.1 refuses \
                 host-pointer buffers): {driver_refusal}"
            )),
            other => other,
        })?;
        Ok(
            buffer.backed_by_iosurface(RetainedIOSurfaceSharedAcrossThreads::new(
                CFRetained::from(iosurface),
            )),
        )
    }

    /// A pixel buffer whose memory is a fresh private IOSurface of
    /// `width`x`height` packed rows, so it can cross to a helper process as
    /// a Mach port. The buffer spans exactly the pixels.
    pub fn new_iosurface_backed_pixel_buffer(
        vulkan_device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        pixel_format: PixelFormat,
    ) -> Result<Self> {
        let iosurface = crate::apple::iosurface::create_private_iosurface_with_packed_rows(
            width,
            height,
            bytes_per_pixel,
            pixel_format,
        )?;
        let pixel_byte_len = u64::from(width) * u64::from(height) * u64::from(bytes_per_pixel);
        Self::from_iosurface_pages(vulkan_device, &iosurface, Some(pixel_byte_len))
    }

    /// A fresh send right naming this buffer's IOSurface, for the
    /// surface-share wire. Refused for a buffer that is not IOSurface-backed.
    pub fn export_iosurface_mach_send_right(
        &self,
    ) -> Result<streamlib_surface_client::OwnedMachSendRight> {
        let iosurface = self.backing_iosurface().ok_or_else(|| {
            Error::NotSupported(
                "export_iosurface_mach_send_right: this buffer's memory is not an IOSurface".into(),
            )
        })?;
        create_iosurface_mach_send_right(iosurface)
    }
}

impl std::fmt::Debug for ImportedIOSurfaceStorageBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedIOSurfaceStorageBuffer")
            .field("iosurface", &describe_iosurface(&self.iosurface))
            .field("byte_size", &self.storage_buffer.byte_size())
            .finish()
    }
}

/// `WxH 'FourCC' IOSurface`.
fn describe_iosurface(iosurface: &IOSurfaceRef) -> String {
    format!(
        "{}x{} '{}' IOSurface",
        iosurface.width(),
        iosurface.height(),
        four_character_code(iosurface.pixel_format())
    )
}

fn four_character_code(pixel_format: u32) -> String {
    let bytes = pixel_format.to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        bytes.iter().map(|&byte| char::from(byte)).collect()
    } else {
        format!("{pixel_format:#010x}")
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;

    use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetIOSurface,
        kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_32BGRA,
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVReturnSuccess,
    };
    use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    use crate::core::Error;
    use crate::core::color::{MatrixId, PrimariesId, RangeId, ResolvedColorInfo, TransferId};
    use crate::core::context::{GpuContext, GpuContextFullAccess, GpuContextLimitedAccess};
    use crate::core::rhi::{
        PixelFormat, RhiColorConverter, SourceLayoutInfo, StorageBuffer, Texture,
        TextureDescriptor, TextureFormat, TextureUsages, VulkanLayout,
    };
    use crate::vulkan::rhi::vulkan_color_converter::COLOR_CONVERTER_WORKGROUP_SIZE;
    use crate::vulkan::rhi::{ImageCopyRegion, VulkanAccess, VulkanStage};

    const HOST_READBACK_WAIT_TIMEOUT_NS: u64 = 5_000_000_000;

    /// The first MoltenVK whose `vkCreateBuffer` accepts
    /// `VkExternalMemoryBufferCreateInfo{HOST_ALLOCATION}`.
    const FIRST_MOLTENVK_IMPORTING_HOST_POINTER_BUFFERS: (u32, u32, u32) = (1, 4, 1);

    fn full_access_or_skip() -> Option<GpuContextFullAccess> {
        match GpuContext::init_for_platform() {
            Ok(gpu) => Some(GpuContextLimitedAccess::new(gpu).to_full_access()),
            Err(e) => {
                tracing::warn!("skipping — no GPU device: {e}");
                None
            }
        }
    }

    /// `(major, minor, patch)` of the MoltenVK driving `full`, or `None` for
    /// another driver.
    fn moltenvk_version(full: &GpuContextFullAccess) -> Option<(u32, u32, u32)> {
        let vulkan_device =
            crate::host_rhi::HostGpuDeviceExt::vulkan_device(full.device().as_ref());
        let mut driver_properties = vk::PhysicalDeviceDriverProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut driver_properties)
            .build();
        unsafe {
            vulkan_device
                .instance()
                .get_physical_device_properties2(vulkan_device.physical_device(), &mut properties)
        };
        if driver_properties.driver_id != vk::DriverId::MOLTENVK {
            return None;
        }
        let driver_info = driver_properties.driver_info.to_string_lossy().into_owned();
        let mut components = driver_info
            .split('.')
            .map(|component| component.trim().parse::<u32>().unwrap_or(0));
        Some((
            components.next().unwrap_or(0),
            components.next().unwrap_or(0),
            components.next().unwrap_or(0),
        ))
    }

    /// A `420v` surface the way the camera's arrive: allocated by CoreVideo.
    fn corevideo_420v_iosurface(width: usize, height: usize) -> CFRetained<IOSurfaceRef> {
        corevideo_iosurface(
            width,
            height,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        )
    }

    fn corevideo_iosurface(
        width: usize,
        height: usize,
        pixel_format: u32,
    ) -> CFRetained<IOSurfaceRef> {
        let no_surface_properties = CFDictionary::<CFString, CFType>::empty();
        let no_surface_properties: &CFType = &no_surface_properties;
        let attributes = CFDictionary::<CFString, CFType>::from_slices(
            &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
            &[no_surface_properties],
        );
        let mut pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
        let created = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                pixel_format,
                Some(attributes.as_opaque()),
                NonNull::from(&mut pixel_buffer),
            )
        };
        assert_eq!(created, kCVReturnSuccess, "CVPixelBufferCreate");
        let pixel_buffer = unsafe {
            CFRetained::from_raw(NonNull::new(pixel_buffer).expect("a created pixel buffer"))
        };
        CVPixelBufferGetIOSurface(Some(&pixel_buffer)).expect("an IOSurface-backed pixel buffer")
    }

    /// Write a `seed`-dependent Y / CbCr pattern through the CPU mapping,
    /// with a sentinel in every row's padding.
    fn write_420_pattern_through_the_cpu(surface: &IOSurfaceRef, seed: usize) {
        const PADDING_SENTINEL: u8 = 0xEE;
        let locked = unsafe { surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
        assert_eq!(locked, 0, "IOSurfaceLock");
        for plane_index in 0..2 {
            let row_bytes = surface.bytes_per_row_of_plane(plane_index);
            let rows = surface.height_of_plane(plane_index);
            let written_row_bytes = surface.width_of_plane(plane_index)
                * surface.bytes_per_element_of_plane(plane_index);
            let plane = unsafe {
                std::slice::from_raw_parts_mut(
                    surface
                        .base_address_of_plane(plane_index)
                        .as_ptr()
                        .cast::<u8>(),
                    row_bytes * rows,
                )
            };
            for (y, row) in plane.chunks_exact_mut(row_bytes).enumerate() {
                for (x, byte) in row.iter_mut().enumerate() {
                    *byte = if x >= written_row_bytes {
                        PADDING_SENTINEL
                    } else if plane_index == 0 {
                        (16 + (x * 5 + y * 7 + seed * 31) % 220) as u8
                    } else {
                        (48 + (x * 13 + y * 11 + seed * 17) % 160) as u8
                    };
                }
            }
        }
        let unlocked =
            unsafe { surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
        assert_eq!(unlocked, 0, "IOSurfaceUnlock");
    }

    /// The surface's two planes copied into a plain host-visible storage
    /// buffer, back to back at the surface's strides, and their layout there.
    fn copy_the_planes_into_a_plain_storage_buffer(
        full: &GpuContextFullAccess,
        surface: &IOSurfaceRef,
    ) -> (StorageBuffer, SourceLayoutInfo) {
        let locked = unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        assert_eq!(locked, 0, "IOSurfaceLock");
        let mut packed = Vec::new();
        for plane_index in 0..2 {
            let plane_bytes =
                surface.bytes_per_row_of_plane(plane_index) * surface.height_of_plane(plane_index);
            packed.extend_from_slice(unsafe {
                std::slice::from_raw_parts(
                    surface
                        .base_address_of_plane(plane_index)
                        .as_ptr()
                        .cast::<u8>(),
                    plane_bytes,
                )
            });
        }
        let unlocked =
            unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        assert_eq!(unlocked, 0, "IOSurfaceUnlock");

        let storage_buffer = full
            .acquire_storage_buffer(packed.len().next_multiple_of(4) as u64)
            .expect("plain storage buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(
                packed.as_ptr(),
                storage_buffer.mapped_ptr(),
                packed.len(),
            )
        };
        let luma_row_bytes = surface.bytes_per_row_of_plane(0);
        let layout = SourceLayoutInfo::nv12(
            luma_row_bytes as u32,
            surface.bytes_per_row_of_plane(1) as u32,
            (luma_row_bytes * surface.height_of_plane(0)) as u32,
        );
        (storage_buffer, layout)
    }

    fn rgba_storage_target(full: &GpuContextFullAccess, width: u32, height: u32) -> Texture {
        full.device()
            .create_texture_local(&TextureDescriptor {
                label: Some("imported-iosurface-storage-buffer-test-output"),
                width,
                height,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
            })
            .expect("RGBA storage target")
    }

    /// Convert every `(source, layout)` with the NV12 buffer kernel in one
    /// submission and return each RGBA frame. Each source gets its own
    /// converter: a kernel's one descriptor set cannot be rewritten between
    /// two dispatches of the same recording.
    fn convert_each_nv12_source_to_rgba(
        full: &GpuContextFullAccess,
        sources: &[(&StorageBuffer, SourceLayoutInfo)],
        width: u32,
        height: u32,
    ) -> Vec<Vec<u8>> {
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Bt709,
            matrix: MatrixId::Bt709,
            range: RangeId::Limited,
        };
        let rgba_byte_len = (width * height * 4) as usize;
        let targets: Vec<Texture> = sources
            .iter()
            .map(|_| rgba_storage_target(full, width, height))
            .collect();
        let converters: Vec<RhiColorConverter> = sources
            .iter()
            .map(|_| {
                full.create_color_converter(PixelFormat::Nv12VideoRange, PixelFormat::Rgba32)
                    .expect("NV12 → RGBA converter")
            })
            .collect();
        let readbacks: Vec<StorageBuffer> = sources
            .iter()
            .map(|_| {
                full.acquire_storage_buffer(rgba_byte_len as u64)
                    .expect("readback buffer")
            })
            .collect();
        let mut recorder = full
            .create_command_recorder("imported_iosurface_storage_buffer_test")
            .expect("recorder");
        let timeline = full.create_timeline_semaphore(0).expect("timeline");

        recorder.begin().expect("begin");
        for (((source, layout), converter), (target, readback)) in sources
            .iter()
            .zip(&converters)
            .zip(targets.iter().zip(&readbacks))
        {
            recorder
                .record_image_barrier(
                    target,
                    VulkanLayout::UNDEFINED,
                    VulkanLayout::GENERAL,
                    VulkanStage::NONE,
                    VulkanStage::COMPUTE_SHADER,
                    VulkanAccess::NONE,
                    VulkanAccess::SHADER_WRITE,
                )
                .expect("target → GENERAL");
            let kernel = converter
                .prepare_buffer_to_image_storage(source, *layout, target, &info, TransferId::Srgb)
                .expect("prepare");
            recorder
                .record_dispatch(
                    &kernel,
                    width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                    height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                    1,
                )
                .expect("dispatch");
            recorder
                .record_image_barrier(
                    target,
                    VulkanLayout::GENERAL,
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    VulkanStage::COMPUTE_SHADER,
                    VulkanStage::ALL_TRANSFER,
                    VulkanAccess::SHADER_WRITE,
                    VulkanAccess::TRANSFER_READ,
                )
                .expect("target → TRANSFER_SRC");
            recorder
                .record_copy_image_to_buffer(
                    target,
                    VulkanLayout::TRANSFER_SRC_OPTIMAL,
                    readback,
                    ImageCopyRegion::tightly_packed(width, height),
                )
                .expect("copy to readback");
            recorder
                .record_buffer_barrier(
                    readback,
                    VulkanStage::ALL_TRANSFER,
                    VulkanStage::HOST,
                    VulkanAccess::TRANSFER_WRITE,
                    VulkanAccess::HOST_READ,
                )
                .expect("readback → HOST_READ");
        }
        recorder
            .submit_signaling_timeline(&timeline, 1)
            .expect("submit");
        timeline
            .wait(1, HOST_READBACK_WAIT_TIMEOUT_NS)
            .expect("the conversions complete");

        readbacks
            .iter()
            .map(|readback| {
                unsafe { std::slice::from_raw_parts(readback.mapped_ptr(), rgba_byte_len) }.to_vec()
            })
            .collect()
    }

    fn assert_rgba_frames_identical(imported: &[u8], copied: &[u8], width: u32, label: &str) {
        if let Some(first) = imported.iter().zip(copied).position(|(a, b)| a != b) {
            let pixel = first / 4;
            panic!(
                "[{label}] {} byte(s) differ; first at pixel ({}, {}): imported {:?} vs copied {:?}",
                imported.iter().zip(copied).filter(|(a, b)| a != b).count(),
                pixel % width as usize,
                pixel / width as usize,
                &imported[pixel * 4..pixel * 4 + 4],
                &copied[pixel * 4..pixel * 4 + 4],
            );
        }
        let distinct_colors: std::collections::HashSet<&[u8]> = imported.chunks_exact(4).collect();
        assert!(
            distinct_colors.len() > 64,
            "[{label}] the pattern survives as many colors, got {}",
            distinct_colors.len()
        );
    }

    /// A CoreVideo `420v` surface imported as a storage buffer converts, read
    /// in place from past its header, byte-identically to the same bytes
    /// copied into a plain storage buffer at offset 0 — and after the CPU
    /// rewrites the surface, the next conversion of the same import follows,
    /// which a copy made at import time could not. Needs MoltenVK 1.4.1 or
    /// later; point `VK_DRIVER_FILES` at one to run it on an older system.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_corevideo_420v_surface_imported_as_a_storage_buffer_converts_like_a_copy_and_follows_cpu_writes()
     {
        let Some(full) = full_access_importing_host_pointer_buffers_or_skip() else {
            return;
        };
        for (width, height) in [(1280usize, 720usize), (70, 38)] {
            let label = format!("{width}x{height}");
            let surface = corevideo_420v_iosurface(width, height);
            write_420_pattern_through_the_cpu(&surface, 1);
            let imported = full
                .import_iosurface_as_storage_buffer(&surface)
                .expect("the surface imports as a storage buffer");
            let imported_layout = imported
                .nv12_source_layout()
                .expect("a 420v surface has an NV12 layout");
            assert!(
                imported_layout.plane0_offset_bytes > 0,
                "[{label}] CoreVideo puts a header before plane 0"
            );
            tracing::info!(?imported_layout, "{label} imported");

            let (copied, copied_layout) =
                copy_the_planes_into_a_plain_storage_buffer(&full, &surface);
            let before_rewrite = convert_each_nv12_source_to_rgba(
                &full,
                &[
                    (imported.storage_buffer(), imported_layout),
                    (&copied, copied_layout),
                ],
                width as u32,
                height as u32,
            );
            assert_rgba_frames_identical(
                &before_rewrite[0],
                &before_rewrite[1],
                width as u32,
                &label,
            );

            write_420_pattern_through_the_cpu(&surface, 2);
            let (copied, copied_layout) =
                copy_the_planes_into_a_plain_storage_buffer(&full, &surface);
            let after_rewrite = convert_each_nv12_source_to_rgba(
                &full,
                &[
                    (imported.storage_buffer(), imported_layout),
                    (&copied, copied_layout),
                ],
                width as u32,
                height as u32,
            );
            assert_rgba_frames_identical(
                &after_rewrite[0],
                &after_rewrite[1],
                width as u32,
                &format!("{label} after the CPU rewrite"),
            );
            assert_ne!(
                before_rewrite[0], after_rewrite[0],
                "[{label}] the imported buffer follows the surface's CPU writes"
            );
        }
    }

    /// Before 1.4.1, MoltenVK refuses the spec's host-pointer buffer, and the
    /// refusal is an error that names why — never a panic.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn moltenvk_before_1_4_1_refuses_the_iosurface_storage_buffer_import_as_an_error() {
        let Some(full) = full_access_or_skip() else {
            return;
        };
        let moltenvk = moltenvk_version(&full);
        if moltenvk.is_none_or(|version| version >= FIRST_MOLTENVK_IMPORTING_HOST_POINTER_BUFFERS) {
            tracing::warn!(
                ?moltenvk,
                "skipping — this driver imports host-pointer buffers"
            );
            return;
        }
        let surface = corevideo_420v_iosurface(1280, 720);
        let refusal = full
            .import_iosurface_as_storage_buffer(&surface)
            .expect_err("this MoltenVK refuses host-pointer buffers");
        assert!(
            matches!(refusal, Error::NotSupported(_)),
            "typed refusal: {refusal}"
        );
        assert!(
            refusal.to_string().contains("driver declined"),
            "the refusal names why: {refusal}"
        );
    }

    fn full_access_importing_host_pointer_buffers_or_skip() -> Option<GpuContextFullAccess> {
        let full = full_access_or_skip()?;
        let moltenvk = moltenvk_version(&full);
        if moltenvk.is_some_and(|version| version < FIRST_MOLTENVK_IMPORTING_HOST_POINTER_BUFFERS) {
            tracing::warn!(
                ?moltenvk,
                "skipping — this MoltenVK refuses host-pointer buffers; set VK_DRIVER_FILES to a \
                 1.4.1+ MoltenVK ICD to run it"
            );
            return None;
        }
        Some(full)
    }

    /// The buffer holds the surface whose pages it aliases: a clone of the
    /// storage buffer keeps the surface retained after the import value is
    /// gone, and the last clone releases it.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_storage_buffer_clone_keeps_the_imported_surface_alive_until_it_drops() {
        let Some(full) = full_access_importing_host_pointer_buffers_or_skip() else {
            return;
        };
        let surface = corevideo_420v_iosurface(1280, 720);
        let retains_before_import = surface.retain_count();
        let imported = full
            .import_iosurface_as_storage_buffer(&surface)
            .expect("the surface imports as a storage buffer");
        let storage_buffer_clone = imported.storage_buffer().clone();
        drop(imported);
        assert!(
            surface.retain_count() > retains_before_import,
            "a live clone of the storage buffer keeps the surface retained"
        );
        drop(storage_buffer_clone);
        assert_eq!(
            surface.retain_count(),
            retains_before_import,
            "the last clone releases the surface"
        );
    }

    /// A surface that is not biplanar 4:2:0 8-bit has no NV12 layout, and the
    /// refusal names the surface rather than panicking on a missing plane.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_non_planar_surface_has_no_nv12_layout_and_the_refusal_names_it() {
        let Some(full) = full_access_importing_host_pointer_buffers_or_skip() else {
            return;
        };
        let bgra_surface = corevideo_iosurface(64, 32, kCVPixelFormatType_32BGRA);
        let imported = full
            .import_iosurface_as_storage_buffer(&bgra_surface)
            .expect("any surface imports as a storage buffer");
        let refusal = imported
            .nv12_source_layout()
            .expect_err("a BGRA surface has no NV12 layout");
        assert!(
            matches!(refusal, Error::NotSupported(_)),
            "typed refusal: {refusal}"
        );
        assert!(
            refusal.to_string().contains("64x32 'BGRA' IOSurface"),
            "the refusal names the surface: {refusal}"
        );
    }
}
