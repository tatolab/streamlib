// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's colour converter run on surfaces named by id: a YCbCr pixel
//! buffer — a camera's imported DMA-BUF, a decoder's output — converted into
//! an RGBA texture a stream publishes; on Linux an RGBA texture converted into
//! a YUYV pixel buffer the CPU hands a V4L2 device; and on macOS an RGBA
//! texture converted into an adopted NV12 IOSurface an encoder reads.

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
    /// An IOSurface's pages imported as a storage buffer, zero-copy.
    #[cfg(target_os = "macos")]
    ImportedIOSurface(crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer),
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

/// An adopted NV12 IOSurface an RGBA texture converts into: its pages
/// imported as a storage buffer, and the format its registration states.
#[cfg(target_os = "macos")]
struct AdoptedNv12IOSurfaceConversionDestination {
    imported_iosurface: crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer,
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
}

/// One caller's conversions: the surface-share sources and YCbCr destinations
/// it has named, a converter per source or destination format, and the
/// recorder they dispatch on.
///
/// Owned by one helper's escalate registry, so the converters' staged bindings
/// are never shared with another node's conversions, and what a helper
/// resolved goes away with it.
#[derive(Default)]
pub(crate) struct SurfaceColorConversionSession {
    sources_by_surface_id: HashMap<String, SurfaceColorConversionSource>,
    color_converters_by_source_format: HashMap<PixelFormat, RhiColorConverter>,
    rgba_or_bgra_texture_sources_by_surface_id: HashMap<String, TextureRegistration>,
    #[cfg(target_os = "linux")]
    yuyv_pixel_buffer_destinations_by_surface_id: HashMap<String, PixelBuffer>,
    #[cfg(target_os = "linux")]
    rgba_to_yuyv_color_converter: Option<RhiColorConverter>,
    #[cfg(target_os = "macos")]
    nv12_iosurface_destinations_by_surface_id:
        HashMap<String, AdoptedNv12IOSurfaceConversionDestination>,
    #[cfg(target_os = "macos")]
    rgba_to_nv12_color_converters_by_destination_format: HashMap<PixelFormat, RhiColorConverter>,
    conversion_recorder: Option<RhiCommandRecorder>,
}

impl SurfaceColorConversionSession {
    /// Convert between `source_surface_id` and `destination_surface_id`,
    /// keeping the transfer curve, and return once the destination's next
    /// reader sees the result — with the texture registration whose layout
    /// the conversion settled, under the id it resolved under, for the caller
    /// to publish. `ycbcr_side_color` describes whichever side is YCbCr: a
    /// YCbCr source converting into an RGBA texture, on Linux an RGBA texture
    /// converting into a YUYV pixel buffer, or on macOS an RGBA texture
    /// converting into an adopted NV12 IOSurface.
    pub(crate) fn convert_surface_into(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        ycbcr_side_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        gpu.refuse_a_retired_frame_id(source_surface_id)?;
        #[cfg(target_os = "linux")]
        if self.converts_into_a_yuyv_pixel_buffer(gpu, source_surface_id, destination_surface_id)? {
            return self.convert_an_rgba_texture_into_a_yuyv_pixel_buffer(
                gpu,
                source_surface_id,
                destination_surface_id,
                ycbcr_side_color,
            );
        }
        #[cfg(target_os = "macos")]
        if self.converts_into_an_adopted_nv12_iosurface(
            gpu,
            source_surface_id,
            destination_surface_id,
        )? {
            return self.convert_an_rgba_texture_into_an_adopted_nv12_iosurface(
                gpu,
                source_surface_id,
                destination_surface_id,
                ycbcr_side_color,
            );
        }
        if self.converts_from_an_rgba_or_bgra_texture(gpu, source_surface_id)? {
            return self.convert_an_rgba_or_bgra_texture_into_an_rgba_texture(
                gpu,
                source_surface_id,
                destination_surface_id,
            );
        }
        let source_color = ycbcr_side_color;
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
        let resolved_color = crate::core::color::ResolveH273ColorVuiDefaults::resolve_defaults(
            source_color,
            ColorSpaceKind::Yuv,
        );
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
            #[cfg(target_os = "macos")]
            SurfaceColorConversionSourcePixels::ImportedIOSurface(imported_iosurface) => {
                color_converter.prepare_buffer_to_image_storage(
                    imported_iosurface.storage_buffer(),
                    source_layout,
                    destination_texture,
                    &resolved_color,
                    resolved_color.transfer,
                )?
            }
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
                #[cfg(target_os = "macos")]
                SurfaceColorConversionSourcePixels::ImportedIOSurface(imported_iosurface) => {
                    recorder.record_buffer_barrier(
                        imported_iosurface.storage_buffer(),
                        VulkanStage::NONE,
                        VulkanStage::COMPUTE_SHADER,
                        VulkanAccess::NONE,
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

    /// Whether `source_surface_id` is an RGBA or BGRA texture another process
    /// registered, imported and kept the first time a conversion names it. A
    /// source this session already reads as YCbCr, or a pooled frame, never
    /// asks the surface store.
    fn converts_from_an_rgba_or_bgra_texture(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
    ) -> Result<bool> {
        if self
            .rgba_or_bgra_texture_sources_by_surface_id
            .contains_key(source_surface_id)
        {
            return Ok(true);
        }
        if self.sources_by_surface_id.contains_key(source_surface_id)
            || crate::core::rhi::PublishedPixelBufferFrameId::parse(source_surface_id).is_some()
        {
            return Ok(false);
        }
        let Some(store) = gpu.surface_store() else {
            return Ok(false);
        };
        let Ok((texture, layout_the_producer_left_it_in)) = store.lookup_texture(source_surface_id)
        else {
            return Ok(false);
        };
        if !matches!(
            texture.format(),
            TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm
        ) {
            return Ok(false);
        }
        self.rgba_or_bgra_texture_sources_by_surface_id.insert(
            source_surface_id.to_string(),
            TextureRegistration::new(texture, layout_the_producer_left_it_in),
        );
        Ok(true)
    }

    /// Copy the RGBA or BGRA texture `source_surface_id` into the RGBA
    /// texture `destination_surface_id`, texel for texel, and return once the
    /// destination's next reader sees it — with the destination's
    /// registration, whose layout the conversion settled.
    ///
    /// The source is another process's image this one imported, so its
    /// contents reach this queue through no earlier submission: the barrier
    /// that makes them visible waits on nothing, and leaves the image in the
    /// layout the next conversion transitions it from.
    fn convert_an_rgba_or_bgra_texture_into_an_rgba_texture(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<(String, TextureRegistration)> {
        const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout =
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
        let source = self
            .rgba_or_bgra_texture_sources_by_surface_id
            .get(source_surface_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "surface {source_surface_id} was resolved and is gone"
                ))
            })?;
        let source_texture = source.texture();
        let destination = resolve_color_conversion_destination(gpu, destination_surface_id)?;
        let destination_texture = destination.texture();
        let source_pixel_format = match source_texture.format() {
            TextureFormat::Bgra8Unorm => PixelFormat::Bgra32,
            _ => PixelFormat::Rgba32,
        };
        let color_converter = match self
            .color_converters_by_source_format
            .get(&source_pixel_format)
        {
            Some(color_converter) => color_converter,
            None => self
                .color_converters_by_source_format
                .entry(source_pixel_format)
                .or_insert(gpu.create_color_converter(source_pixel_format, PixelFormat::Rgba32)?),
        };
        let kernel =
            color_converter.prepare_image_to_image_storage(source_texture, destination_texture)?;
        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                source_texture,
                source.current_layout(),
                LAYOUT_THE_CONVERSION_SAMPLES_IN,
                VulkanStage::NONE,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::NONE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_image_barrier(
                destination_texture,
                destination.current_layout(),
                VulkanLayout::GENERAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE | VulkanAccess::MEMORY_READ,
                VulkanAccess::SHADER_WRITE,
            )?;
            recorder.record_dispatch(
                &kernel,
                destination_texture
                    .width()
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                destination_texture
                    .height()
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
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
        source.update_layout(LAYOUT_THE_CONVERSION_SAMPLES_IN);
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
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn keeps_a_source_for(&self, surface_id: &str) -> bool {
        self.sources_by_surface_id.contains_key(surface_id)
    }

    /// Forget what this session resolved for `surface_id`, as a source or a
    /// destination, answering whether it held anything.
    pub(crate) fn forget_resolved_surface(&mut self, surface_id: &str) -> bool {
        let forgot_a_source = self.sources_by_surface_id.remove(surface_id).is_some()
            | self
                .rgba_or_bgra_texture_sources_by_surface_id
                .remove(surface_id)
                .is_some();
        #[cfg(target_os = "macos")]
        let forgot_a_destination = self
            .nv12_iosurface_destinations_by_surface_id
            .remove(surface_id)
            .is_some();
        #[cfg(target_os = "linux")]
        let forgot_a_destination = self
            .yuyv_pixel_buffer_destinations_by_surface_id
            .remove(surface_id)
            .is_some();
        forgot_a_source || forgot_a_destination
    }

    /// Whether `destination_surface_id` is a `yuyv` pixel buffer, resolved and
    /// kept the first time a conversion names it. A source this session
    /// already reads as YCbCr, or a pooled frame, never asks the surface
    /// store.
    #[cfg(target_os = "linux")]
    fn converts_into_a_yuyv_pixel_buffer(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<bool> {
        if self
            .yuyv_pixel_buffer_destinations_by_surface_id
            .contains_key(destination_surface_id)
        {
            return Ok(true);
        }
        if self.sources_by_surface_id.contains_key(source_surface_id)
            || self
                .rgba_or_bgra_texture_sources_by_surface_id
                .contains_key(source_surface_id)
            || a_pooled_ycbcr_frame_held_in_this_process(gpu, source_surface_id)
        {
            return Ok(false);
        }
        let Ok(pixel_buffer) = gpu.resolve_pixel_buffer_by_surface_id(destination_surface_id)
        else {
            return Ok(false);
        };
        if pixel_buffer.format() != PixelFormat::Yuyv422 {
            return Ok(false);
        }
        self.yuyv_pixel_buffer_destinations_by_surface_id
            .insert(destination_surface_id.to_string(), pixel_buffer);
        Ok(true)
    }

    /// Convert the RGBA texture `source_surface_id` into the `yuyv` pixel
    /// buffer `destination_surface_id`, tightly packed, in
    /// `ycbcr_side_color`'s matrix and range, and return once the buffer's
    /// CPU readers see it — with the source's registration, whose layout the
    /// conversion settled.
    #[cfg(target_os = "linux")]
    fn convert_an_rgba_texture_into_a_yuyv_pixel_buffer(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        ycbcr_side_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout =
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
        let destination = self
            .yuyv_pixel_buffer_destinations_by_surface_id
            .get(destination_surface_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "surface {destination_surface_id} was resolved and is gone"
                ))
            })?;
        let source = gpu.resolve_texture_registration_by_surface_id(
            source_surface_id,
            None,
            destination.width,
            destination.height,
        )?;
        let source_texture = source.texture();
        if (source_texture.width(), source_texture.height())
            != (destination.width, destination.height)
        {
            return Err(Error::GpuError(format!(
                "texture {source_surface_id} is {}x{} and pixel buffer {destination_surface_id} \
                 is {}x{}; the conversion scales nothing",
                source_texture.width(),
                source_texture.height(),
                destination.width,
                destination.height
            )));
        }
        let resolved_color = crate::core::color::ResolveH273ColorVuiDefaults::resolve_defaults(
            ycbcr_side_color,
            ColorSpaceKind::Yuv,
        );
        let color_converter = match self.rgba_to_yuyv_color_converter.as_ref() {
            Some(color_converter) => color_converter,
            None => self
                .rgba_to_yuyv_color_converter
                .insert(gpu.create_color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)?),
        };
        let kernel = color_converter.prepare_image_to_yuyv_pixel_buffer(
            source_texture,
            destination,
            destination.width * 2,
            &resolved_color,
        )?;
        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                source_texture,
                source.current_layout(),
                LAYOUT_THE_CONVERSION_SAMPLES_IN,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_dispatch(
                &kernel,
                destination
                    .width
                    .div_ceil(2)
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                destination.height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                1,
            )?;
            recorder.record_buffer_barrier(
                destination,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::HOST,
                VulkanAccess::SHADER_WRITE,
                VulkanAccess::HOST_READ,
            )
        })();
        if let Err(record_failure) = recorded {
            recorder.abort_recording();
            return Err(record_failure);
        }
        recorder.submit()?;
        source.update_layout(LAYOUT_THE_CONVERSION_SAMPLES_IN);
        recorder.wait_for_completion()?;
        Ok((source_surface_id.to_string(), source))
    }

    /// Whether `destination_surface_id` is an adopted NV12 IOSurface, resolved
    /// and kept the first time a conversion names it. A source this session
    /// already reads as YCbCr, or a pooled YCbCr frame, never asks the surface
    /// store.
    #[cfg(target_os = "macos")]
    fn converts_into_an_adopted_nv12_iosurface(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> Result<bool> {
        if self
            .nv12_iosurface_destinations_by_surface_id
            .contains_key(destination_surface_id)
        {
            return Ok(true);
        }
        if self.sources_by_surface_id.contains_key(source_surface_id)
            || a_pooled_ycbcr_frame_held_in_this_process(gpu, source_surface_id)
        {
            return Ok(false);
        }
        let Some(store) = gpu.surface_store() else {
            return Ok(false);
        };
        let Ok(registration) =
            store.lookup_iosurface_registration_without_importing_it(destination_surface_id)
        else {
            return Ok(false);
        };
        if !matches!(
            registration.pixel_format,
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange
        ) {
            return Ok(false);
        }
        let imported_iosurface = gpu.import_iosurface_as_storage_buffer(&registration.iosurface)?;
        self.nv12_iosurface_destinations_by_surface_id.insert(
            destination_surface_id.to_string(),
            AdoptedNv12IOSurfaceConversionDestination {
                imported_iosurface,
                pixel_format: registration.pixel_format,
                width: registration.width,
                height: registration.height,
            },
        );
        Ok(true)
    }

    /// Convert the RGBA texture `source_surface_id` — or an RGBA pixel buffer,
    /// refreshed into a texture first — into the adopted NV12 IOSurface
    /// `destination_surface_id` in `ycbcr_side_color`'s matrix, and
    /// return once the surface's CPU and media-engine readers see it — with
    /// the source's registration, whose layout the conversion settled.
    #[cfg(target_os = "macos")]
    fn convert_an_rgba_texture_into_an_adopted_nv12_iosurface(
        &mut self,
        gpu: &GpuContext,
        source_surface_id: &str,
        destination_surface_id: &str,
        ycbcr_side_color: &H273ColorVui,
    ) -> Result<(String, TextureRegistration)> {
        const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout =
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL;
        let destination = self
            .nv12_iosurface_destinations_by_surface_id
            .get(destination_surface_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "surface {destination_surface_id} was resolved and is gone"
                ))
            })?;
        let source = gpu.resolve_texture_registration_by_surface_id(
            source_surface_id,
            None,
            destination.width,
            destination.height,
        )?;
        let source_texture = source.texture();
        if (source_texture.width(), source_texture.height())
            != (destination.width, destination.height)
        {
            return Err(Error::GpuError(format!(
                "texture {source_surface_id} is {}x{} and IOSurface {destination_surface_id} is \
                 {}x{}; the conversion scales nothing",
                source_texture.width(),
                source_texture.height(),
                destination.width,
                destination.height
            )));
        }
        let destination_is_full_range = destination.pixel_format == PixelFormat::Nv12FullRange;
        if let Some(stated_full_range) = ycbcr_side_color.full_range
            && stated_full_range != destination_is_full_range
        {
            return Err(Error::GpuError(format!(
                "IOSurface {destination_surface_id} is {:?} and the conversion was asked for {} \
                 range",
                destination.pixel_format,
                if stated_full_range { "full" } else { "limited" }
            )));
        }
        let resolved_color = crate::core::color::ResolveH273ColorVuiDefaults::resolve_defaults(
            &H273ColorVui {
                full_range: Some(destination_is_full_range),
                ..*ycbcr_side_color
            },
            ColorSpaceKind::Yuv,
        );
        let color_converter = match self
            .rgba_to_nv12_color_converters_by_destination_format
            .get(&destination.pixel_format)
        {
            Some(color_converter) => color_converter,
            None => self
                .rgba_to_nv12_color_converters_by_destination_format
                .entry(destination.pixel_format)
                .or_insert(
                    gpu.create_color_converter(PixelFormat::Rgba32, destination.pixel_format)?,
                ),
        };
        let destination_storage_buffer = destination.imported_iosurface.storage_buffer();
        let kernel = color_converter.prepare_image_to_nv12_buffer(
            source_texture,
            destination_storage_buffer,
            destination.imported_iosurface.nv12_source_layout()?,
            &resolved_color,
        )?;
        let (dispatch_group_x, dispatch_group_y) =
            RhiColorConverter::image_to_nv12_buffer_dispatch_group_counts(
                source_texture.width(),
                source_texture.height(),
            );
        let recorder = match self.conversion_recorder.as_mut() {
            Some(recorder) => recorder,
            None => self
                .conversion_recorder
                .insert(gpu.create_command_recorder("surface_color_conversion")?),
        };
        recorder.begin()?;
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                source_texture,
                source.current_layout(),
                LAYOUT_THE_CONVERSION_SAMPLES_IN,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_dispatch(&kernel, dispatch_group_x, dispatch_group_y, 1)?;
            recorder.record_buffer_barrier(
                destination_storage_buffer,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::HOST,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::HOST_READ,
            )
        })();
        if let Err(record_failure) = recorded {
            recorder.abort_recording();
            return Err(record_failure);
        }
        recorder.submit()?;
        source.update_layout(LAYOUT_THE_CONVERSION_SAMPLES_IN);
        recorder.wait_for_completion()?;
        Ok((source_surface_id.to_string(), source))
    }
}

/// Whether `surface_id` names a pooled YCbCr frame in this process, which
/// converts into an RGBA texture and never out of one.
fn a_pooled_ycbcr_frame_held_in_this_process(gpu: &GpuContext, surface_id: &str) -> bool {
    crate::core::rhi::PublishedPixelBufferFrameId::parse(surface_id).is_some()
        && gpu
            .pooled_backing_held_in_this_process(surface_id)
            .is_some_and(|pixel_buffer| {
                matches!(
                    pixel_buffer.format(),
                    PixelFormat::Nv12VideoRange
                        | PixelFormat::Nv12FullRange
                        | PixelFormat::Yuyv422
                        | PixelFormat::Uyvy422
                )
            })
}

/// The pooled frame `surface_id` names in this process, with its format,
/// extent and tight layout — resolved per conversion, since a frame id names
/// one generation of a slot the pool hands back once it is read.
fn pooled_frame_held_in_this_process(
    gpu: &GpuContext,
    surface_id: &str,
) -> Result<Option<SurfaceColorConversionSource>> {
    if crate::core::rhi::PublishedPixelBufferFrameId::parse(surface_id).is_none() {
        return Ok(None);
    }
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

/// The surface-share registration's IOSurface, its pages imported as a
/// storage buffer, with its planes where the surface itself lays them out.
#[cfg(target_os = "macos")]
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
    let registration = store.lookup_iosurface_registration_without_importing_it(surface_id)?;
    let planes = iosurface_planes_within_its_allocation(
        &registration.iosurface,
        registration.pixel_format,
        registration.width,
        registration.height,
    )?;
    let imported_iosurface = gpu.import_iosurface_as_storage_buffer(&registration.iosurface)?;
    Ok(SurfaceColorConversionSource {
        pixels: SurfaceColorConversionSourcePixels::ImportedIOSurface(imported_iosurface),
        pixel_format: registration.pixel_format,
        width: registration.width,
        height: registration.height,
        planes,
    })
}

/// Where `iosurface`'s planes sit from its base address, at its own strides;
/// a surface another device fills, so its reads take no prior GPU write.
#[cfg(target_os = "macos")]
fn iosurface_planes_within_its_allocation(
    iosurface: &objc2_io_surface::IOSurfaceRef,
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
) -> Result<PixelPlanesWithinOneAllocation> {
    let base_address = iosurface.base_address().as_ptr() as u64;
    let (plane_offsets_bytes, plane_strides_bytes) = if iosurface.plane_count() == 0 {
        (vec![0], vec![iosurface.bytes_per_row() as u64])
    } else {
        (0..iosurface.plane_count())
            .map(|plane_index| {
                (
                    (iosurface.base_address_of_plane(plane_index).as_ptr() as u64)
                        .saturating_sub(base_address),
                    iosurface.bytes_per_row_of_plane(plane_index) as u64,
                )
            })
            .unzip()
    };
    PixelPlanesWithinOneAllocation::new(
        pixel_format,
        width,
        height,
        iosurface.alloc_size() as u64,
        plane_offsets_bytes,
        plane_strides_bytes,
        true,
    )
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
