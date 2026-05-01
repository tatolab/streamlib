// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 4-layer alpha-over compositor via [`VulkanComputeKernel`].
//!
//! Linux counterpart to the macOS Metal kernel at
//! `examples/camera-python-display/src/shaders/blending_compositor.metal`.

use std::sync::Arc;

use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor, RhiPixelBuffer};
use crate::core::{Result, StreamError};

use super::{HostVulkanDevice, VulkanComputeKernel};

/// Push-constants layout matching `blending_compositor.comp`'s
/// `layout(push_constant)` block.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlendingCompositorPushConstants {
    pub width: u32,
    pub height: u32,
    pub pip_width: u32,
    pub pip_height: u32,
    pub flags: u32,
    pub pip_slide_progress: f32,
}

/// `flags` bit positions for [`BlendingCompositorPushConstants`].
pub mod flag_bits {
    pub const HAS_VIDEO: u32 = 1 << 0;
    pub const HAS_LOWER_THIRD: u32 = 1 << 1;
    pub const HAS_WATERMARK: u32 = 1 << 2;
    pub const HAS_PIP: u32 = 1 << 3;
}

const BLENDING_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // video
    ComputeBindingSpec::storage_buffer(1), // lower_third
    ComputeBindingSpec::storage_buffer(2), // watermark
    ComputeBindingSpec::storage_buffer(3), // pip
    ComputeBindingSpec::storage_buffer(4), // output
];

const WORKGROUP_SIZE: u32 = 16;

/// Inputs for one compositor dispatch. All buffers are BGRA8 packed
/// little-endian.
///
/// **Layer-size contract.** `video`, `lower_third`, and `watermark`
/// must match `output`'s dimensions exactly — the GLSL kernel samples
/// them at integer pixel coordinates with no resampling, so a size
/// mismatch is rejected at dispatch time with [`StreamError::GpuError`].
/// `pip` may be any size; it is bilinearly sampled inside the PiP
/// rect. This is stricter than the macOS Metal version (which silently
/// resamples mismatched layers via the linear sampler); when porting
/// upstream overlay processors to Linux, ensure they emit at the
/// camera resolution.
pub struct BlendingCompositorInputs<'a> {
    pub video: Option<&'a RhiPixelBuffer>,
    pub lower_third: Option<&'a RhiPixelBuffer>,
    pub watermark: Option<&'a RhiPixelBuffer>,
    pub pip: Option<&'a RhiPixelBuffer>,
    pub output: &'a RhiPixelBuffer,
    pub pip_slide_progress: f32,
}

/// 4-layer Porter-Duff "over" compositor with animated PiP frame.
pub struct VulkanBlendingCompositor {
    kernel: VulkanComputeKernel,
    /// 1×1 transparent BGRA buffer used as a placeholder for any input
    /// that the caller leaves unbound — descriptor sets must be fully
    /// populated even when the corresponding `has_*` flag is false.
    placeholder: RhiPixelBuffer,
}

impl VulkanBlendingCompositor {
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let spv = include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.spv"));
        let kernel = VulkanComputeKernel::new(
            vulkan_device,
            &ComputeKernelDescriptor {
                label: "blending_compositor",
                spv,
                bindings: BLENDING_BINDINGS,
                push_constant_size: std::mem::size_of::<BlendingCompositorPushConstants>() as u32,
            },
        )?;

        let placeholder = make_placeholder(vulkan_device)?;

        Ok(Self { kernel, placeholder })
    }

    /// Composite `inputs` into `inputs.output`. The output's dimensions
    /// drive the dispatch size; missing layer inputs short-circuit via
    /// the `has_*` flags inside the shader.
    pub fn dispatch(&self, inputs: BlendingCompositorInputs<'_>) -> Result<()> {
        let width = inputs.output.width;
        let height = inputs.output.height;

        let mut flags = 0u32;
        if inputs.video.is_some()        { flags |= flag_bits::HAS_VIDEO; }
        if inputs.lower_third.is_some()  { flags |= flag_bits::HAS_LOWER_THIRD; }
        if inputs.watermark.is_some()    { flags |= flag_bits::HAS_WATERMARK; }
        if inputs.pip.is_some()          { flags |= flag_bits::HAS_PIP; }

        let video       = inputs.video.unwrap_or(&self.placeholder);
        let lower_third = inputs.lower_third.unwrap_or(&self.placeholder);
        let watermark   = inputs.watermark.unwrap_or(&self.placeholder);
        let pip         = inputs.pip.unwrap_or(&self.placeholder);

        // Screen-aligned layers must match the output's dimensions — the
        // shader assumes 1:1 pixel alignment for layers 0..2 (mirroring
        // the Metal version, which samples them at screen UV).
        if let Some(video) = inputs.video {
            check_match("video", video, width, height)?;
        }
        if let Some(lt) = inputs.lower_third {
            check_match("lower_third", lt, width, height)?;
        }
        if let Some(wm) = inputs.watermark {
            check_match("watermark", wm, width, height)?;
        }

        let push = BlendingCompositorPushConstants {
            width,
            height,
            pip_width: pip.width,
            pip_height: pip.height,
            flags,
            pip_slide_progress: inputs.pip_slide_progress.clamp(0.0, 1.0),
        };

        self.kernel.set_storage_buffer(0, video)?;
        self.kernel.set_storage_buffer(1, lower_third)?;
        self.kernel.set_storage_buffer(2, watermark)?;
        self.kernel.set_storage_buffer(3, pip)?;
        self.kernel.set_storage_buffer(4, inputs.output)?;
        self.kernel.set_push_constants_value(&push)?;

        let dispatch_x = width.div_ceil(WORKGROUP_SIZE);
        let dispatch_y = height.div_ceil(WORKGROUP_SIZE);
        self.kernel.dispatch(dispatch_x, dispatch_y, 1)
    }
}

fn check_match(name: &str, buf: &RhiPixelBuffer, w: u32, h: u32) -> Result<()> {
    if buf.width != w || buf.height != h {
        return Err(StreamError::GpuError(format!(
            "BlendingCompositor: {name} input is {}×{}, expected {w}×{h} (must match output)",
            buf.width, buf.height
        )));
    }
    Ok(())
}

fn make_placeholder(vulkan_device: &Arc<HostVulkanDevice>) -> Result<RhiPixelBuffer> {
    use crate::core::rhi::{PixelFormat, RhiPixelBufferRef};
    use crate::vulkan::rhi::HostVulkanPixelBuffer;

    let buf = HostVulkanPixelBuffer::new(vulkan_device, 1, 1, 4, PixelFormat::Bgra32)?;
    // Zero out so any unbound layer reads as fully transparent.
    unsafe {
        std::ptr::write_bytes(buf.mapped_ptr(), 0, 4);
    }
    Ok(RhiPixelBuffer::new(RhiPixelBufferRef {
        inner: Arc::new(buf),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{PixelFormat, RhiPixelBufferRef};
    use crate::vulkan::rhi::HostVulkanPixelBuffer;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(Arc::new(d)),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    fn make_buf(device: &Arc<HostVulkanDevice>, w: u32, h: u32) -> RhiPixelBuffer {
        let vk = HostVulkanPixelBuffer::new(device, w, h, 4, PixelFormat::Bgra32)
            .expect("pixel buffer");
        RhiPixelBuffer::new(RhiPixelBufferRef {
            inner: Arc::new(vk),
        })
    }

    fn fill(buf: &RhiPixelBuffer, b: u8, g: u8, r: u8, a: u8) {
        let pixel = (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24);
        let count = (buf.width * buf.height) as usize;
        unsafe {
            let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
            for i in 0..count {
                *ptr.add(i) = pixel;
            }
        }
    }

    fn read_pixel(buf: &RhiPixelBuffer, x: u32, y: u32) -> (u8, u8, u8, u8) {
        unsafe {
            let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
            let p = *ptr.add((y * buf.width + x) as usize);
            (
                (p & 0xFF) as u8,
                ((p >> 8) & 0xFF) as u8,
                ((p >> 16) & 0xFF) as u8,
                ((p >> 24) & 0xFF) as u8,
            )
        }
    }

    #[test]
    fn new_compiles_kernel() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let result = VulkanBlendingCompositor::new(&device);
        assert!(result.is_ok(), "compositor creation must succeed: {:?}", result.err());
    }

    #[test]
    fn output_matches_video_when_only_video_bound() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let compositor = VulkanBlendingCompositor::new(&device).expect("compositor");

        let video = make_buf(&device, 64, 32);
        let out = make_buf(&device, 64, 32);
        // BGRA = (10, 200, 50, 255) → opaque green-ish.
        fill(&video, 10, 200, 50, 255);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(&video),
                lower_third: None,
                watermark: None,
                pip: None,
                output: &out,
                pip_slide_progress: 0.0,
            })
            .expect("dispatch");

        // Sample a center pixel — should round-trip the input bytes.
        let (b, g, r, a) = read_pixel(&out, 16, 16);
        // ±1 tolerance per channel for float→u8 rounding paths.
        assert!((b as i32 - 10).abs() <= 1, "B={b}");
        assert!((g as i32 - 200).abs() <= 1, "G={g}");
        assert!((r as i32 - 50).abs() <= 1, "R={r}");
        assert!((a as i32 - 255).abs() <= 1, "A={a}");
    }

    #[test]
    fn no_video_falls_back_to_dark_blue() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let compositor = VulkanBlendingCompositor::new(&device).expect("compositor");

        let out = make_buf(&device, 32, 32);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: None,
                lower_third: None,
                watermark: None,
                pip: None,
                output: &out,
                pip_slide_progress: 0.0,
            })
            .expect("dispatch");

        // Shader's no-video fallback is vec4(0.05, 0.05, 0.12, 1.0) →
        // BGRA roughly (31, 13, 13, 255). PiP and overlay flags are off,
        // so the entire frame should be the fallback.
        let (b, g, r, a) = read_pixel(&out, 8, 8);
        let expected_b = (0.12_f32 * 255.0).round() as i32; // 31
        let expected_g = (0.05_f32 * 255.0).round() as i32; // 13
        let expected_r = (0.05_f32 * 255.0).round() as i32; // 13
        assert!((b as i32 - expected_b).abs() <= 1, "B={b}");
        assert!((g as i32 - expected_g).abs() <= 1, "G={g}");
        assert!((r as i32 - expected_r).abs() <= 1, "R={r}");
        assert_eq!(a, 255, "alpha must be opaque on fallback");
    }

    #[test]
    fn rejects_layer_size_mismatch() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let compositor = VulkanBlendingCompositor::new(&device).expect("compositor");
        let video = make_buf(&device, 32, 32);
        let out = make_buf(&device, 64, 32);

        let err = compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(&video),
                lower_third: None,
                watermark: None,
                pip: None,
                output: &out,
                pip_slide_progress: 0.0,
            })
            .expect_err("size mismatch must error");
        assert!(matches!(err, StreamError::GpuError(_)));
    }
}
