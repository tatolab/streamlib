// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
    pixel_format_color_kind, ColorConverterPushConstants, ComputeBindingSpec,
    ComputeKernelDescriptor, PixelFormat, SourceLayoutInfo, Texture,
    COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
};
use crate::core::{Error, Result};

use super::vulkan_storage_binding::VulkanStorageBindable;
use super::{HostVulkanDevice, VulkanComputeKernel};

/// Compute-shader workgroup tile size (one pixel per thread, 16×16
/// workgroups). Exposed so consumers driving the dispatch through
/// [`crate::vulkan::rhi::RhiCommandRecorder::record_dispatch`] can
/// compute `(group_x, group_y) = ⌈(width, height) / 16⌉`.
pub const COLOR_CONVERTER_WORKGROUP_SIZE: u32 = 16;

const BUFFER_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // YCbCr / RGB byte input
    ComputeBindingSpec::storage_image(1),  // RGBA output
];

/// Vulkan implementation of [`crate::core::rhi::RhiColorConverter`].
pub struct VulkanColorConverter {
    vulkan_device: Arc<HostVulkanDevice>,
    src_format: PixelFormat,
    dst_format: PixelFormat,
    buffer_to_image_kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
}

impl VulkanColorConverter {
    /// Build a converter for the given `(src, dst)` pair. Kernels are
    /// allocated lazily on first dispatch.
    pub fn new(
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
        })
    }

    pub fn src_format(&self) -> PixelFormat {
        self.src_format
    }

    pub fn dst_format(&self) -> PixelFormat {
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
    pub fn prepare_buffer_to_image<B: VulkanStorageBindable + ?Sized>(
        &self,
        src: &B,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let width = dst.width();
        let height = dst.height();

        let kernel = self.get_or_build_buffer_to_image_kernel()?;
        kernel.set_storage_buffer(0, src)?;
        kernel.set_storage_image(1, dst)?;
        let push = ColorConverterPushConstants::from_resolved(
            info,
            pixel_format_color_kind(self.src_format),
            dst_transfer,
            width,
            height,
            src_layout,
        );
        kernel.set_push_constants_value(&push)?;
        Ok(kernel)
    }

    /// Convert `src` into `dst` end-to-end. Builds (if needed),
    /// binds, and dispatches the buffer→image kernel via its own
    /// command buffer + fence + queue submit. Use this when there's
    /// no surrounding [`crate::vulkan::rhi::RhiCommandRecorder`];
    /// otherwise prefer [`Self::prepare_buffer_to_image`].
    pub fn convert_buffer_to_image<B: VulkanStorageBindable + ?Sized>(
        &self,
        src: &B,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        let kernel = self.prepare_buffer_to_image(src, src_layout, dst, info, TransferId::Srgb)?;
        let dispatch_x = dst.width().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        let dispatch_y = dst.height().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        kernel.dispatch(dispatch_x, dispatch_y, 1)
    }

    fn get_or_build_buffer_to_image_kernel(&self) -> Result<Arc<VulkanComputeKernel>> {
        let mut guard = self.buffer_to_image_kernel.lock();
        if let Some(k) = guard.as_ref() {
            return Ok(Arc::clone(k));
        }
        let kernel = Arc::new(self.build_buffer_to_image_kernel()?);
        *guard = Some(Arc::clone(&kernel));
        Ok(kernel)
    }

    fn build_buffer_to_image_kernel(&self) -> Result<VulkanComputeKernel> {
        let spv: &[u8] = match self.src_format {
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange => {
                include_bytes!(concat!(env!("OUT_DIR"), "/color_convert_nv12_buffer_to_rgba.spv"))
            }
            PixelFormat::Yuyv422 => {
                include_bytes!(concat!(env!("OUT_DIR"), "/color_convert_yuyv_buffer_to_rgba.spv"))
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
                label: label.as_str(),
                spv,
                bindings: BUFFER_TO_IMAGE_BINDINGS,
                push_constant_size: COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
            },
        )
    }
}

fn validate_format_pair(src: PixelFormat, dst: PixelFormat) -> Result<()> {
    let src_ok = matches!(
        src,
        PixelFormat::Nv12VideoRange
            | PixelFormat::Nv12FullRange
            | PixelFormat::Yuyv422
            | PixelFormat::Rgba32
            | PixelFormat::Bgra32
    );
    let dst_ok = matches!(dst, PixelFormat::Rgba32);
    if !src_ok || !dst_ok {
        return Err(Error::NotSupported(format!(
            "color converter: unsupported format pair {:?} → {:?} (today: \
             {{NV12, YUYV, RGBA, BGRA}} → RGBA only)",
            src, dst
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_device() -> Option<Arc<HostVulkanDevice>> {
        HostVulkanDevice::new().ok()
    }

    /// Construction succeeds for every supported `(src, dst)` pair —
    /// no SPIR-V is loaded yet (that's lazy on first dispatch), but the
    /// format-pair validation runs.
    #[test]
    fn new_succeeds_for_every_supported_pair() {
        let Some(device) = try_device() else { return };
        for src in [
            PixelFormat::Nv12VideoRange,
            PixelFormat::Nv12FullRange,
            PixelFormat::Yuyv422,
        ] {
            let conv = VulkanColorConverter::new(&device, src, PixelFormat::Rgba32)
                .expect("converter construction must succeed");
            assert_eq!(conv.src_format(), src);
            assert_eq!(conv.dst_format(), PixelFormat::Rgba32);
        }
    }

    /// Unsupported destination format is rejected at construction
    /// time, not at first dispatch.
    #[test]
    fn new_rejects_unsupported_dst_format() {
        let Some(device) = try_device() else { return };
        let err = VulkanColorConverter::new(&device, PixelFormat::Nv12FullRange, PixelFormat::Bgra32)
            .err()
            .expect("must reject Bgra32 dest today");
        assert!(matches!(err, Error::NotSupported(_)));
    }
}
