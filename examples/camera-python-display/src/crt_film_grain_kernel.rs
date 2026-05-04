// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 80s Blade Runner CRT + film-grain post-effect kernel — sandboxed
//! scenario content.
//!
//! ## Why this lives in the example, not the engine
//!
//! Pre-#487 this kernel and its compute shader (storage-buffer in/out,
//! manual bilinear sampling, packed-BGRA uint addressing) lived in
//! `libs/streamlib/src/vulkan/rhi/`. That placement encoded a single
//! demo's app content (Blade Runner CRT vibe) into the engine. It also
//! encoded a **wrong-shape hot-path pattern** — synchronous fence-
//! blocked GPU dispatch with internal layout-barrier management — into
//! the engine's API surface, despite production engines (UE5, Bevy,
//! Granite, wgpu) deliberately not shipping such an API. Real
//! renderers use a render graph that schedules barriers across passes
//! and lets the CPU race ahead 1–2 frames; the synchronous-blocking
//! shape stalls the CPU every frame.
//!
//! #487 relocated the kernel here as transitional sandboxed code
//! AND ported it from a compute kernel (storage-buffer in/out) to a
//! graphics kernel (sampled texture in / color attachment out) — the
//! buffer-based shape can't consume the post-#485 texture-throughout
//! pipeline. The boundary-check exception
//! (`xtask/src/check_boundaries.rs::VULKANALIA_*_ALLOWLIST`) gates
//! the example's `vulkanalia` import.
//!
//! ## When this goes away
//!
//! When **RDG (#631)** ships and absorbs the kernel-wrapper command-
//! buffer recording into render-graph passes. The example switches
//! to RDG primitives in the same PR; this file, the matching
//! `blending_compositor_kernel.rs`, the example's `vulkanalia` Cargo
//! dep, and the boundary-check allowlist exception are all removed
//! together.
//!
//! ## Lifecycle
//!
//! Caller pre-allocates a ring of output `StreamTexture`s (mirrors
//! `BlendingCompositor`'s `OUTPUT_RING_DEPTH = 2`), hands one to
//! [`SandboxedCrtFilmGrain::dispatch`] per frame along with the input
//! texture + its current Vulkan layout, and `dispatch` returns once
//! the GPU has signaled the kernel's per-render fence. After return,
//! both input and output textures are in `SHADER_READ_ONLY_OPTIMAL`,
//! ready for the next consumer to sample without re-barriering.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use streamlib::core::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState, DrawCall,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    PrimitiveTopology, RasterizationState, ScissorRect, StreamTexture, TextureFormat,
    VertexInputState, Viewport, VulkanLayout,
};
use streamlib::core::{Result, StreamError};
use streamlib::host_rhi::{HostVulkanDevice, VulkanGraphicsKernel};

/// Push-constants layout — must match `crt_film_grain.frag`'s
/// `layout(push_constant)` block byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CrtFilmGrainPushConstants {
    pub width: u32,
    pub height: u32,
    pub time: f32,
    pub crt_curve: f32,
    pub scanline_intensity: f32,
    pub chromatic_aberration: f32,
    pub grain_intensity: f32,
    pub grain_speed: f32,
    pub vignette_intensity: f32,
    pub brightness: f32,
}

/// Single input layer (the pre-effect texture) + its current Vulkan
/// layout. The kernel barriers from `current_layout` to
/// `SHADER_READ_ONLY_OPTIMAL` before the draw and leaves it there
/// afterward.
#[derive(Clone, Copy)]
pub struct CrtFilmGrainInput<'a> {
    pub texture: &'a StreamTexture,
    pub current_layout: VulkanLayout,
}

/// Render target for one CRT/film-grain dispatch. Same shape as
/// `BlendingOutput` in the sibling kernel.
#[derive(Clone, Copy)]
pub struct CrtFilmGrainOutput<'a> {
    pub texture: &'a StreamTexture,
    pub current_layout: VulkanLayout,
}

pub struct CrtFilmGrainInputs<'a> {
    pub input: CrtFilmGrainInput<'a>,
    pub output: CrtFilmGrainOutput<'a>,
    pub time_seconds: f32,
    pub crt_curve: f32,
    pub scanline_intensity: f32,
    pub chromatic_aberration: f32,
    pub grain_intensity: f32,
    pub grain_speed: f32,
    pub vignette_intensity: f32,
    pub brightness: f32,
}

/// CRT + film-grain post-effect graphics kernel.
///
/// See the module-level docs for the full transitional rationale and
/// the link to RDG (#631) — the destination this code migrates to
/// when the engine grows a proper render graph.
pub struct SandboxedCrtFilmGrain {
    label: &'static str,
    vulkan_device: Arc<HostVulkanDevice>,
    device: vulkanalia::Device,
    queue: vk::Queue,
    kernel: VulkanGraphicsKernel,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
}

impl SandboxedCrtFilmGrain {
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let label = "crt_film_grain";

        let vert =
            include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.vert.spv"));
        let frag =
            include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.frag.spv"));

        let stages = [GraphicsStage::vertex(vert), GraphicsStage::fragment(frag)];
        let bindings = [
            GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
        ];
        let descriptor = GraphicsKernelDescriptor {
            label,
            stages: &stages,
            bindings: &bindings,
            push_constants: GraphicsPushConstants {
                size: std::mem::size_of::<CrtFilmGrainPushConstants>() as u32,
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
                attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 1,
        };
        let kernel = VulkanGraphicsKernel::new(vulkan_device, &descriptor)?;

        let device = vulkan_device.device().clone();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();

        let command_pool = create_command_pool(&device, queue_family_index)?;
        let command_buffer = allocate_command_buffer(&device, command_pool)?;
        let fence = create_signaled_fence(&device)?;

        Ok(Self {
            label,
            vulkan_device: Arc::clone(vulkan_device),
            device,
            queue,
            kernel,
            command_pool,
            command_buffer,
            fence,
        })
    }

    /// Apply the CRT/film-grain effect from `inputs.input.texture`
    /// into `inputs.output.texture`. Output dimensions drive the
    /// viewport/scissor; input must match the output 1:1 (the shader
    /// samples at the same screen UV).
    pub fn dispatch(&self, inputs: CrtFilmGrainInputs<'_>) -> Result<()> {
        let width = inputs.output.texture.width();
        let height = inputs.output.texture.height();

        if inputs.input.texture.width() != width
            || inputs.input.texture.height() != height
        {
            return Err(StreamError::GpuError(format!(
                "{}: input is {}×{}, expected {width}×{height} (must match output)",
                self.label,
                inputs.input.texture.width(),
                inputs.input.texture.height(),
            )));
        }

        self.kernel.set_sampled_texture(0, 0, inputs.input.texture)?;

        let push = CrtFilmGrainPushConstants {
            width,
            height,
            time: inputs.time_seconds,
            crt_curve: inputs.crt_curve,
            scanline_intensity: inputs.scanline_intensity,
            chromatic_aberration: inputs.chromatic_aberration,
            grain_intensity: inputs.grain_intensity,
            grain_speed: inputs.grain_speed,
            vignette_intensity: inputs.vignette_intensity,
            brightness: inputs.brightness,
        };
        self.kernel.set_push_constants_value(0, &push)?;

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

            // Pre-render barriers: input → SHADER_READ_ONLY_OPTIMAL,
            // output → COLOR_ATTACHMENT_OPTIMAL.
            let mut barriers: Vec<vk::ImageMemoryBarrier2> = Vec::with_capacity(2);
            if inputs.input.current_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
                let input_image = inputs.input.texture.vulkan_inner().image().ok_or_else(|| {
                    StreamError::GpuError(format!("{}: input texture has no VkImage", self.label))
                })?;
                barriers.push(input_barrier_to_shader_read_only(
                    input_image,
                    inputs.input.current_layout.as_vk(),
                ));
            }
            let output_image = inputs.output.texture.vulkan_inner().image().ok_or_else(|| {
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
            // attachment. The full-screen triangle covers every pixel,
            // so DONT_CARE on load is fine.
            let output_view = inputs
                .output
                .texture
                .vulkan_inner()
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

            // Post-render: output COLOR_ATTACHMENT_OPTIMAL →
            // SHADER_READ_ONLY_OPTIMAL.
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
}

impl Drop for SandboxedCrtFilmGrain {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
            self.device.destroy_fence(self.fence, None);
            self.device
                .free_command_buffers(self.command_pool, &[self.command_buffer]);
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
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
    use streamlib::core::rhi::{
        PixelFormat, TextureDescriptor, TextureReadbackDescriptor, TextureSourceLayout,
        TextureUsages,
    };
    use streamlib::host_rhi::{HostVulkanPixelBuffer, VulkanTextureReadback};

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

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
        StreamTexture::from_vulkan(host_tex)
    }

    fn fill_texture_solid(
        device: &Arc<HostVulkanDevice>,
        texture: &StreamTexture,
        b: u8,
        g: u8,
        r: u8,
        a: u8,
    ) {
        let w = texture.width();
        let h = texture.height();
        let staging = HostVulkanPixelBuffer::new(device, w, h, 4, PixelFormat::Bgra32)
            .expect("staging");
        let pixel = (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24);
        unsafe {
            let ptr = staging.mapped_ptr() as *mut u32;
            for i in 0..(w * h) as usize {
                *ptr.add(i) = pixel;
            }
        }
        let image = texture.vulkan_inner().image().expect("image");
        unsafe {
            device
                .upload_buffer_to_image(staging.buffer(), image, w, h)
                .expect("upload");
        }
    }

    /// Read a single BGRA pixel via the RHI readback primitive.
    fn read_pixel(
        device: &Arc<HostVulkanDevice>,
        texture: &StreamTexture,
        x: u32,
        y: u32,
    ) -> (u8, u8, u8, u8) {
        let w = texture.width();
        let h = texture.height();
        let readback = VulkanTextureReadback::new(
            device,
            &TextureReadbackDescriptor {
                label: "crt-test-readback",
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

    /// Read the full BGRA buffer from a texture for visual-smoke tests.
    fn read_bgra_buffer(
        device: &Arc<HostVulkanDevice>,
        texture: &StreamTexture,
    ) -> Vec<u8> {
        let w = texture.width();
        let h = texture.height();
        let readback = VulkanTextureReadback::new(
            device,
            &TextureReadbackDescriptor {
                label: "crt-test-readback-full",
                format: TextureFormat::Bgra8Unorm,
                width: w,
                height: h,
            },
        )
        .expect("readback");
        let ticket = readback
            .submit(texture, TextureSourceLayout::ShaderReadOnly)
            .expect("readback submit");
        let mut bytes = vec![0u8; (w * h * 4) as usize];
        readback
            .wait_and_read_with(ticket, u64::MAX, |bgra| -> std::io::Result<()> {
                bytes.copy_from_slice(bgra);
                Ok(())
            })
            .expect("readback wait")
            .expect("readback read closure");
        bytes
    }

    fn default_inputs<'a>(
        input: &'a StreamTexture,
        output: &'a StreamTexture,
    ) -> CrtFilmGrainInputs<'a> {
        CrtFilmGrainInputs {
            input: CrtFilmGrainInput {
                texture: input,
                current_layout: VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            },
            output: CrtFilmGrainOutput {
                texture: output,
                current_layout: VulkanLayout::UNDEFINED,
            },
            time_seconds: 0.0,
            crt_curve: 0.7,
            scanline_intensity: 0.6,
            chromatic_aberration: 0.004,
            grain_intensity: 0.18,
            grain_speed: 1.0,
            vignette_intensity: 0.5,
            brightness: 2.2,
        }
    }

    #[test]
    fn new_compiles_kernel() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let result = SandboxedCrtFilmGrain::new(&device);
        assert!(
            result.is_ok(),
            "kernel creation must succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn rejects_size_mismatch() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let kernel = SandboxedCrtFilmGrain::new(&device).expect("kernel");
        let input = make_render_texture(&device, 32, 32);
        let output = make_render_texture(&device, 64, 32);
        fill_texture_solid(&device, &input, 0, 0, 0, 255);

        let err = kernel
            .dispatch(default_inputs(&input, &output))
            .expect_err("size mismatch must error");
        assert!(matches!(err, StreamError::GpuError(_)));
    }

    /// Runs the kernel against a uniform mid-grey input. The center of
    /// the output (well inside the barrel-distorted screen rect) must
    /// be non-black after CRT processing, and the far corners must be
    /// black (outside the curved bounds → explicitly zeroed by the
    /// shader's outside-bounds carve-out).
    #[test]
    fn solid_input_produces_curved_bounds() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let kernel = SandboxedCrtFilmGrain::new(&device).expect("kernel");

        let w: u32 = 256;
        let h: u32 = 192;
        let input = make_render_texture(&device, w, h);
        let output = make_render_texture(&device, w, h);
        // Mid-grey opaque BGRA = (128, 128, 128, 255).
        fill_texture_solid(&device, &input, 128, 128, 128, 255);

        let mut inputs = default_inputs(&input, &output);
        // Disable grain so we can make deterministic assertions about
        // the center pixel; barrel curve stays in to test the bounds
        // carve-out.
        inputs.grain_intensity = 0.0;
        kernel.dispatch(inputs).expect("dispatch");

        let (cb, cg, cr, _ca) = read_pixel(&device, &output, w / 2, h / 2);
        assert!(
            cb as u32 + cg as u32 + cr as u32 > 0,
            "center pixel must not be fully black after CRT pass: BGR=({cb},{cg},{cr})"
        );

        let (b0, g0, r0, _a0) = read_pixel(&device, &output, 0, 0);
        assert_eq!(
            (b0, g0, r0),
            (0, 0, 0),
            "top-left corner must be zeroed by outside-bounds carve-out"
        );
    }

    /// Visual smoke: feeds a checkerboard + magenta block + green
    /// diagonal through the kernel, dispatches, writes a PNG of the
    /// result for human review. Mirrors the engine version's shape.
    #[test]
    fn visual_smoke_emits_png() {
        let device = match try_vulkan_device() {
            Some(d) => d,
            None => return,
        };
        let kernel = SandboxedCrtFilmGrain::new(&device).expect("kernel");

        let w: u32 = 480;
        let h: u32 = 320;
        let input = make_render_texture(&device, w, h);
        let output = make_render_texture(&device, w, h);

        // Compose a synthetic input via staging buffer: 32x32
        // checkerboard with a magenta block in the upper-left and a
        // green diagonal stripe.
        let staging =
            HostVulkanPixelBuffer::new(&device, w, h, 4, PixelFormat::Bgra32).expect("staging");
        unsafe {
            let ptr = staging.mapped_ptr() as *mut u32;
            for y in 0..h {
                for x in 0..w {
                    let cell = ((x / 32) + (y / 32)) % 2 == 0;
                    let mut b = if cell { 200u32 } else { 60u32 };
                    let mut g = if cell { 200u32 } else { 60u32 };
                    let mut r = if cell { 200u32 } else { 60u32 };
                    if x < 96 && y < 96 {
                        b = 255;
                        g = 0;
                        r = 255;
                    }
                    let on_diag = (x as i32 - y as i32).abs() < 8;
                    if on_diag {
                        b = 0;
                        g = 240;
                        r = 0;
                    }
                    let pixel = b | (g << 8) | (r << 16) | (255u32 << 24);
                    *ptr.add((y * w + x) as usize) = pixel;
                }
            }
        }
        let image = input.vulkan_inner().image().expect("image");
        unsafe {
            device
                .upload_buffer_to_image(staging.buffer(), image, w, h)
                .expect("upload");
        }

        let mut inputs = default_inputs(&input, &output);
        // Pick a non-zero animation phase so scanlines + grain are
        // visible in the rendered PNG.
        inputs.time_seconds = 0.4;
        kernel.dispatch(inputs).expect("dispatch must succeed");

        let bgra_bytes = read_bgra_buffer(&device, &output);
        let bgra_size = bgra_bytes.len();
        let mut rgba = vec![0u8; bgra_size];
        for chunk in 0..(bgra_size / 4) {
            let i = chunk * 4;
            rgba[i] = bgra_bytes[i + 2];
            rgba[i + 1] = bgra_bytes[i + 1];
            rgba[i + 2] = bgra_bytes[i];
            rgba[i + 3] = bgra_bytes[i + 3];
        }

        let out_path = std::env::var("STREAMLIB_CRT_FILM_GRAIN_PNG_OUT")
            .unwrap_or_else(|_| "target/crt_film_grain_smoke.png".to_string());
        let _ = std::fs::create_dir_all(
            std::path::Path::new(&out_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );
        let file = std::fs::File::create(&out_path)
            .unwrap_or_else(|e| panic!("create {out_path}: {e}"));
        let bw = std::io::BufWriter::new(file);
        let mut encoder = png::Encoder::new(bw, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&rgba).expect("PNG data");
        eprintln!("crt_film_grain visual smoke wrote {out_path}");
    }
}
