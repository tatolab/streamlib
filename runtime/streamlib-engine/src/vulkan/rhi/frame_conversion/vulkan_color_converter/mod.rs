// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `(src_format, dst_format)`-keyed color converter backed by
//! [`VulkanComputeKernel`].
//!
//! Each converter instance owns up to one kernel per binding-shape
//! (buffer source, image source); kernels are built lazily on first
//! use and cached for the lifetime of the converter. `ResolvedColorInfo`
//! drives the matrix + transfer math via per-frame push constants —
//! mid-stream color changes don't touch the pipeline.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::color::{ResolvedColorInfo, TransferId};
use crate::core::rhi::{
    COLOR_CONVERTER_PUSH_CONSTANT_SIZE, ColorConverterPushConstants, ComputeBindingSpec,
    ComputeKernelDescriptor, PixelFormat, SourceLayoutInfo, Texture, TextureFormat,
};
use crate::core::{Error, Result};
use crate::host_rhi::HostTextureExt as _;

use crate::vulkan::rhi::{HostVulkanDevice, VulkanComputeKernel};

/// Compute-shader workgroup tile size (one pixel per thread, 16×16
/// workgroups). Exposed so consumers driving the dispatch through
/// [`crate::vulkan::rhi::RhiCommandRecorder::record_dispatch`] can
/// compute `(group_x, group_y) = ⌈(width, height) / 16⌉`.
pub(crate) const COLOR_CONVERTER_WORKGROUP_SIZE: u32 = 16;

const BUFFER_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // YCbCr / RGB byte input
    ComputeBindingSpec::storage_image(1),  // RGBA output
];

const IMAGE_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::sampled_texture(0), // RGBA or BGRA input, fetched by texel
    ComputeBindingSpec::storage_image(1),   // RGBA output
];

const IMAGE_TO_BYTE_BUFFER_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::sampled_texture(0), // RGBA input, fetched by texel
    ComputeBindingSpec::storage_buffer(1),  // YUYV or NV12 byte output
];

/// Pixels one thread of the image→NV12 kernel writes, across and down:
/// `color_convert_rgba_image_to_nv12_buffer.slang`'s 4×2 block.
const IMAGE_TO_NV12_BUFFER_PIXELS_PER_THREAD: (u32, u32) = (4, 2);

/// The `(group_x, group_y)` a `width` × `height` image→NV12 dispatch
/// covers the frame with.
pub(crate) fn image_to_nv12_buffer_dispatch_group_counts(width: u32, height: u32) -> (u32, u32) {
    let (pixels_across, pixels_down) = IMAGE_TO_NV12_BUFFER_PIXELS_PER_THREAD;
    (
        width
            .div_ceil(pixels_across)
            .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
        height
            .div_ceil(pixels_down)
            .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
    )
}

/// The kernel in `slot`, built by `build` the first time it is asked for.
fn kernel_built_once(
    slot: &Mutex<Option<Arc<VulkanComputeKernel>>>,
    build: impl FnOnce() -> Result<VulkanComputeKernel>,
) -> Result<Arc<VulkanComputeKernel>> {
    let mut built = slot.lock();
    if let Some(kernel) = built.as_ref() {
        return Ok(Arc::clone(kernel));
    }
    let kernel = Arc::new(build()?);
    *built = Some(Arc::clone(&kernel));
    Ok(kernel)
}

/// Vulkan implementation of [`crate::core::rhi::RhiColorConverter`].
pub(crate) struct VulkanColorConverter {
    vulkan_device: Arc<HostVulkanDevice>,
    src_format: PixelFormat,
    dst_format: PixelFormat,
    buffer_to_image_kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
    image_to_yuyv_buffer_kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
    image_to_nv12_buffer_kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
    image_to_image_kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
}

impl VulkanColorConverter {
    /// Build a converter for the given `(src, dst)` pair. Kernels are
    /// allocated lazily on first dispatch.
    pub(crate) fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        src_format: PixelFormat,
        dst_format: PixelFormat,
    ) -> Result<Self> {
        validate_format_pair(src_format, dst_format)?;
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            src_format,
            dst_format,
            buffer_to_image_kernel: Mutex::new(None),
            image_to_yuyv_buffer_kernel: Mutex::new(None),
            image_to_nv12_buffer_kernel: Mutex::new(None),
            image_to_image_kernel: Mutex::new(None),
        })
    }

    pub(crate) fn src_format(&self) -> PixelFormat {
        self.src_format
    }

    pub(crate) fn dst_format(&self) -> PixelFormat {
        self.dst_format
    }

    /// Bind `(src, dst)` + push-constants on the buffer→image kernel
    /// and return the kernel for the caller to dispatch. Used by
    /// consumers that drive dispatch through
    /// [`crate::vulkan::rhi::RhiCommandRecorder::record_dispatch`]
    /// so the compute step nests inside their own recorded command
    /// buffer with barriers and copies.
    ///
    /// `dst_transfer` is the output transfer curve; when it matches
    /// `info.transfer` the shader bypasses the transfer-function path.
    /// `Srgb` is the right default for the RGBA8_UNORM displays we
    /// ship today; once display color-space negotiation (#817) lands,
    /// consumers thread the negotiated curve through here.
    pub(crate) fn prepare_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let kernel = self.get_or_build_buffer_to_image_kernel()?;
        kernel.set_storage_buffer_storage_from_byte_offset(
            0,
            src,
            u64::from(src_layout.plane0_offset_bytes),
        )?;
        self.finish_buffer_to_image(&kernel, dst, info, dst_transfer, src_layout)?;
        Ok(kernel)
    }

    pub(crate) fn prepare_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let kernel = self.get_or_build_buffer_to_image_kernel()?;
        kernel.set_storage_buffer_pixel_from_byte_offset(
            0,
            src,
            u64::from(src_layout.plane0_offset_bytes),
        )?;
        self.finish_buffer_to_image(&kernel, dst, info, dst_transfer, src_layout)?;
        Ok(kernel)
    }

    /// Convert `src` into `dst` end-to-end. Builds (if needed),
    /// binds, and dispatches the buffer→image kernel via its own
    /// command buffer + fence + queue submit. Use this when there's
    /// no surrounding [`crate::vulkan::rhi::RhiCommandRecorder`];
    /// otherwise prefer [`Self::prepare_buffer_to_image_storage`].
    pub(crate) fn convert_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        let kernel =
            self.prepare_buffer_to_image_storage(src, src_layout, dst, info, TransferId::Srgb)?;
        Self::dispatch_buffer_to_image(&kernel, dst)
    }

    /// [`PixelBuffer`](crate::core::rhi::PixelBuffer) variant of
    /// [`Self::convert_buffer_to_image_storage`].
    pub(crate) fn convert_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        let kernel =
            self.prepare_buffer_to_image_pixel(src, src_layout, dst, info, TransferId::Srgb)?;
        Self::dispatch_buffer_to_image(&kernel, dst)
    }

    /// Bind an RGBA texture source, a YUYV storage-buffer destination and
    /// the encoding push-constants on the image→buffer kernel, and return
    /// it for the caller to dispatch over `⌈width/2 / 16⌉ × ⌈height / 16⌉`
    /// groups. The destination must hold `dst_stride_bytes × height` bytes.
    pub(crate) fn prepare_image_to_yuyv_buffer(
        &self,
        src: &Texture,
        dst: &crate::core::rhi::StorageBuffer,
        dst_stride_bytes: u32,
        info: &ResolvedColorInfo,
    ) -> Result<Arc<VulkanComputeKernel>> {
        self.prepare_image_to_yuyv_destination(
            src,
            dst.byte_size(),
            dst_stride_bytes,
            info,
            |kernel| kernel.set_storage_buffer_storage(1, dst),
        )
    }

    /// [`PixelBuffer`](crate::core::rhi::PixelBuffer)-shape destination
    /// variant of [`Self::prepare_image_to_yuyv_buffer`]: a `yuyv` pixel
    /// buffer the CPU reads the converted picture from.
    pub(crate) fn prepare_image_to_yuyv_pixel_buffer(
        &self,
        src: &Texture,
        dst: &crate::core::rhi::PixelBuffer,
        dst_stride_bytes: u32,
        info: &ResolvedColorInfo,
    ) -> Result<Arc<VulkanComputeKernel>> {
        self.prepare_image_to_yuyv_destination(
            src,
            dst.plane_size(0),
            dst_stride_bytes,
            info,
            |kernel| kernel.set_storage_buffer_pixel(1, dst),
        )
    }

    fn prepare_image_to_yuyv_destination(
        &self,
        src: &Texture,
        dst_byte_size: u64,
        dst_stride_bytes: u32,
        info: &ResolvedColorInfo,
        bind_the_destination: impl FnOnce(&VulkanComputeKernel) -> Result<()>,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let (width, height) = (src.width(), src.height());
        if dst_stride_bytes < width * 2 || !dst_stride_bytes.is_multiple_of(4) {
            return Err(Error::Configuration(format!(
                "color converter image→YUYV: destination stride {dst_stride_bytes} must be a \
                 multiple of 4 and at least 2 × width ({width})"
            )));
        }
        let needed = u64::from(dst_stride_bytes) * u64::from(height);
        if dst_byte_size < needed {
            return Err(Error::Configuration(format!(
                "color converter image→YUYV: destination holds {dst_byte_size} bytes but \
                 {height} rows of {dst_stride_bytes} need {needed}"
            )));
        }
        let kernel = kernel_built_once(&self.image_to_yuyv_buffer_kernel, || {
            self.build_image_to_byte_buffer_kernel(
                &[PixelFormat::Yuyv422],
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/color_convert_rgba_image_to_yuyv_buffer.spv"
                )),
                "rgba_image_to_yuyv_buffer",
            )
        })?;
        kernel.set_sampled_texture(0, src)?;
        bind_the_destination(&kernel)?;
        let push = ColorConverterPushConstants::from_resolved_for_rgb_to_ycbcr(
            info,
            width,
            height,
            dst_stride_bytes,
        );
        kernel.set_push_constants_value(&push)?;
        Ok(kernel)
    }

    /// Bind an RGBA texture source, an NV12 storage-buffer destination laid
    /// out as `dst_layout` describes, and the encoding push-constants on the
    /// image→NV12 kernel, and return it for the caller to dispatch over
    /// `image_to_nv12_buffer_dispatch_group_counts` groups.
    pub(crate) fn prepare_image_to_nv12_buffer(
        &self,
        src: &Texture,
        dst: &crate::core::rhi::StorageBuffer,
        dst_layout: SourceLayoutInfo,
        info: &ResolvedColorInfo,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let (width, height) = (src.width(), src.height());
        refuse_an_nv12_destination_the_kernel_cannot_write(width, height, dst, dst_layout)?;
        let kernel = kernel_built_once(&self.image_to_nv12_buffer_kernel, || {
            self.build_image_to_byte_buffer_kernel(
                &[PixelFormat::Nv12VideoRange, PixelFormat::Nv12FullRange],
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/color_convert_rgba_image_to_nv12_buffer.spv"
                )),
                "rgba_image_to_nv12_buffer",
            )
        })?;
        kernel.set_sampled_texture(0, src)?;
        kernel.set_storage_buffer_storage_from_byte_offset(
            1,
            dst,
            u64::from(dst_layout.plane0_offset_bytes),
        )?;
        let push = ColorConverterPushConstants::from_resolved_for_rgb_to_ycbcr_nv12(
            info, width, height, dst_layout,
        );
        kernel.set_push_constants_value(&push)?;
        Ok(kernel)
    }

    /// Bind a sampled RGBA or BGRA texture source and an RGBA storage-image
    /// destination of the same extent on the image→image kernel, and return
    /// it for the caller to dispatch over `⌈width / 16⌉ × ⌈height / 16⌉`
    /// groups.
    pub(crate) fn prepare_image_to_image_storage(
        &self,
        src: &Texture,
        dst: &Texture,
    ) -> Result<Arc<VulkanComputeKernel>> {
        self.refuse_a_format_pair_the_image_to_image_kernel_does_not_convert()?;
        if (src.width(), src.height()) != (dst.width(), dst.height()) {
            return Err(Error::Configuration(format!(
                "color converter image→image: source is {}x{} and destination {}x{}; the \
                 conversion scales nothing",
                src.width(),
                src.height(),
                dst.width(),
                dst.height()
            )));
        }
        if dst.format() != TextureFormat::Rgba8Unorm {
            return Err(Error::Configuration(format!(
                "color converter image→image: destination is {:?}; the kernel writes rgba8_unorm",
                dst.format()
            )));
        }
        if src.vulkan_inner().image() == dst.vulkan_inner().image() {
            return Err(Error::Configuration(
                "color converter image→image: the source and destination are one image, which \
                 cannot be sampled and written in one dispatch"
                    .to_string(),
            ));
        }
        let kernel = self.image_to_image_kernel_built_once()?;
        kernel.set_sampled_texture(0, src)?;
        kernel.set_storage_image(1, dst)?;
        Ok(kernel)
    }

    /// Build the image→image kernel's pipeline now, so no later
    /// [`Self::prepare_image_to_image_storage`] builds one.
    pub(crate) fn build_the_image_to_image_kernel(&self) -> Result<()> {
        self.image_to_image_kernel_built_once().map(drop)
    }

    fn refuse_a_format_pair_the_image_to_image_kernel_does_not_convert(&self) -> Result<()> {
        if !matches!(self.src_format, PixelFormat::Rgba32 | PixelFormat::Bgra32)
            || self.dst_format != PixelFormat::Rgba32
        {
            return Err(Error::NotSupported(format!(
                "color converter image→image: {:?} → {:?} is not RGBA or BGRA → RGBA",
                self.src_format, self.dst_format
            )));
        }
        Ok(())
    }

    fn image_to_image_kernel_built_once(&self) -> Result<Arc<VulkanComputeKernel>> {
        self.refuse_a_format_pair_the_image_to_image_kernel_does_not_convert()?;
        kernel_built_once(&self.image_to_image_kernel, || {
            let label = format!(
                "color_convert_rgba_image_to_rgba_image:{:?}_to_{:?}",
                self.src_format, self.dst_format
            );
            VulkanComputeKernel::new(
                &self.vulkan_device,
                &ComputeKernelDescriptor {
                    entry_point: "main",
                    label: label.as_str(),
                    spv: include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/color_convert_rgba_image_to_rgba_image.spv"
                    )),
                    bindings: IMAGE_TO_IMAGE_BINDINGS,
                    push_constant_size: 0,
                    specialization_constants: &[],
                },
            )
        })
    }

    /// Build an RGBA-image-to-byte-buffer kernel, refused unless this
    /// converter's destination is one of `kernel_dst_formats`.
    fn build_image_to_byte_buffer_kernel(
        &self,
        kernel_dst_formats: &[PixelFormat],
        spv: &[u8],
        kernel_name: &str,
    ) -> Result<VulkanComputeKernel> {
        if !kernel_dst_formats.contains(&self.dst_format) {
            return Err(Error::NotSupported(format!(
                "color converter {kernel_name}: destination {:?} is none of {kernel_dst_formats:?}",
                self.dst_format
            )));
        }
        let label = format!(
            "color_convert_{kernel_name}:{:?}_to_{:?}",
            self.src_format, self.dst_format
        );
        VulkanComputeKernel::new(
            &self.vulkan_device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: label.as_str(),
                spv,
                bindings: IMAGE_TO_BYTE_BUFFER_BINDINGS,
                push_constant_size: COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
                specialization_constants: &[],
            },
        )
    }

    fn finish_buffer_to_image(
        &self,
        kernel: &VulkanComputeKernel,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
        src_layout: SourceLayoutInfo,
    ) -> Result<()> {
        let width = dst.width();
        let height = dst.height();
        kernel.set_storage_image(1, dst)?;
        let push = ColorConverterPushConstants::from_resolved(
            info,
            dst_transfer,
            width,
            height,
            src_layout,
        );
        kernel.set_push_constants_value(&push)?;
        Ok(())
    }

    fn dispatch_buffer_to_image(kernel: &VulkanComputeKernel, dst: &Texture) -> Result<()> {
        let dispatch_x = dst.width().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        let dispatch_y = dst.height().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        kernel.dispatch(dispatch_x, dispatch_y, 1)
    }

    fn get_or_build_buffer_to_image_kernel(&self) -> Result<Arc<VulkanComputeKernel>> {
        kernel_built_once(&self.buffer_to_image_kernel, || {
            self.build_buffer_to_image_kernel()
        })
    }

    fn build_buffer_to_image_kernel(&self) -> Result<VulkanComputeKernel> {
        let spv: &[u8] = match self.src_format {
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange => {
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/color_convert_nv12_buffer_to_rgba.spv"
                ))
            }
            PixelFormat::Yuyv422 => {
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/color_convert_yuyv_buffer_to_rgba.spv"
                ))
            }
            other => {
                return Err(Error::NotSupported(format!(
                    "color converter buffer-source path: unsupported src format {:?}",
                    other
                )));
            }
        };
        let label = format!(
            "color_convert_buffer_to_image:{:?}_to_{:?}",
            self.src_format, self.dst_format
        );
        VulkanComputeKernel::new(
            &self.vulkan_device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: label.as_str(),
                spv,
                bindings: BUFFER_TO_IMAGE_BINDINGS,
                push_constant_size: COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
                specialization_constants: &[],
            },
        )
    }
}

/// The kernel writes whole `u32`s: both strides and the chroma offset must
/// be multiples of 4, each luma row must hold the width rounded up to 4, and
/// both planes must fit the buffer past the luma plane's start.
fn refuse_an_nv12_destination_the_kernel_cannot_write(
    width: u32,
    height: u32,
    dst: &crate::core::rhi::StorageBuffer,
    dst_layout: SourceLayoutInfo,
) -> Result<()> {
    let SourceLayoutInfo {
        plane0_offset_bytes,
        plane0_stride_bytes,
        plane1_stride_bytes,
        plane1_offset_bytes,
    } = dst_layout;
    let luma_row_bytes = width.next_multiple_of(4);
    if plane0_stride_bytes < luma_row_bytes
        || plane1_stride_bytes < luma_row_bytes
        || ![
            plane0_stride_bytes,
            plane1_stride_bytes,
            plane1_offset_bytes,
        ]
        .iter()
        .all(|value| value.is_multiple_of(4))
    {
        return Err(Error::Configuration(format!(
            "color converter image→NV12: strides {plane0_stride_bytes} / {plane1_stride_bytes} \
             and chroma offset {plane1_offset_bytes} must be multiples of 4, each stride at \
             least {luma_row_bytes} for a {width}-wide frame"
        )));
    }
    let chroma_plane_end = u64::from(plane0_offset_bytes)
        + u64::from(plane1_offset_bytes)
        + u64::from(plane1_stride_bytes) * u64::from(height.div_ceil(2));
    let luma_plane_end =
        u64::from(plane0_offset_bytes) + u64::from(plane0_stride_bytes) * u64::from(height);
    let needed = chroma_plane_end.max(luma_plane_end);
    if dst.byte_size() < needed {
        return Err(Error::Configuration(format!(
            "color converter image→NV12: destination holds {} bytes but a {width}x{height} \
             frame laid out as {dst_layout:?} needs {needed}",
            dst.byte_size(),
        )));
    }
    Ok(())
}

fn validate_format_pair(src: PixelFormat, dst: PixelFormat) -> Result<()> {
    let decode_pair = matches!(
        src,
        PixelFormat::Nv12VideoRange
            | PixelFormat::Nv12FullRange
            | PixelFormat::Yuyv422
            | PixelFormat::Rgba32
            | PixelFormat::Bgra32
    ) && matches!(dst, PixelFormat::Rgba32);
    let encode_pair = matches!(src, PixelFormat::Rgba32 | PixelFormat::Bgra32)
        && matches!(
            dst,
            PixelFormat::Yuyv422 | PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange
        );
    if !decode_pair && !encode_pair {
        return Err(Error::NotSupported(format!(
            "color converter: unsupported format pair {:?} → {:?} (today: \
             {{NV12, YUYV, RGBA, BGRA}} → RGBA, and {{RGBA, BGRA}} → {{YUYV, NV12}})",
            src, dst
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod image_to_yuyv_buffer_tests;

#[cfg(test)]
mod image_to_nv12_buffer_tests;
