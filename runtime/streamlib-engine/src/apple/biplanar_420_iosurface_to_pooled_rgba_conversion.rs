// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Biplanar 4:2:0 frames on IOSurfaces — a camera's, a hardware decoder's —
//! landed in pooled `Rgba32` pixel buffers through the capture conversion
//! stage.
//!
//! Each IOSurface's memory is imported as a storage buffer, zero-copy, and
//! kept for the next time its producer recycles the surface; from the first
//! import the driver refuses, the planes are copied into a staging buffer
//! instead — no dial.

use std::collections::HashMap;

use objc2_io_surface::{IOSurfaceID, IOSurfaceLockOptions, IOSurfaceRef};

use crate::core::color::ResolvedColorInfo;
use crate::core::context::captured_video_frame_to_pooled_rgba_conversion_stage::{
    CapturedVideoFrameBytesInAStorageBuffer, CapturedVideoFrameToPooledRgbaConversionStage,
};
use crate::core::context::{GpuContextFullAccess, GpuContextLimitedAccess};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, PublishedPixelBufferFrameId, SourceLayoutInfo, StorageBuffer,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer;

/// How many distinct IOSurfaces one conversion keeps imported. A producer
/// recycles a handful from its own pool, so one past this is churning
/// surfaces and starts over rather than growing.
const MOST_IMPORTED_IOSURFACES_KEPT: usize = 16;

/// How a stream's IOSurfaces reach the GPU.
enum Biplanar420IOSurfaceTransport {
    /// Each IOSurface's memory imported as a storage buffer, kept for the
    /// next time the producer recycles the surface.
    ImportedIOSurfaceStorageBuffers {
        imported_by_iosurface_id: HashMap<IOSurfaceID, ImportedIOSurfaceStorageBuffer>,
    },
    /// Each frame's planes copied into one host-visible storage buffer.
    CopiedIntoAStorageBuffer {
        staging: CpuUploadStagingStorageBuffer,
    },
}

/// One stream's conversion of biplanar 4:2:0 IOSurfaces into pooled `Rgba32`
/// pixel buffers. Owned by the one thread that converts.
pub(crate) struct Biplanar420IOSurfaceToPooledRgbaConversion {
    conversion_stage: CapturedVideoFrameToPooledRgbaConversionStage,
    frame_transport: Biplanar420IOSurfaceTransport,
    /// What the frames come from — a camera's name, a decoder's stream — for
    /// the log lines.
    source_description: String,
}

impl Biplanar420IOSurfaceToPooledRgbaConversion {
    /// A conversion for `width` × `height` frames whose surfaces are in
    /// `source_pixel_format`; pooled frames come out at that extent, so a
    /// larger surface is read from its top-left corner.
    pub(crate) fn create(
        gpu_context: &GpuContextLimitedAccess,
        source_description: &str,
        source_pixel_format: PixelFormat,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let conversion_stage = gpu_context.escalate(|full| {
            CapturedVideoFrameToPooledRgbaConversionStage::create(
                full,
                source_pixel_format,
                width,
                height,
            )
        })?;
        Ok(Self {
            conversion_stage,
            frame_transport: Biplanar420IOSurfaceTransport::ImportedIOSurfaceStorageBuffers {
                imported_by_iosurface_id: HashMap::new(),
            },
            source_description: source_description.to_string(),
        })
    }

    /// How this stream's surfaces reach the GPU, for a log line.
    pub(crate) fn describe_transport(&self) -> &'static str {
        match self.frame_transport {
            Biplanar420IOSurfaceTransport::ImportedIOSurfaceStorageBuffers { .. } => {
                "IOSurface zero-copy"
            }
            Biplanar420IOSurfaceTransport::CopiedIntoAStorageBuffer { .. } => "CPU upload",
        }
    }

    /// Convert the frame on `iosurface` into a freshly acquired pooled pixel
    /// buffer, and return once that buffer is host-readable — by when the
    /// surface's pixels are read, so its producer may recycle it.
    pub(crate) fn convert_into_pooled_pixel_buffer(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        iosurface: &IOSurfaceRef,
        color: &ResolvedColorInfo,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        self.take_this_surface_into_the_transport(gpu_context, iosurface)?;
        let device_bytes = match &self.frame_transport {
            Biplanar420IOSurfaceTransport::ImportedIOSurfaceStorageBuffers {
                imported_by_iosurface_id,
            } => {
                let imported = imported_by_iosurface_id
                    .get(&iosurface.id())
                    .ok_or_else(|| {
                        Error::Runtime("the frame's IOSurface was not imported".into())
                    })?;
                CapturedVideoFrameBytesInAStorageBuffer {
                    storage_buffer: imported.storage_buffer(),
                    layout: imported.nv12_source_layout()?,
                    written_by_another_device: true,
                }
            }
            Biplanar420IOSurfaceTransport::CopiedIntoAStorageBuffer { staging } => {
                staging.copy_the_planes_of(iosurface)?;
                CapturedVideoFrameBytesInAStorageBuffer {
                    storage_buffer: &staging.storage_buffer,
                    layout: staging.layout,
                    written_by_another_device: false,
                }
            }
        };
        self.conversion_stage
            .convert_into_pooled_pixel_buffer(gpu_context, device_bytes, color)
    }

    /// Make sure the transport can take `iosurface`: import it on the first
    /// frame it carries, and from the first import the driver refuses, switch
    /// the stream to CPU upload staged for this surface's shape.
    fn take_this_surface_into_the_transport(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        iosurface: &IOSurfaceRef,
    ) -> Result<()> {
        let Biplanar420IOSurfaceTransport::ImportedIOSurfaceStorageBuffers {
            imported_by_iosurface_id,
        } = &mut self.frame_transport
        else {
            return Ok(());
        };
        if imported_by_iosurface_id.contains_key(&iosurface.id()) {
            return Ok(());
        }
        match gpu_context.escalate(|full| full.import_iosurface_as_storage_buffer(iosurface)) {
            Ok(imported) => {
                if imported_by_iosurface_id.len() >= MOST_IMPORTED_IOSURFACES_KEPT {
                    self.conversion_stage.wait_for_the_previous_submission()?;
                    imported_by_iosurface_id.clear();
                }
                imported_by_iosurface_id.insert(iosurface.id(), imported);
                tracing::debug!(
                    source = %self.source_description,
                    iosurface_id = iosurface.id(),
                    imported_iosurfaces = imported_by_iosurface_id.len(),
                    "imported one more of the source's recycled IOSurfaces"
                );
            }
            Err(import_refusal) => {
                tracing::warn!(
                    source = %self.source_description,
                    error = %import_refusal,
                    "the GPU cannot import the source's IOSurfaces; copying each frame through \
                     the CPU instead"
                );
                self.conversion_stage.wait_for_the_previous_submission()?;
                self.frame_transport = Biplanar420IOSurfaceTransport::CopiedIntoAStorageBuffer {
                    staging: gpu_context.escalate(|full| {
                        CpuUploadStagingStorageBuffer::shaped_for(full, iosurface)
                    })?,
                };
            }
        }
        Ok(())
    }
}

/// The storage buffer a CPU upload lands both planes in, luma then chroma,
/// and the plane geometry it was shaped for.
struct CpuUploadStagingStorageBuffer {
    storage_buffer: StorageBuffer,
    layout: SourceLayoutInfo,
    luma_bytes_per_row: usize,
    luma_height: usize,
    chroma_bytes_per_row: usize,
    chroma_height: usize,
}

impl CpuUploadStagingStorageBuffer {
    /// Staging shaped for `iosurface`'s two planes at their own strides.
    fn shaped_for(full: &GpuContextFullAccess, iosurface: &IOSurfaceRef) -> Result<Self> {
        if iosurface.plane_count() != 2 {
            return Err(Error::Runtime(format!(
                "the source's IOSurface has {} planes where biplanar 4:2:0 has 2",
                iosurface.plane_count()
            )));
        }
        let (luma_bytes_per_row, luma_height) = (
            iosurface.bytes_per_row_of_plane(0),
            iosurface.height_of_plane(0),
        );
        let (chroma_bytes_per_row, chroma_height) = (
            iosurface.bytes_per_row_of_plane(1),
            iosurface.height_of_plane(1),
        );
        let luma_plane_bytes = luma_bytes_per_row * luma_height;
        let as_u32 = |value: usize| {
            u32::try_from(value).map_err(|_| {
                Error::Runtime(format!(
                    "the source's IOSurface geometry {value} does not fit a u32"
                ))
            })
        };
        let layout = SourceLayoutInfo::nv12(
            as_u32(luma_bytes_per_row)?,
            as_u32(chroma_bytes_per_row)?,
            as_u32(luma_plane_bytes)?,
        );
        let byte_size =
            (luma_plane_bytes + chroma_bytes_per_row * chroma_height).next_multiple_of(4) as u64;
        Ok(Self {
            storage_buffer: full.acquire_storage_buffer(byte_size)?,
            layout,
            luma_bytes_per_row,
            luma_height,
            chroma_bytes_per_row,
            chroma_height,
        })
    }

    /// Copy both of `iosurface`'s planes in, under a read-only lock so the copy
    /// sees a coherent frame, refusing a surface of any other shape.
    fn copy_the_planes_of(&self, iosurface: &IOSurfaceRef) -> Result<()> {
        let surface_geometry = (
            iosurface.plane_count(),
            iosurface.bytes_per_row_of_plane(0),
            iosurface.height_of_plane(0),
            iosurface.bytes_per_row_of_plane(1),
            iosurface.height_of_plane(1),
        );
        let staged_geometry = (
            2,
            self.luma_bytes_per_row,
            self.luma_height,
            self.chroma_bytes_per_row,
            self.chroma_height,
        );
        if surface_geometry != staged_geometry {
            return Err(Error::Runtime(format!(
                "the source's IOSurface changed shape (planes, rows and strides \
                 {surface_geometry:?}, staged for {staged_geometry:?})"
            )));
        }
        let luma_plane_bytes = self.luma_bytes_per_row * self.luma_height;
        let chroma_plane_bytes = self.chroma_bytes_per_row * self.chroma_height;
        let destination = self.storage_buffer.mapped_ptr();
        // SAFETY: the surface is locked for reading around the copies; its
        // geometry was just checked to be the one the staging buffer was sized
        // for, so each plane is `bytes_per_row × height` bytes at its base
        // address and lands inside the buffer, luma then chroma.
        unsafe {
            let locked = iosurface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
            if locked != 0 {
                return Err(Error::Runtime(format!(
                    "locking the source's IOSurface for reading failed ({locked})"
                )));
            }
            std::ptr::copy_nonoverlapping(
                iosurface.base_address_of_plane(0).as_ptr().cast::<u8>(),
                destination,
                luma_plane_bytes,
            );
            std::ptr::copy_nonoverlapping(
                iosurface.base_address_of_plane(1).as_ptr().cast::<u8>(),
                destination.add(luma_plane_bytes),
                chroma_plane_bytes,
            );
            iosurface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
        }
        Ok(())
    }
}
