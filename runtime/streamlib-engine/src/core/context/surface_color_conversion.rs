// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's colour converter run on surfaces named by id: a YCbCr pixel
//! buffer — a camera's imported DMA-BUF, a decoder's output — converted into
//! an RGBA texture a stream publishes.

use std::collections::HashMap;

use crate::core::color::{ColorSpaceKind, H273ColorVui};
use crate::core::context::{GpuContext, TextureRegistration};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, PixelPlanesWithinOneAllocation, RhiColorConverter, TextureFormat,
    VulkanLayout,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    COLOR_CONVERTER_WORKGROUP_SIZE, RhiCommandRecorder, VulkanAccess, VulkanStage,
};

/// Where a conversion source's pixels reach the GPU from.
enum SurfaceColorConversionSourcePixels {
    /// The allocation itself, imported into the device.
    ImportedPixelBuffer(PixelBuffer),
    /// A foreign device's DMA-BUF the device cannot import, mapped into this
    /// process and read by the GPU in place or through one staged copy.
    #[cfg(target_os = "linux")]
    ForeignDmaBufReadThroughAHostMapping {
        host_mapping_read_by_gpu: crate::vulkan::rhi::HostMappingReadByGpu,
        dma_buf_mapping: crate::linux::dma_buf_mapped_for_cpu_reads::DmaBufMappedForCpuReads,
    },
}

/// A conversion source resolved once and read on every later conversion: its
/// pixels, what its registration says they are, and the layout the converter
/// reads them through.
struct SurfaceColorConversionSource {
    pixels: SurfaceColorConversionSourcePixels,
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    planes: PixelPlanesWithinOneAllocation,
}

/// One caller's conversions: the surface-share sources it has named, a
/// converter per source format, and the recorder they dispatch on.
///
/// Owned by one helper's escalate registry, so the converters' staged bindings
/// are never shared with another node's conversions, and what a helper
/// resolved goes away with it.
#[derive(Default)]
pub(crate) struct SurfaceColorConversionSession {
    sources_by_surface_id: HashMap<String, SurfaceColorConversionSource>,
    color_converters_by_source_format: HashMap<PixelFormat, RhiColorConverter>,
    conversion_recorder: Option<RhiCommandRecorder>,
}

impl SurfaceColorConversionSession {
    /// Convert `source_surface_id`'s YCbCr pixels, described by
    /// `source_color`, into the RGBA texture `destination_surface_id`, keeping
    /// the source's transfer curve, and return once the destination's next
    /// reader sees them — with the registration whose layout the conversion
    /// settled, under the id it resolved under, for the caller to publish.
    pub(crate) fn convert_surface_into(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        source_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        gpu.refuse_a_retired_frame_id(source_surface_id)?;
        let mut pooled_frame_source_of_this_conversion;
        let source = match pooled_frame_held_in_this_process(gpu, source_surface_id)? {
            Some(pooled_frame_source) => {
                pooled_frame_source_of_this_conversion = pooled_frame_source;
                &mut pooled_frame_source_of_this_conversion
            }
            None => {
                if !self.sources_by_surface_id.contains_key(source_surface_id) {
                    let resolved = resolve_color_conversion_source_through_the_surface_store(
                        gpu,
                        source_surface_id,
                    )?;
                    self.sources_by_surface_id
                        .insert(source_surface_id.to_string(), resolved);
                }
                self.sources_by_surface_id
                    .get_mut(source_surface_id)
                    .ok_or_else(|| {
                        Error::GpuError(format!(
                            "surface {source_surface_id} was resolved and is gone"
                        ))
                    })?
            }
        };
        let destination = resolve_color_conversion_destination(gpu, destination_surface_id)?;
        let destination_texture = destination.texture();
        if (destination_texture.width(), destination_texture.height())
            != (source.width, source.height)
        {
            return Err(Error::GpuError(format!(
                "surface {source_surface_id} is {}x{} and destination {destination_surface_id} \
                 is {}x{}; the conversion scales nothing",
                source.width,
                source.height,
                destination_texture.width(),
                destination_texture.height()
            )));
        }

        let source_layout = source
            .planes
            .color_converter_source_layout(source.pixel_format)?;
        let resolved_color = source_color.resolve_defaults(ColorSpaceKind::Yuv);
        let color_converter = match self
            .color_converters_by_source_format
            .get(&source.pixel_format)
        {
            Some(color_converter) => color_converter,
            None => self
                .color_converters_by_source_format
                .entry(source.pixel_format)
                .or_insert(gpu.create_color_converter(source.pixel_format, PixelFormat::Rgba32)?),
        };
        let kernel = match &source.pixels {
            SurfaceColorConversionSourcePixels::ImportedPixelBuffer(pixel_buffer) => {
                color_converter.prepare_buffer_to_image_pixel(
                    pixel_buffer,
                    source_layout,
                    destination_texture,
                    &resolved_color,
                    resolved_color.transfer,
                )?
            }
            #[cfg(target_os = "linux")]
            SurfaceColorConversionSourcePixels::ForeignDmaBufReadThroughAHostMapping {
                host_mapping_read_by_gpu,
                ..
            } => color_converter.prepare_buffer_to_image_storage(
                host_mapping_read_by_gpu.storage_buffer(),
                source_layout,
                destination_texture,
                &resolved_color,
                resolved_color.transfer,
            )?,
        };
        #[cfg(target_os = "linux")]
        if let SurfaceColorConversionSourcePixels::ForeignDmaBufReadThroughAHostMapping {
            host_mapping_read_by_gpu,
            dma_buf_mapping,
        } = &mut source.pixels
        {
            dma_buf_mapping.begin_reads()?;
            host_mapping_read_by_gpu.stage_from_host();
        }
        #[cfg(target_os = "linux")]
        let _reads_through_the_dma_buf_mapping = match &source.pixels {
            SurfaceColorConversionSourcePixels::ForeignDmaBufReadThroughAHostMapping {
                dma_buf_mapping,
                ..
            } => Some(ReadsThroughADmaBufMappingThatEndOnDrop { dma_buf_mapping }),
            SurfaceColorConversionSourcePixels::ImportedPixelBuffer(_) => None,
        };

        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                destination_texture,
                destination.current_layout(),
                VulkanLayout::GENERAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE | VulkanAccess::MEMORY_READ,
                VulkanAccess::SHADER_WRITE,
            )?;
            // A foreign writer's stores reach the GPU through no earlier
            // submission, so the read is made available from nothing; a buffer
            // the GPU or the CPU filled is ordered by the wide barrier every
            // read of an unknown producer takes.
            match &source.pixels {
                SurfaceColorConversionSourcePixels::ImportedPixelBuffer(pixel_buffer) => {
                    if source.planes.written_by_a_foreign_device() {
                        recorder.record_buffer_barrier(
                            pixel_buffer,
                            VulkanStage::NONE,
                            VulkanStage::COMPUTE_SHADER,
                            VulkanAccess::NONE,
                            VulkanAccess::SHADER_READ,
                        )?;
                    } else {
                        recorder.record_buffer_barrier(
                            pixel_buffer,
                            VulkanStage::ALL_COMMANDS,
                            VulkanStage::COMPUTE_SHADER,
                            VulkanAccess::MEMORY_WRITE,
                            VulkanAccess::SHADER_READ,
                        )?;
                    }
                }
                #[cfg(target_os = "linux")]
                SurfaceColorConversionSourcePixels::ForeignDmaBufReadThroughAHostMapping {
                    host_mapping_read_by_gpu,
                    ..
                } => {
                    recorder.record_buffer_barrier(
                        host_mapping_read_by_gpu.storage_buffer(),
                        VulkanStage::HOST,
                        VulkanStage::COMPUTE_SHADER,
                        VulkanAccess::HOST_WRITE,
                        VulkanAccess::SHADER_READ,
                    )?;
                }
            }
            recorder.record_dispatch(
                &kernel,
                source.width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                source.height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                1,
            )?;
            recorder.record_image_barrier(
                destination_texture,
                VulkanLayout::GENERAL,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::SHADER_WRITE,
                VulkanAccess::MEMORY_READ,
            )
        })();
        if let Err(record_failure) = recorded {
            recorder.abort_recording();
            return Err(record_failure);
        }
        recorder.submit()?;
        destination.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        recorder.wait_for_completion()?;
        Ok((destination_surface_id.to_string(), destination))
    }

    /// Resolve `surface_id` as a foreign device's DMA-BUF read through a host
    /// mapping, the way a device that cannot import the buffer resolves it.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn resolve_a_foreign_dma_buf_through_a_host_mapping(
        &mut self,
        gpu: &GpuContext,
        surface_id: &str,
    ) -> Result<()> {
        let store = gpu
            .surface_store()
            .ok_or_else(|| Error::Configuration("no surface-share service is connected".into()))?;
        let registration = store.lookup_dma_buf_registration_without_importing_it(surface_id)?;
        let resolved = foreign_dma_buf_read_through_a_host_mapping(
            gpu,
            surface_id,
            registration,
            WhyAForeignDmaBufIsReadThroughAHostMapping::TheDmaBufImportFailed(
                "a test resolves it so".to_string(),
            ),
        )?;
        self.sources_by_surface_id
            .insert(surface_id.to_string(), resolved);
        Ok(())
    }

    /// Whether this session keeps a resolved source for `surface_id`.
    #[cfg(test)]
    pub(crate) fn keeps_a_source_for(&self, surface_id: &str) -> bool {
        self.sources_by_surface_id.contains_key(surface_id)
    }

    /// Forget what this session resolved for `surface_id`, answering whether
    /// it held anything.
    pub(crate) fn forget_source(&mut self, surface_id: &str) -> bool {
        self.sources_by_surface_id.remove(surface_id).is_some()
    }
}

/// The pooled frame `surface_id` names in this process, with its format,
/// extent and tight layout — resolved per conversion, since a frame id names
/// one generation of a slot the pool hands back once it is read.
fn pooled_frame_held_in_this_process(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<Option<SurfaceColorConversionSource>> {
    if let Some(pixel_buffer) = gpu.pooled_backing_held_in_this_process(surface_id)
        && pixel_buffer.width != 0
    {
        let pixel_format = pixel_buffer.format();
        let (width, height) = (pixel_buffer.width, pixel_buffer.height);
        let tight = PixelPlanesWithinOneAllocation::tightly_packed(pixel_format, width, height)?;
        let planes = PixelPlanesWithinOneAllocation::new(
            pixel_format,
            width,
            height,
            pixel_buffer.plane_size(0),
            tight.plane_offsets_bytes().to_vec(),
            tight.plane_strides_bytes().to_vec(),
            false,
        )?;
        return Ok(Some(SurfaceColorConversionSource {
            pixels: SurfaceColorConversionSourcePixels::ImportedPixelBuffer(pixel_buffer),
            pixel_format,
            width,
            height,
            planes,
        }));
    }
    Ok(None)
}

/// The surface-share registration's pixels: imported into the device, or —
/// for a foreign device's DMA-BUF the device cannot import — mapped into this
/// process and read by the GPU through that mapping.
#[cfg(target_os = "linux")]
fn resolve_color_conversion_source_through_the_surface_store(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<SurfaceColorConversionSource> {
    let store = gpu.surface_store().ok_or_else(|| {
        Error::Configuration(format!(
            "surface {surface_id} is not a pixel buffer this process holds, and no surface-share \
             service is connected to look it up"
        ))
    })?;
    if !gpu.gpu_capabilities().supports_cross_device_dma_buf_probe
        && let Ok(registration) = store.lookup_dma_buf_registration_without_importing_it(surface_id)
        && registration.planes.written_by_a_foreign_device()
    {
        return foreign_dma_buf_read_through_a_host_mapping(
            gpu,
            surface_id,
            registration,
            WhyAForeignDmaBufIsReadThroughAHostMapping::TheDriverIsBlocklistedForCrossDeviceDmaBufImports,
        );
    }
    match store.lookup_pixel_buffer_with_its_plane_layout(surface_id) {
        Ok(looked_up) => Ok(SurfaceColorConversionSource {
            pixels: SurfaceColorConversionSourcePixels::ImportedPixelBuffer(looked_up.pixel_buffer),
            pixel_format: looked_up.pixel_format,
            width: looked_up.width,
            height: looked_up.height,
            planes: looked_up.planes,
        }),
        Err(import_failure) => {
            match store.lookup_dma_buf_registration_without_importing_it(surface_id) {
                Ok(registration) if registration.planes.written_by_a_foreign_device() => {
                    foreign_dma_buf_read_through_a_host_mapping(
                        gpu,
                        surface_id,
                        registration,
                        WhyAForeignDmaBufIsReadThroughAHostMapping::TheDmaBufImportFailed(
                            import_failure.to_string(),
                        ),
                    )
                }
                _ => Err(import_failure),
            }
        }
    }
}

/// Why a foreign device's DMA-BUF reaches the converter through a host
/// mapping instead of a Vulkan import of the buffer.
#[cfg(target_os = "linux")]
enum WhyAForeignDmaBufIsReadThroughAHostMapping {
    /// The device is never asked to import the DMA-BUF, nor its mapping as a
    /// host pointer: NVIDIA refuses that import with
    /// `ERROR_OUT_OF_DEVICE_MEMORY` and then fails every later allocation.
    TheDriverIsBlocklistedForCrossDeviceDmaBufImports,
    /// The device refused the DMA-BUF import with this error; its mapping may
    /// still import as a host pointer.
    TheDmaBufImportFailed(String),
}

/// Map a foreign device's DMA-BUF `registration` read-only and hand the
/// mapping to GPU reads, logging once why the buffer itself was not
/// imported and which tier the mapping took.
#[cfg(target_os = "linux")]
fn foreign_dma_buf_read_through_a_host_mapping(
    gpu: &GpuContext,
    surface_id: &str,
    registration: crate::core::context::surface_store::DmaBufRegistrationWithItsPlaneLayout,
    why_the_dma_buf_is_not_imported: WhyAForeignDmaBufIsReadThroughAHostMapping,
) -> Result<SurfaceColorConversionSource> {
    use crate::linux::dma_buf_mapped_for_cpu_reads::DmaBufMappedForCpuReads;
    let dma_buf_mapping =
        DmaBufMappedForCpuReads::map(registration.dma_buf_fd, registration.allocation_byte_size)?;
    // SAFETY: the mapping moves into the source beside the host mapping and
    // is dropped after it, so it outlives every read the host mapping serves.
    let (host_mapping_read_by_gpu, why_the_dma_buf_is_not_imported) =
        match why_the_dma_buf_is_not_imported {
            WhyAForeignDmaBufIsReadThroughAHostMapping::TheDriverIsBlocklistedForCrossDeviceDmaBufImports => {
                let why = "this device's driver is blocklisted for cross-device DMA-BUF imports";
                let staged = unsafe {
                    gpu.stage_host_mapping_for_gpu_reads(
                        dma_buf_mapping.mapped_ptr(),
                        dma_buf_mapping.allocation_byte_size(),
                        format!("{why}, so its mapping is not imported as a host pointer either"),
                    )
                }?;
                (staged, why.to_string())
            }
            WhyAForeignDmaBufIsReadThroughAHostMapping::TheDmaBufImportFailed(import_failure) => {
                let imported_or_staged = unsafe {
                    gpu.import_host_mapping_for_gpu_reads(
                        dma_buf_mapping.mapped_ptr(),
                        dma_buf_mapping.mapped_byte_len(),
                        dma_buf_mapping.allocation_byte_size(),
                    )
                }?;
                (imported_or_staged, import_failure)
            }
        };
    tracing::info!(
        surface_id,
        tier = host_mapping_read_by_gpu.tier().as_str(),
        why_the_dma_buf_is_not_imported,
        host_pointer_import_refusal = ?host_mapping_read_by_gpu.fallback_reason(),
        "a foreign device's DMA-BUF reaches the colour converter through a host mapping"
    );
    Ok(SurfaceColorConversionSource {
        pixels: SurfaceColorConversionSourcePixels::ForeignDmaBufReadThroughAHostMapping {
            host_mapping_read_by_gpu,
            dma_buf_mapping,
        },
        pixel_format: registration.pixel_format,
        width: registration.width,
        height: registration.height,
        planes: registration.planes,
    })
}

/// One conversion's read of a DMA-BUF through its mapping, already begun
/// with the exporter's read sync, and ended when this drops — after the
/// conversion was waited for, or on the path that abandoned it.
#[cfg(target_os = "linux")]
struct ReadsThroughADmaBufMappingThatEndOnDrop<'mapping> {
    dma_buf_mapping: &'mapping crate::linux::dma_buf_mapped_for_cpu_reads::DmaBufMappedForCpuReads,
}

#[cfg(target_os = "linux")]
impl Drop for ReadsThroughADmaBufMappingThatEndOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(sync_failure) = self.dma_buf_mapping.end_reads() {
            tracing::warn!("ending a read through a DMA-BUF mapping failed: {sync_failure}");
        }
    }
}

#[cfg(target_os = "macos")]
fn resolve_color_conversion_source_through_the_surface_store(
    _gpu: &GpuContext,
    surface_id: &str,
) -> Result<SurfaceColorConversionSource> {
    Err(Error::NotSupported(format!(
        "surface {surface_id} is not a pixel buffer this process holds; a cross-process \
         conversion source is a Linux DMA-BUF import today"
    )))
}

/// The texture `surface_id` names, refused unless the converter can write it:
/// an `rgba8_unorm` texture allocated with `storage_binding` usage.
fn resolve_color_conversion_destination(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<TextureRegistration> {
    gpu.refuse_a_retired_frame_id(surface_id)?;
    let registration = gpu.resolve_texture_registration_by_surface_id(surface_id, None, 0, 0)?;
    let texture = registration.texture();
    if texture.format() != TextureFormat::Rgba8Unorm {
        return Err(Error::GpuError(format!(
            "surface {surface_id} is a {:?} texture; the conversion writes rgba8_unorm",
            texture.format()
        )));
    }
    if !texture.supports_storage_write() {
        return Err(Error::GpuError(format!(
            "surface {surface_id}'s texture was allocated without \"storage_binding\" usage, so \
             the conversion cannot write it"
        )));
    }
    Ok(registration)
}
