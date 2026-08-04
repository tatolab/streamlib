// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Present-composition primitive: draws a source texture onto a presentation
//! attachment with aspect-managed scaling and black bars.
//!
//! Absorbs the draw step display consumers previously assembled by hand
//! (blit kernel + descriptor staging + barrier + fullscreen-triangle draw)
//! into one call per frame. Colorspace selection and HDR metadata stay with
//! [`VulkanPresentTarget`](super::VulkanPresentTarget) — the compositor's
//! kernel is (re)built against whatever attachment format that pick yields.

use std::sync::Arc;

use crate::core::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    PrimitiveTopology, RasterizationState, ScissorRect, Texture, TextureFormat, VertexInputState,
    Viewport, VulkanLayout,
};
use crate::core::{Error, Result};

use super::vulkan_command_recorder::RhiCommandRecorder;
use super::vulkan_device::HostVulkanDevice;
use super::vulkan_graphics_kernel::{OffscreenColorTarget, OffscreenDraw, VulkanGraphicsKernel};
use super::vulkan_pipeline_flags::{VulkanAccess, VulkanStage};
use super::vulkan_present_target::{MAX_FRAMES_IN_FLIGHT, PresentFrame};

/// How a source rectangle maps onto a destination of a different aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentScalingMode {
    /// Preserve aspect; the whole source is visible, black bars fill the rest.
    Fit,
    /// Preserve aspect; the destination is covered, source overflow is cropped.
    Fill,
    /// Ignore aspect; the source is stretched to the destination exactly.
    Stretch,
}

/// Per-axis UV scale for the display-blit fragment shader's
/// `(uv - 0.5) / scale + 0.5` sampling (out-of-range samples render black).
pub fn composition_scale(
    mode: PresentScalingMode,
    source_extent: (u32, u32),
    destination_extent: (u32, u32),
) -> (f32, f32) {
    let source_aspect = source_extent.0 as f32 / source_extent.1 as f32;
    let destination_aspect = destination_extent.0 as f32 / destination_extent.1 as f32;
    match mode {
        PresentScalingMode::Stretch => (1.0, 1.0),
        PresentScalingMode::Fit => {
            if source_aspect > destination_aspect {
                (1.0, destination_aspect / source_aspect)
            } else {
                (source_aspect / destination_aspect, 1.0)
            }
        }
        PresentScalingMode::Fill => {
            if source_aspect > destination_aspect {
                (source_aspect / destination_aspect, 1.0)
            } else {
                (1.0, destination_aspect / source_aspect)
            }
        }
    }
}

/// Owns the display-blit graphics kernel for one attachment format and
/// records "draw this texture onto that attachment" as a single call.
pub struct VulkanPresentCompositor {
    kernel: VulkanGraphicsKernel,
    attachment_format: TextureFormat,
}

const DISPLAY_BLIT_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.vert.spv"));
const DISPLAY_BLIT_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.frag.spv"));

/// Push-constant block of `display_blit.frag`: `vec2 scale` + `vec2 offset`.
#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayBlitPushConstants {
    scale: [f32; 2],
    offset: [f32; 2],
}

fn build_display_blit_kernel(
    vulkan_device: &Arc<HostVulkanDevice>,
    attachment_format: TextureFormat,
) -> Result<VulkanGraphicsKernel> {
    let stages = [
        GraphicsStage::vertex(DISPLAY_BLIT_VERT_SPV),
        GraphicsStage::fragment(DISPLAY_BLIT_FRAG_SPV),
    ];
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let descriptor = GraphicsKernelDescriptor {
        label: "present-compositor-display-blit",
        stages: &stages,
        bindings: &bindings,
        push_constants: GraphicsPushConstants {
            size: std::mem::size_of::<DisplayBlitPushConstants>() as u32,
            stages: GraphicsShaderStageFlags::FRAGMENT,
        },
        pipeline_state: GraphicsPipelineState {
            topology: PrimitiveTopology::TriangleList,
            vertex_input: VertexInputState::None,
            rasterization: RasterizationState::default(),
            multisample: MultisampleState::default(),
            depth_stencil: DepthStencilState::Disabled,
            color_blend: ColorBlendState::Disabled {
                color_write_mask: ColorWriteMask::RGBA,
            },
            attachment_formats: AttachmentFormats::color_only(attachment_format),
            dynamic_state: GraphicsDynamicState::ViewportScissor,
        },
        descriptor_sets_in_flight: MAX_FRAMES_IN_FLIGHT as u32,
    };
    VulkanGraphicsKernel::new(vulkan_device, &descriptor)
}

impl VulkanPresentCompositor {
    /// Build the compositor's kernel against `attachment_format` (the
    /// present target's [`color_format`](super::VulkanPresentTarget::color_format),
    /// or an offscreen target's format).
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        attachment_format: TextureFormat,
    ) -> Result<Self> {
        Ok(Self {
            kernel: build_display_blit_kernel(vulkan_device, attachment_format)?,
            attachment_format,
        })
    }

    /// The attachment format the current kernel was built against.
    pub fn attachment_format(&self) -> TextureFormat {
        self.attachment_format
    }

    /// Rebuild the kernel if `attachment_format` differs from the current one
    /// (a swapchain recreate can flip SDR BGRA8 → HDR10 A2B10G10R10). Returns
    /// whether a rebuild happened.
    pub fn ensure_attachment_format(
        &mut self,
        vulkan_device: &Arc<HostVulkanDevice>,
        attachment_format: TextureFormat,
    ) -> Result<bool> {
        if attachment_format == self.attachment_format {
            return Ok(false);
        }
        self.kernel = build_display_blit_kernel(vulkan_device, attachment_format)?;
        self.attachment_format = attachment_format;
        Ok(true)
    }

    /// Record the composition of `source` onto an acquired swapchain frame.
    ///
    /// Transitions `source` to `SHADER_READ_ONLY_OPTIMAL` when
    /// `source_current_layout` says it is not there already — the caller's
    /// layout bookkeeping must record that transition. Opens and closes its
    /// own dynamic-rendering pass (clearing to black), so call it once per
    /// frame with no pass active.
    pub fn compose_to_present_frame(
        &self,
        frame: &mut PresentFrame<'_>,
        source: &Texture,
        source_current_layout: VulkanLayout,
        scaling: PresentScalingMode,
    ) -> Result<()> {
        if frame.color_format != self.attachment_format {
            return Err(Error::GpuError(format!(
                "VulkanPresentCompositor: kernel was built for {:?} but the acquired \
                 frame's attachment is {:?} — call ensure_attachment_format after a \
                 swapchain recreate",
                self.attachment_format, frame.color_format
            )));
        }
        let frame_index = frame.frame_index;
        let destination_extent = frame.extent;

        self.kernel.set_sampled_texture(frame_index, 0, source)?;
        if source_current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
            frame.recorder.record_image_barrier(
                source,
                source_current_layout,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::FRAGMENT_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
        }

        let (scale_x, scale_y) = composition_scale(
            scaling,
            (source.width(), source.height()),
            destination_extent,
        );
        self.kernel.set_push_constants_value(
            frame_index,
            &DisplayBlitPushConstants {
                scale: [scale_x, scale_y],
                offset: [0.0, 0.0],
            },
        )?;

        frame.begin_rendering(Some([0.0, 0.0, 0.0, 1.0]))?;
        let draw = DrawCall {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
            viewport: Some(Viewport::full(destination_extent.0, destination_extent.1)),
            scissor: Some(ScissorRect::full(destination_extent.0, destination_extent.1)),
        };
        frame.recorder.record_draw(&self.kernel, frame_index, &draw)?;
        frame.end_rendering()
    }

    /// Compose `source` into `destination` without a window: the offscreen
    /// arm of the same draw, used by headless callers and the composition
    /// correctness tests. Submits and waits; `destination` is left in
    /// `COLOR_ATTACHMENT_OPTIMAL`, `source` in `SHADER_READ_ONLY_OPTIMAL`.
    pub fn compose_to_offscreen_texture(
        &self,
        vulkan_device: &Arc<HostVulkanDevice>,
        frame_index: u32,
        destination: &Texture,
        source: &Texture,
        source_current_layout: VulkanLayout,
        scaling: PresentScalingMode,
    ) -> Result<()> {
        if destination.format() != self.attachment_format {
            return Err(Error::GpuError(format!(
                "VulkanPresentCompositor: kernel was built for {:?} but the offscreen \
                 destination is {:?} — call ensure_attachment_format first",
                self.attachment_format,
                destination.format()
            )));
        }
        if source_current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
            let mut recorder = RhiCommandRecorder::new(
                vulkan_device,
                "present-compositor-offscreen-source-barrier",
            )?;
            recorder.begin()?;
            recorder.record_image_barrier(
                source,
                source_current_layout,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::FRAGMENT_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.submit_and_wait()?;
        }

        self.kernel.set_sampled_texture(frame_index, 0, source)?;
        let (scale_x, scale_y) = composition_scale(
            scaling,
            (source.width(), source.height()),
            (destination.width(), destination.height()),
        );
        self.kernel.set_push_constants_value(
            frame_index,
            &DisplayBlitPushConstants {
                scale: [scale_x, scale_y],
                offset: [0.0, 0.0],
            },
        )?;

        self.kernel.offscreen_render(
            frame_index,
            &[OffscreenColorTarget {
                texture: destination,
                clear_color: Some([0.0, 0.0, 0.0, 1.0]),
            }],
            (destination.width(), destination.height()),
            OffscreenDraw::Draw(DrawCall {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
                viewport: None,
                scissor: None,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- composition_scale: pure math, no GPU --------------------------------

    /// Wide 16:9 source into a narrower 4:3 destination.
    const SRC_16_9: (u32, u32) = (1920, 1080);
    const DST_4_3: (u32, u32) = (1024, 768);

    #[test]
    fn stretch_ignores_aspect() {
        assert_eq!(
            composition_scale(PresentScalingMode::Stretch, SRC_16_9, DST_4_3),
            (1.0, 1.0)
        );
    }

    #[test]
    fn fit_letterboxes_wide_source_in_narrow_destination() {
        let (sx, sy) = composition_scale(PresentScalingMode::Fit, SRC_16_9, DST_4_3);
        assert_eq!(sx, 1.0, "full width is used");
        let expected = (4.0 / 3.0) / (16.0 / 9.0);
        assert!((sy - expected).abs() < 1e-6, "got {sy}, want {expected}");
        assert!(sy < 1.0, "vertical shrink → black bars top and bottom");
    }

    #[test]
    fn fit_pillarboxes_narrow_source_in_wide_destination() {
        let (sx, sy) = composition_scale(PresentScalingMode::Fit, DST_4_3, SRC_16_9);
        assert_eq!(sy, 1.0, "full height is used");
        assert!(sx < 1.0, "horizontal shrink → black bars left and right");
    }

    #[test]
    fn fill_crops_wide_source_in_narrow_destination() {
        let (sx, sy) = composition_scale(PresentScalingMode::Fill, SRC_16_9, DST_4_3);
        assert_eq!(sy, 1.0);
        assert!(sx > 1.0, "horizontal magnify → left/right edges cropped");
    }

    #[test]
    fn fill_crops_narrow_source_in_wide_destination() {
        let (sx, sy) = composition_scale(PresentScalingMode::Fill, DST_4_3, SRC_16_9);
        assert_eq!(sx, 1.0);
        assert!(sy > 1.0, "vertical magnify → top/bottom edges cropped");
    }

    #[test]
    fn matched_aspect_is_identity_in_every_mode() {
        for mode in [
            PresentScalingMode::Fit,
            PresentScalingMode::Fill,
            PresentScalingMode::Stretch,
        ] {
            assert_eq!(
                composition_scale(mode, (1920, 1080), (1280, 720)),
                (1.0, 1.0),
                "{mode:?}"
            );
        }
    }

    // ---- GPU tests -----------------------------------------------------------

    use crate::core::rhi::{TextureDescriptor, TextureSourceLayout, TextureUsages};
    use crate::host_rhi::HostTextureExt;
    use crate::vulkan::rhi::{HostVulkanBuffer, HostVulkanTexture, VulkanTextureReadback};

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    #[test]
    fn constructs_for_bgra8_attachment() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let compositor = VulkanPresentCompositor::new(&device, TextureFormat::Bgra8Unorm)
            .expect("compositor must construct");
        assert_eq!(compositor.attachment_format(), TextureFormat::Bgra8Unorm);
    }

    #[test]
    fn ensure_attachment_format_rebuilds_only_on_change() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let mut compositor =
            VulkanPresentCompositor::new(&device, TextureFormat::Bgra8Unorm).expect("compositor");
        assert!(
            !compositor
                .ensure_attachment_format(&device, TextureFormat::Bgra8Unorm)
                .expect("same format"),
            "same format must not rebuild"
        );
        assert!(
            compositor
                .ensure_attachment_format(&device, TextureFormat::Rgba8Unorm)
                .expect("new format"),
            "format flip must rebuild"
        );
        assert_eq!(compositor.attachment_format(), TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn offscreen_destination_format_mismatch_is_rejected() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let compositor =
            VulkanPresentCompositor::new(&device, TextureFormat::Bgra8Unorm).expect("compositor");
        let destination = make_solid_texture(&device, 64, 64, TextureFormat::Rgba8Unorm, [0; 4]);
        let source = make_solid_texture(&device, 64, 64, TextureFormat::Bgra8Unorm, [255; 4]);
        let err = compositor
            .compose_to_offscreen_texture(
                &device,
                0,
                &destination,
                &source,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                PresentScalingMode::Stretch,
            )
            .expect_err("format mismatch must be rejected");
        assert!(
            format!("{err}").contains("ensure_attachment_format"),
            "error names the fix: {err}"
        );
    }

    /// Allocate a sampled+attachment-capable texture and fill it with one
    /// solid color via a staged copy, leaving it in
    /// `SHADER_READ_ONLY_OPTIMAL`.
    fn make_solid_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        format: TextureFormat,
        color: [u8; 4],
    ) -> Texture {
        let descriptor = TextureDescriptor {
            width,
            height,
            format,
            usage: TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::RENDER_ATTACHMENT,
            label: Some("present-compositor-test-texture"),
        };
        let host_texture = HostVulkanTexture::new(device, &descriptor).expect("texture");
        let texture = <Texture as HostTextureExt>::from_vulkan(host_texture);

        let byte_count = (width as u64) * (height as u64) * 4;
        let staging = HostVulkanBuffer::new(device, byte_count).expect("staging");
        unsafe {
            let mut mapped = staging.mapped_ptr();
            for _ in 0..(width * height) {
                std::ptr::copy_nonoverlapping(color.as_ptr(), mapped, 4);
                mapped = mapped.add(4);
            }
        }

        let mut recorder =
            RhiCommandRecorder::new(device, "present-compositor-test-upload").expect("recorder");
        recorder.begin().expect("begin");
        recorder
            .record_image_barrier(
                &texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COPY,
                VulkanAccess::NONE,
                VulkanAccess::TRANSFER_WRITE,
            )
            .expect("to transfer-dst");
        recorder
            .record_copy_buffer_to_image(
                &staging,
                &texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                super::super::vulkan_command_recorder::ImageCopyRegion::tightly_packed(
                    width, height,
                ),
            )
            .expect("copy");
        recorder
            .record_image_barrier(
                &texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                VulkanStage::COPY,
                VulkanStage::FRAGMENT_SHADER,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )
            .expect("to shader-read");
        recorder.submit_and_wait().expect("upload submit");
        texture
    }

    /// Read `destination` back and return its BGRA bytes.
    fn read_back(device: &Arc<HostVulkanDevice>, destination: &Texture) -> Vec<u8> {
        let readback = VulkanTextureReadback::new(
            device,
            &crate::core::rhi::TextureReadbackDescriptor {
                label: "present-compositor-test-readback",
                format: destination.format(),
                width: destination.width(),
                height: destination.height(),
            },
        )
        .expect("readback");
        let ticket = readback
            .submit(destination, TextureSourceLayout::ColorAttachment)
            .expect("readback submit");
        readback
            .wait_and_read(ticket, u64::MAX)
            .expect("readback wait")
            .to_vec()
    }

    fn pixel_at(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn fit_composition_letterboxes_with_black_bars() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        // White 2:1 source into a square destination: Fit shrinks vertically —
        // white band across the middle, black bars top and bottom.
        let source = make_solid_texture(
            &device,
            128,
            64,
            TextureFormat::Bgra8Unorm,
            [255, 255, 255, 255],
        );
        let destination =
            make_solid_texture(&device, 64, 64, TextureFormat::Bgra8Unorm, [0, 0, 0, 255]);
        let compositor =
            VulkanPresentCompositor::new(&device, TextureFormat::Bgra8Unorm).expect("compositor");
        compositor
            .compose_to_offscreen_texture(
                &device,
                0,
                &destination,
                &source,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                PresentScalingMode::Fit,
            )
            .expect("compose");

        let bytes = read_back(&device, &destination);
        // Bars: y ∈ [0, 16) and [48, 64). Content: y ∈ (16, 48).
        assert_eq!(
            pixel_at(&bytes, 64, 32, 4),
            [0, 0, 0, 255],
            "top bar is black"
        );
        assert_eq!(
            pixel_at(&bytes, 64, 32, 60),
            [0, 0, 0, 255],
            "bottom bar is black"
        );
        assert_eq!(
            pixel_at(&bytes, 64, 32, 32),
            [255, 255, 255, 255],
            "center is source content"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn fill_composition_covers_the_destination() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let source = make_solid_texture(
            &device,
            128,
            64,
            TextureFormat::Bgra8Unorm,
            [0, 0, 255, 255],
        );
        let destination =
            make_solid_texture(&device, 64, 64, TextureFormat::Bgra8Unorm, [0, 0, 0, 255]);
        let compositor =
            VulkanPresentCompositor::new(&device, TextureFormat::Bgra8Unorm).expect("compositor");
        compositor
            .compose_to_offscreen_texture(
                &device,
                0,
                &destination,
                &source,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                PresentScalingMode::Fill,
            )
            .expect("compose");

        let bytes = read_back(&device, &destination);
        for (x, y) in [(0, 0), (63, 0), (0, 63), (63, 63), (32, 32)] {
            assert_eq!(
                pixel_at(&bytes, 64, x, y),
                [0, 0, 255, 255],
                "({x},{y}) is covered by source content — no bars in Fill"
            );
        }
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn stretch_composition_fills_regardless_of_aspect() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let source = make_solid_texture(
            &device,
            128,
            32,
            TextureFormat::Bgra8Unorm,
            [0, 255, 0, 255],
        );
        let destination =
            make_solid_texture(&device, 64, 64, TextureFormat::Bgra8Unorm, [0, 0, 0, 255]);
        let compositor =
            VulkanPresentCompositor::new(&device, TextureFormat::Bgra8Unorm).expect("compositor");
        compositor
            .compose_to_offscreen_texture(
                &device,
                0,
                &destination,
                &source,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                PresentScalingMode::Stretch,
            )
            .expect("compose");

        let bytes = read_back(&device, &destination);
        for (x, y) in [(0, 0), (63, 63), (32, 32)] {
            assert_eq!(
                pixel_at(&bytes, 64, x, y),
                [0, 255, 0, 255],
                "({x},{y}) is stretched source content"
            );
        }
    }
}
