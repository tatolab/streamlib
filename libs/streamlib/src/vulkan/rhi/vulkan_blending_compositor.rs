// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 4-layer alpha-over compositor on the canonical graphics-kernel RHI.
//!
//! Reads four screen-aligned input layers (`video` + `lower_third` +
//! `watermark` + `pip`) as `sampled_texture` bindings and renders a
//! single composited frame into a render-target [`StreamTexture`] via
//! [`VulkanGraphicsKernel`] — a full-screen-triangle + fragment-shader
//! pass with the cyberpunk N54 News PiP chrome baked into the
//! fragment.
//!
//! The pre-RHI version of this compositor stored every layer in a
//! linear DMA-BUF `RhiPixelBuffer` and bound them as
//! `storage_buffer`s; the fragment shader did manual `unpack_bgra` /
//! `pack_bgra` byte arithmetic and a hand-rolled bilinear PiP sampler.
//! That shape predated the graphics-kernel RHI and could not consume
//! the tiled DMA-BUF `VkImage`s every modern producer in the codebase
//! emits (camera ring, OpenGL/Skia/Vulkan adapter outputs). This
//! module replaces it end-to-end with the graphics-kernel pattern:
//! hardware sampler units handle tiled-format access, hardware
//! bilinear filtering replaces the manual PiP sampler, and the output
//! is a real render-target VkImage that downstream consumers
//! (`LinuxDisplayProcessor`, future encoders) resolve through the
//! standard [`crate::core::context::TextureRegistration`] path.
//!
//! Lifecycle: caller pre-allocates a ring of output `StreamTexture`s
//! (typically `MAX_FRAMES_IN_FLIGHT = 2`), hands one to
//! [`VulkanBlendingCompositor::dispatch`] per frame along with the
//! four layer textures + their current Vulkan layouts, and
//! [`dispatch`] returns once the GPU has signaled the kernel's
//! per-render fence — same synchronous shape as
//! [`super::VulkanComputeKernel::dispatch`] and
//! [`super::VulkanGraphicsKernel::offscreen_render`]. After return,
//! every input texture and the output texture are left in
//! `SHADER_READ_ONLY_OPTIMAL`, ready for the next consumer to sample
//! without re-barriering.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    PrimitiveTopology, RasterizationState, ScissorRect, StreamTexture, TextureDescriptor,
    TextureFormat, TextureUsages, VertexInputState, Viewport, VulkanLayout,
};
use crate::core::{Result, StreamError};

use super::{HostVulkanDevice, VulkanGraphicsKernel};

/// Push-constants layout — must match `blending_compositor.frag`'s
/// `layout(push_constant)` block byte-for-byte.
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

/// One layer input: texture + the layout it is currently in. The
/// compositor barriers from `current_layout` to
/// `SHADER_READ_ONLY_OPTIMAL` before the draw and leaves the texture
/// in that layout afterward.
#[derive(Clone, Copy)]
pub struct BlendingLayer<'a> {
    pub texture: &'a StreamTexture,
    pub current_layout: VulkanLayout,
}

/// Render target for one composited frame: caller-owned ring slot +
/// the layout it was last left in (typically
/// `SHADER_READ_ONLY_OPTIMAL` from the prior consumer's barrier, or
/// `UNDEFINED` on the first dispatch into this slot).
#[derive(Clone, Copy)]
pub struct BlendingOutput<'a> {
    pub texture: &'a StreamTexture,
    pub current_layout: VulkanLayout,
}

/// Inputs for one compositor dispatch.
///
/// **Layer-size contract.** `video`, `lower_third`, and `watermark`
/// must match `output`'s dimensions exactly — the fragment shader
/// samples them at the same screen UV with no resampling, so a size
/// mismatch is rejected at dispatch time. `pip` may be any size; it is
/// sampled bilinearly inside the PiP rect via the kernel's default
/// linear sampler.
pub struct BlendingCompositorInputs<'a> {
    pub video: Option<BlendingLayer<'a>>,
    pub lower_third: Option<BlendingLayer<'a>>,
    pub watermark: Option<BlendingLayer<'a>>,
    pub pip: Option<BlendingLayer<'a>>,
    pub output: BlendingOutput<'a>,
    pub pip_slide_progress: f32,
}

/// 4-layer Porter-Duff "over" compositor with animated PiP frame chrome.
pub struct VulkanBlendingCompositor {
    label: &'static str,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    kernel: VulkanGraphicsKernel,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// 1×1 transparent BGRA texture used for any unbound layer slot —
    /// graphics-kernel descriptor sets must be fully populated even
    /// when the corresponding `has_*` flag is false. Pre-uploaded once
    /// at construction; ends in `SHADER_READ_ONLY_OPTIMAL`.
    placeholder: StreamTexture,
}

impl VulkanBlendingCompositor {
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let label = "blending_compositor";

        let vert =
            include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.vert.spv"));
        let frag =
            include_bytes!(concat!(env!("OUT_DIR"), "/blending_compositor.frag.spv"));

        let stages = [GraphicsStage::vertex(vert), GraphicsStage::fragment(frag)];
        let bindings = [
            GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(1, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(2, GraphicsShaderStageFlags::FRAGMENT),
            GraphicsBindingSpec::sampled_texture(3, GraphicsShaderStageFlags::FRAGMENT),
        ];
        let descriptor = GraphicsKernelDescriptor {
            label,
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants {
                size: std::mem::size_of::<BlendingCompositorPushConstants>() as u32,
                stages: GraphicsShaderStageFlags::FRAGMENT,
            },
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleList,
                vertex_input: VertexInputState::None,
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Disabled,
                // Fragment shader does manual Porter-Duff alpha-over; no
                // hardware blend.
                color_blend: ColorBlendState::Disabled {
                    color_write_mask: ColorWriteMask::RGBA,
                },
                attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            // Synchronous dispatch (every call submits + waits) — one
            // descriptor set is enough; mirrors `VulkanComputeKernel`'s
            // shape.
            descriptor_sets_in_flight: 1,
        };
        let kernel = VulkanGraphicsKernel::new(vulkan_device, &descriptor)?;

        let device = vulkan_device.device().clone();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();

        let command_pool = create_command_pool(&device, queue_family_index)?;
        let command_buffer = allocate_command_buffer(&device, command_pool)?;
        let fence = create_signaled_fence(&device)?;

        // 1×1 transparent BGRA placeholder — the descriptor set must
        // bind a real image for every sampled_texture binding even
        // when the corresponding `has_*` flag is off. The fragment
        // shader gates the actual sample via the flag, so the
        // placeholder is never read; it just keeps the descriptor
        // legal.
        let placeholder = make_placeholder_texture(vulkan_device)?;

        Ok(Self {
            label,
            vulkan_device: Arc::clone(vulkan_device),
            device,
            queue,
            kernel,
            command_pool,
            command_buffer,
            fence,
            placeholder,
        })
    }

    /// Composite `inputs` into `inputs.output` and signal completion.
    ///
    /// Records (input barriers → begin_rendering → bind+draw →
    /// end_rendering → output barrier) into a single command buffer
    /// owned by this compositor, submits, and waits for the fence
    /// before returning. After return, every input texture and the
    /// output texture are in `SHADER_READ_ONLY_OPTIMAL`.
    pub fn dispatch(&self, inputs: BlendingCompositorInputs<'_>) -> Result<()> {
        let width = inputs.output.texture.width();
        let height = inputs.output.texture.height();

        // Layer-size contract — screen-aligned inputs must match the
        // output's dimensions exactly (PiP is sampler-rescaled, so it
        // is exempt).
        for (name, layer) in [
            ("video", inputs.video),
            ("lower_third", inputs.lower_third),
            ("watermark", inputs.watermark),
        ] {
            if let Some(layer) = layer {
                if layer.texture.width() != width || layer.texture.height() != height {
                    return Err(StreamError::GpuError(format!(
                        "{}: '{name}' layer is {}×{}, expected {width}×{height} (must match output)",
                        self.label,
                        layer.texture.width(),
                        layer.texture.height(),
                    )));
                }
            }
        }

        let (video, vlayout) = self.layer_or_placeholder(inputs.video);
        let (lower_third, lt_layout) = self.layer_or_placeholder(inputs.lower_third);
        let (watermark, wm_layout) = self.layer_or_placeholder(inputs.watermark);
        let (pip, pip_layout) = self.layer_or_placeholder(inputs.pip);
        let pip_dims = (pip.width(), pip.height());

        // Stage descriptor + push-constant writes onto the kernel
        // (single descriptor set since the kernel's ring depth is 1).
        self.kernel.set_sampled_texture(0, 0, video)?;
        self.kernel.set_sampled_texture(0, 1, lower_third)?;
        self.kernel.set_sampled_texture(0, 2, watermark)?;
        self.kernel.set_sampled_texture(0, 3, pip)?;

        let mut flags = 0u32;
        if inputs.video.is_some()       { flags |= flag_bits::HAS_VIDEO; }
        if inputs.lower_third.is_some() { flags |= flag_bits::HAS_LOWER_THIRD; }
        if inputs.watermark.is_some()   { flags |= flag_bits::HAS_WATERMARK; }
        if inputs.pip.is_some()         { flags |= flag_bits::HAS_PIP; }

        let push = BlendingCompositorPushConstants {
            width,
            height,
            pip_width: pip_dims.0,
            pip_height: pip_dims.1,
            flags,
            pip_slide_progress: inputs.pip_slide_progress.clamp(0.0, 1.0),
        };
        self.kernel.set_push_constants_value(0, &push)?;

        // Wait for any prior dispatch on this compositor to drain so
        // we can safely reset the command buffer.
        unsafe {
            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "{}: wait_for_fences failed: {e}",
                        self.label
                    ))
                })?;
            self.device.reset_fences(&[self.fence]).map_err(|e| {
                StreamError::GpuError(format!("{}: reset_fences failed: {e}", self.label))
            })?;
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "{}: reset_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            let begin = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            self.device
                .begin_command_buffer(self.command_buffer, &begin)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "{}: begin_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            // Pre-render barriers: each non-placeholder input → SHADER_READ_ONLY_OPTIMAL,
            // output → COLOR_ATTACHMENT_OPTIMAL. The placeholder is built
            // already in SHADER_READ_ONLY_OPTIMAL and stays there
            // forever, so it never needs a barrier.
            let mut barriers: Vec<vk::ImageMemoryBarrier2> = Vec::with_capacity(5);
            for (layer, layout) in [
                (inputs.video.map(|l| l.texture), vlayout),
                (inputs.lower_third.map(|l| l.texture), lt_layout),
                (inputs.watermark.map(|l| l.texture), wm_layout),
                (inputs.pip.map(|l| l.texture), pip_layout),
            ] {
                let Some(tex) = layer else { continue };
                if layout == VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
                    continue;
                }
                let image = tex.inner.image().ok_or_else(|| {
                    StreamError::GpuError(format!("{}: input texture has no VkImage", self.label))
                })?;
                barriers.push(input_barrier_to_shader_read_only(image, layout.as_vk()));
            }
            let output_image = inputs.output.texture.inner.image().ok_or_else(|| {
                StreamError::GpuError(format!("{}: output texture has no VkImage", self.label))
            })?;
            barriers.push(output_barrier_to_color_attachment(
                output_image,
                inputs.output.current_layout.as_vk(),
            ));
            let dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&barriers)
                .build();
            self.device.cmd_pipeline_barrier2(self.command_buffer, &dep);

            // Begin dynamic rendering with the output as the sole color
            // attachment. UNDEFINED → COLOR_ATTACHMENT_OPTIMAL is
            // already covered by the barrier above; LOAD is fine
            // because we draw a full-screen triangle that overwrites
            // every pixel.
            let output_view = inputs
                .output
                .texture
                .inner
                .image_view()
                .unwrap_or(vk::ImageView::null());
            let color_attachment = vk::RenderingAttachmentInfo::builder()
                .image_view(output_view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::DONT_CARE)
                .store_op(vk::AttachmentStoreOp::STORE)
                .build();
            let color_attachments = [color_attachment];
            let rendering_info = vk::RenderingInfo::builder()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width, height },
                })
                .layer_count(1)
                .color_attachments(&color_attachments)
                .build();
            self.device.cmd_begin_rendering(self.command_buffer, &rendering_info);

            self.kernel.cmd_bind_and_draw(
                self.command_buffer,
                0,
                &DrawCall {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                    viewport: Some(Viewport::full(width, height)),
                    scissor: Some(ScissorRect::full(width, height)),
                },
            )?;

            self.device.cmd_end_rendering(self.command_buffer);

            // Post-render barrier: output COLOR_ATTACHMENT_OPTIMAL →
            // SHADER_READ_ONLY_OPTIMAL so downstream consumers (display,
            // future encoders) can sample it without re-barriering.
            let post = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(output_image)
                .subresource_range(color_subresource_range())
                .build();
            let post_barriers = [post];
            let post_dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&post_barriers)
                .build();
            self.device.cmd_pipeline_barrier2(self.command_buffer, &post_dep);

            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "{}: end_command_buffer failed: {e}",
                        self.label
                    ))
                })?;

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(self.command_buffer)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();
            self.vulkan_device
                .submit_to_queue(self.queue, &[submit], self.fence)?;

            self.device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(|e| {
                    StreamError::GpuError(format!(
                        "{}: post-submit wait failed: {e}",
                        self.label
                    ))
                })?;
        }
        Ok(())
    }

    fn layer_or_placeholder<'a>(
        &'a self,
        layer: Option<BlendingLayer<'a>>,
    ) -> (&'a StreamTexture, VulkanLayout) {
        match layer {
            Some(BlendingLayer { texture, current_layout }) => (texture, current_layout),
            None => (&self.placeholder, VulkanLayout::SHADER_READ_ONLY_OPTIMAL),
        }
    }
}

impl Drop for VulkanBlendingCompositor {
    fn drop(&mut self) {
        unsafe {
            // Best-effort: the device may already be lost. None of these
            // calls panic on stale handles.
            let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
            self.device.destroy_fence(self.fence, None);
            self.device
                .free_command_buffers(self.command_pool, &[self.command_buffer]);
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

fn make_placeholder_texture(vulkan_device: &Arc<HostVulkanDevice>) -> Result<StreamTexture> {
    use crate::core::rhi::PixelFormat;

    let desc = TextureDescriptor::new(1, 1, TextureFormat::Bgra8Unorm).with_usage(
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
    );
    // Local (non-DMA-BUF) texture is fine for an internal placeholder —
    // it never crosses a process boundary.
    let host_tex = vulkan_device.create_texture_local(&desc)?;
    let image = host_tex.image().ok_or_else(|| {
        StreamError::GpuError("placeholder texture has no VkImage".into())
    })?;

    // Upload zeros (transparent BGRA); `upload_buffer_to_image` leaves
    // the image in SHADER_READ_ONLY_OPTIMAL — same path
    // `GpuContext::upload_pixel_buffer_as_texture` rides for cross-
    // process pixel-buffer fallbacks (`vulkan_device.rs:1851`).
    let staging =
        crate::vulkan::rhi::HostVulkanPixelBuffer::new(vulkan_device, 1, 1, 4, PixelFormat::Bgra32)?;
    unsafe {
        std::ptr::write_bytes(staging.mapped_ptr(), 0, 4);
        vulkan_device.upload_buffer_to_image(staging.buffer(), image, 1, 1)?;
    }

    Ok(StreamTexture {
        inner: Arc::new(host_tex),
    })
}

fn create_command_pool(
    device: &vulkanalia::Device,
    queue_family_index: u32,
) -> Result<vk::CommandPool> {
    let info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .build();
    unsafe { device.create_command_pool(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("create_command_pool: {e}")))
}

fn allocate_command_buffer(
    device: &vulkanalia::Device,
    pool: vk::CommandPool,
) -> Result<vk::CommandBuffer> {
    let info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let buffers = unsafe { device.allocate_command_buffers(&info) }
        .map_err(|e| StreamError::GpuError(format!("allocate_command_buffers: {e}")))?;
    Ok(buffers[0])
}

fn create_signaled_fence(device: &vulkanalia::Device) -> Result<vk::Fence> {
    let info = vk::FenceCreateInfo::builder()
        .flags(vk::FenceCreateFlags::SIGNALED)
        .build();
    unsafe { device.create_fence(&info, None) }
        .map_err(|e| StreamError::GpuError(format!("create_fence: {e}")))
}

fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::builder()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
        .build()
}

fn input_barrier_to_shader_read_only(
    image: vk::Image,
    old_layout: vk::ImageLayout,
) -> vk::ImageMemoryBarrier2 {
    // Tolerant src masks (ALL_COMMANDS + MEMORY_WRITE) cover every
    // upstream producer (camera compute, OpenGL adapter glFinish,
    // Skia/Vulkan adapters, future encoders) without per-producer
    // tuning — same shape `LinuxDisplayProcessor` and
    // `VulkanTextureReadback` use for the identical purpose.
    vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
        .old_layout(old_layout)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_subresource_range())
        .build()
}

fn output_barrier_to_color_attachment(
    image: vk::Image,
    old_layout: vk::ImageLayout,
) -> vk::ImageMemoryBarrier2 {
    vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
        .old_layout(old_layout)
        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_subresource_range())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{
        TextureReadbackDescriptor, TextureSourceLayout,
    };
    use crate::vulkan::rhi::VulkanTextureReadback;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    /// Allocate a render-target-capable texture for compositor input or
    /// output use. Local (non-DMA-BUF) — these are unit-test fixtures
    /// and never cross a process boundary.
    fn make_render_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
    ) -> StreamTexture {
        let desc = TextureDescriptor::new(width, height, TextureFormat::Bgra8Unorm).with_usage(
            TextureUsages::RENDER_ATTACHMENT
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
        );
        let host_tex = device.create_texture_local(&desc).expect("texture");
        StreamTexture {
            inner: Arc::new(host_tex),
        }
    }

    /// Fill a texture with a single BGRA color via host-visible staging
    /// buffer + cmd_copy_buffer_to_image. Leaves the image in
    /// SHADER_READ_ONLY_OPTIMAL.
    fn fill_texture_solid(
        device: &Arc<HostVulkanDevice>,
        texture: &StreamTexture,
        b: u8,
        g: u8,
        r: u8,
        a: u8,
    ) {
        use crate::core::rhi::PixelFormat;
        let w = texture.width();
        let h = texture.height();
        let staging =
            crate::vulkan::rhi::HostVulkanPixelBuffer::new(device, w, h, 4, PixelFormat::Bgra32)
                .expect("staging");
        let pixel = (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24);
        unsafe {
            let ptr = staging.mapped_ptr() as *mut u32;
            for i in 0..(w * h) as usize {
                *ptr.add(i) = pixel;
            }
        }
        let image = texture.inner.image().expect("image");
        unsafe {
            device
                .upload_buffer_to_image(staging.buffer(), image, w, h)
                .expect("upload");
        }
    }

    /// Read one pixel from a texture via the RHI's readback primitive.
    fn read_pixel(
        gpu_ctx_device: &Arc<HostVulkanDevice>,
        texture: &StreamTexture,
        x: u32,
        y: u32,
    ) -> (u8, u8, u8, u8) {
        let w = texture.width();
        let h = texture.height();
        let readback = VulkanTextureReadback::new(
            gpu_ctx_device,
            &TextureReadbackDescriptor {
                label: "blending-test-readback",
                format: TextureFormat::Bgra8Unorm,
                width: w,
                height: h,
            },
        )
        .expect("readback");
        let ticket = readback
            .submit(texture, TextureSourceLayout::ShaderReadOnly)
            .expect("readback submit");
        let mut sample: (u8, u8, u8, u8) = (0, 0, 0, 0);
        readback
            .wait_and_read_with(ticket, u64::MAX, |bgra| -> std::io::Result<()> {
                let idx = ((y * w + x) * 4) as usize;
                sample = (bgra[idx], bgra[idx + 1], bgra[idx + 2], bgra[idx + 3]);
                Ok(())
            })
            .expect("readback wait")
            .expect("readback read closure");
        sample
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

        let video = make_render_texture(&device, 64, 32);
        let output = make_render_texture(&device, 64, 32);
        // BGRA = (10, 200, 50, 255) → opaque green-ish.
        fill_texture_solid(&device, &video, 10, 200, 50, 255);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(BlendingLayer {
                    texture: &video,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                lower_third: None,
                watermark: None,
                pip: None,
                output: BlendingOutput {
                    texture: &output,
                    current_layout: VulkanLayout::UNDEFINED,
                },
                pip_slide_progress: 0.0,
            })
            .expect("dispatch");

        // ±1 tolerance per channel for unorm round-trip.
        let (b, g, r, a) = read_pixel(&device, &output, 16, 16);
        assert!((b as i32 - 10).abs() <= 1, "B={b}");
        assert!((g as i32 - 200).abs() <= 1, "G={g}");
        assert!((r as i32 - 50).abs() <= 1, "R={r}");
        assert!((a as i32 - 255).abs() <= 1, "A={a}");
    }

    #[test]
    fn no_video_falls_back_to_dark_blue() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let compositor = VulkanBlendingCompositor::new(&device).expect("compositor");
        let output = make_render_texture(&device, 32, 32);

        compositor
            .dispatch(BlendingCompositorInputs {
                video: None,
                lower_third: None,
                watermark: None,
                pip: None,
                output: BlendingOutput {
                    texture: &output,
                    current_layout: VulkanLayout::UNDEFINED,
                },
                pip_slide_progress: 0.0,
            })
            .expect("dispatch");

        // Fragment shader's no-video fallback is vec4(0.05, 0.05, 0.12, 1.0)
        // → BGRA roughly (31, 13, 13, 255).
        let (b, g, r, a) = read_pixel(&device, &output, 8, 8);
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

        let video = make_render_texture(&device, 32, 32);
        let output = make_render_texture(&device, 64, 32);
        fill_texture_solid(&device, &video, 0, 0, 0, 255);

        let err = compositor
            .dispatch(BlendingCompositorInputs {
                video: Some(BlendingLayer {
                    texture: &video,
                    current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                }),
                lower_third: None,
                watermark: None,
                pip: None,
                output: BlendingOutput {
                    texture: &output,
                    current_layout: VulkanLayout::UNDEFINED,
                },
                pip_slide_progress: 0.0,
            })
            .expect_err("size mismatch must error");
        assert!(matches!(err, StreamError::GpuError(_)));
    }
}

