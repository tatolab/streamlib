// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared scaffolding for the OpenGL adapter integration tests.
//! Each per-test file pulls this in via
//! `#[path = "common.rs"] mod common;`.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // each test file uses a different subset

use std::sync::Arc;

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::{StreamTexture, TextureDescriptor, TextureFormat, TextureUsages};
use streamlib::host_rhi::HostVulkanTexture;
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter, DRM_FORMAT_ARGB8888,
};

pub fn try_init_runtime() -> Option<(GpuContext, Arc<EglRuntime>)> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_opengl=debug,streamlib=warn")
        .try_init();
    let gpu = GpuContext::init_for_platform_sync().ok()?;
    let egl = match EglRuntime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("EglRuntime::new failed: {e} — skipping");
            return None;
        }
    };
    Some((gpu, egl))
}

pub struct HostFixture {
    pub gpu: GpuContext,
    pub egl: Arc<EglRuntime>,
    pub adapter: Arc<OpenGlSurfaceAdapter>,
}

impl HostFixture {
    pub fn try_new() -> Option<Self> {
        let (gpu, egl) = try_init_runtime()?;
        let adapter = Arc::new(OpenGlSurfaceAdapter::new(Arc::clone(&egl)));
        Some(Self { gpu, egl, adapter })
    }

    /// Allocate a host-side render-target VkImage, export its
    /// DMA-BUF + DRM modifier, register both with the OpenGL
    /// adapter as a `GL_TEXTURE_2D`, and return everything the test
    /// needs.
    pub fn register_surface(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
    ) -> RegisteredSurface {
        self.register_surface_inner(surface_id, width, height, /*external_oes=*/ false)
    }

    /// Same as [`Self::register_surface`] but registers via
    /// [`OpenGlSurfaceAdapter::register_external_oes_host_surface`]
    /// so the resulting GL texture is bound under
    /// `GL_TEXTURE_EXTERNAL_OES`. The underlying VkImage is identical
    /// (render-target-capable tiled DMA-BUF) — the difference is
    /// purely in the GL binding target.
    pub fn register_external_oes_surface(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
    ) -> RegisteredSurface {
        self.register_surface_inner(surface_id, width, height, /*external_oes=*/ true)
    }

    /// Allocate a host VkImage with an explicit DRM modifier (typically a
    /// sampler-only `external_only=TRUE` modifier discovered via
    /// [`streamlib::host_rhi::drm_modifier_probe`]) and register it via
    /// [`OpenGlSurfaceAdapter::register_external_oes_host_surface`].
    ///
    /// Unlike [`Self::register_external_oes_surface`] (which goes through
    /// `acquire_render_target_dma_buf_image` and gets a tiled,
    /// render-target-capable modifier), this variant lets a test exercise
    /// the path real-world camera DMA-BUFs hit on NVIDIA — where the EGL
    /// driver flags the only available modifier as `external_only=TRUE`.
    /// Usage flags are [`TextureUsages::TEXTURE_BINDING`] +
    /// [`TextureUsages::COPY_DST`] + [`TextureUsages::COPY_SRC`] (no
    /// `RENDER_ATTACHMENT`) — matches the sampler-only contract of
    /// `GL_TEXTURE_EXTERNAL_OES`.
    pub fn register_external_oes_surface_with_modifier(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        modifier: u64,
    ) -> Result<RegisteredSurface, String> {
        let vulkan_device = self.gpu.device().vulkan_device();
        let desc = TextureDescriptor::new(width, height, TextureFormat::Bgra8Unorm).with_usage(
            TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
        );
        let host_texture = HostVulkanTexture::new_render_target_dma_buf(
            vulkan_device,
            &desc,
            &[modifier],
        )
        .map_err(|e| format!("HostVulkanTexture::new_render_target_dma_buf failed: {e}"))?;

        let dma_buf_fd = host_texture
            .export_dma_buf_fd()
            .map_err(|e| format!("export_dma_buf_fd failed: {e}"))?;
        let plane_layout = host_texture
            .dma_buf_plane_layout()
            .map_err(|e| format!("dma_buf_plane_layout failed: {e}"))?;
        let chosen_modifier = host_texture.chosen_drm_format_modifier();
        assert_eq!(
            chosen_modifier, modifier,
            "driver picked modifier 0x{chosen_modifier:016x}, but the candidate \
             list contained only 0x{modifier:016x} — VUID violation in the RHI"
        );

        let texture = StreamTexture::from_vulkan(host_texture);

        let registration = HostSurfaceRegistration {
            dma_buf_fd,
            width,
            height,
            drm_fourcc: DRM_FORMAT_ARGB8888,
            drm_format_modifier: modifier,
            plane_offset: plane_layout[0].0,
            plane_stride: plane_layout[0].1,
        };

        self.adapter
            .register_external_oes_host_surface(surface_id, registration)
            .map_err(|e| format!("register_external_oes_host_surface failed: {e:?}"))?;

        let descriptor = StreamlibSurface::new(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            SurfaceUsage::SAMPLED,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        Ok(RegisteredSurface {
            descriptor,
            texture,
            width,
            height,
        })
    }

    fn register_surface_inner(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        external_oes: bool,
    ) -> RegisteredSurface {
        let texture = self
            .gpu
            .acquire_render_target_dma_buf_image(width, height, TextureFormat::Bgra8Unorm)
            .expect("acquire_render_target_dma_buf_image");

        let dma_buf_fd = texture
            .vulkan_inner()
            .export_dma_buf_fd()
            .expect("export DMA-BUF");
        let plane_layout = texture
            .vulkan_inner()
            .dma_buf_plane_layout()
            .expect("dma_buf_plane_layout");
        let modifier = texture.vulkan_inner().chosen_drm_format_modifier();

        let registration = HostSurfaceRegistration {
            // EGL dups the FD on import; we hand over our copy.
            dma_buf_fd,
            width,
            height,
            // Vulkan `Bgra8Unorm` is "memory: B,G,R,A". The DRM
            // fourcc with that memory layout is ARGB8888 — its
            // 32-bit value 0xAARRGGBB happens to land B at byte 0
            // on a little-endian box. Using ABGR8888 (memory:
            // R,G,B,A) here would silently swap R↔B on every
            // GL-side write because the EGL importer trusts the
            // declared fourcc.
            drm_fourcc: DRM_FORMAT_ARGB8888,
            drm_format_modifier: modifier,
            plane_offset: plane_layout[0].0,
            plane_stride: plane_layout[0].1,
        };

        if external_oes {
            self.adapter
                .register_external_oes_host_surface(surface_id, registration)
                .expect("register_external_oes_host_surface");
        } else {
            self.adapter
                .register_host_surface(surface_id, registration)
                .expect("register_host_surface");
        }

        let descriptor = StreamlibSurface::new(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        RegisteredSurface {
            descriptor,
            texture,
            width,
            height,
        }
    }
}

pub struct RegisteredSurface {
    pub descriptor: StreamlibSurface,
    pub texture: StreamTexture,
    pub width: u32,
    pub height: u32,
}

/// Common helper: acquire write through the Vulkan adapter, clear the
/// VkImage to a known color, release. Used by tests that need the
/// host to seed a known pattern before exercising the GL side.
pub fn host_write_clear_color(
    gpu: &GpuContext,
    surface: &RegisteredSurface,
    color: [f32; 4],
) {
    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    let device = Arc::clone(gpu.device().vulkan_device());
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    let image = surface.texture.vulkan_inner().image().expect("image handle");

    let pool = unsafe {
        dev.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .build(),
            None,
        )
    }
    .expect("create_command_pool");
    let cmd = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .expect("allocate_command_buffers")[0];
    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .expect("begin_command_buffer");

    // UNDEFINED → TRANSFER_DST so the clear lands cleanly.
    let to_transfer = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let bs = [to_transfer];
    let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let clear_value = vk::ClearColorValue { float32: color };
    let range = vk::ImageSubresourceRange::builder()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
        .build();
    let ranges = [range];
    unsafe {
        dev.cmd_clear_color_image(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &clear_value,
            &ranges,
        )
    };

    // TRANSFER_DST → GENERAL so GL's later sampler import sees a
    // layout it tolerates. (Vulkan->GL handoff via DMA-BUF is opaque
    // to the modifier — the layout-record on the host side is what
    // matters for the next Vulkan consumer.)
    let to_general = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let bs2 = [to_general];
    let dep2 = vk::DependencyInfo::builder().image_memory_barriers(&bs2).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep2) };

    unsafe { dev.end_command_buffer(cmd) }.expect("end_command_buffer");
    let cmd_infos = [vk::CommandBufferSubmitInfo::builder().command_buffer(cmd).build()];
    let submits = [vk::SubmitInfo2::builder().command_buffer_infos(&cmd_infos).build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }.expect("submit");
    unsafe { dev.queue_wait_idle(queue) }.expect("queue_wait_idle");
    unsafe { dev.destroy_command_pool(pool, None) };
}

/// Read pixels from the host VkImage back into a CPU buffer for
/// verification. Returns BGRA8 bytes in `width*height*4` size.
pub fn host_readback(gpu: &GpuContext, surface: &RegisteredSurface) -> Vec<u8> {
    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    let device = Arc::clone(gpu.device().vulkan_device());
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();
    let image = surface.texture.vulkan_inner().image().expect("image handle");
    let bytes = (surface.width as u64) * (surface.height as u64) * 4;

    // Staging buffer (HOST_VISIBLE | HOST_COHERENT).
    let buf = unsafe {
        dev.create_buffer(
            &vk::BufferCreateInfo::builder()
                .size(bytes)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .build(),
            None,
        )
    }
    .expect("create_buffer");
    let mem_req = unsafe { dev.get_buffer_memory_requirements(buf) };
    let inst = device.instance();
    let phys = device.physical_device();
    let mem_props = unsafe { inst.get_physical_device_memory_properties(phys) };
    let needed = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mem_idx = (0..mem_props.memory_type_count)
        .find(|i| {
            let bit = 1u32 << i;
            (mem_req.memory_type_bits & bit) != 0
                && mem_props.memory_types[*i as usize]
                    .property_flags
                    .contains(needed)
        })
        .expect("host-visible memory type");
    let mem = unsafe {
        dev.allocate_memory(
            &vk::MemoryAllocateInfo::builder()
                .allocation_size(mem_req.size)
                .memory_type_index(mem_idx)
                .build(),
            None,
        )
    }
    .expect("allocate_memory");
    unsafe { dev.bind_buffer_memory(buf, mem, 0) }.expect("bind_buffer_memory");

    let pool = unsafe {
        dev.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .build(),
            None,
        )
    }
    .expect("create_command_pool");
    let cmd = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .expect("allocate_command_buffers")[0];
    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .expect("begin_command_buffer");

    let to_src = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let bs = [to_src];
    let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let copy = vk::BufferImageCopy::builder()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(
            vk::ImageSubresourceLayers::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1)
                .build(),
        )
        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
        .image_extent(vk::Extent3D {
            width: surface.width,
            height: surface.height,
            depth: 1,
        })
        .build();
    let regions = [copy];
    unsafe {
        dev.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buf,
            &regions,
        )
    };

    // Restore GENERAL so subsequent acquires don't see a stale layout
    // record.
    let to_general = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::TRANSFER_READ)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let bs2 = [to_general];
    let dep2 = vk::DependencyInfo::builder().image_memory_barriers(&bs2).build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep2) };

    unsafe { dev.end_command_buffer(cmd) }.expect("end_command_buffer");
    let cmd_infos = [vk::CommandBufferSubmitInfo::builder().command_buffer(cmd).build()];
    let submits = [vk::SubmitInfo2::builder().command_buffer_infos(&cmd_infos).build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }.expect("submit");
    unsafe { dev.queue_wait_idle(queue) }.expect("queue_wait_idle");

    let mapped = unsafe { dev.map_memory(mem, 0, bytes, vk::MemoryMapFlags::empty()) }
        .expect("map_memory");
    let slice = unsafe { std::slice::from_raw_parts(mapped as *const u8, bytes as usize) };
    let out = slice.to_vec();
    unsafe { dev.unmap_memory(mem) };
    unsafe { dev.destroy_command_pool(pool, None) };
    unsafe { dev.destroy_buffer(buf, None) };
    unsafe { dev.free_memory(mem, None) };
    out
}

