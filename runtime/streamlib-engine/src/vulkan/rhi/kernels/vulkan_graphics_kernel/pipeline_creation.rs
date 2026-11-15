// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{
    ColorBlendState, DepthStencilState, GraphicsBindingSpec, GraphicsDynamicState,
    GraphicsKernelDescriptor, GraphicsPipelineState, GraphicsShaderStage, GraphicsStage,
    TextureFormat, VertexInputState,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::kernels::vulkan_pipeline_cache_on_disk::{
    PipelineCacheKernelKind, create_pipeline_cache_handle, persist_pipeline_cache,
    pipeline_cache_file_path_keyed_by, read_cache_blob,
};

use super::pipeline_state_conversions::{
    blend_factor_to_vk, blend_op_to_vk, color_write_mask_to_vk, cull_mode_to_vk,
    depth_compare_op_to_vk, depth_format_to_vk, descriptor_kind_to_vk, front_face_to_vk,
    graphics_stage_to_vk, polygon_mode_to_vk, primitive_topology_to_vk, shader_stage_flags_to_vk,
    texture_format_to_vk, vertex_attribute_format_to_vk, vertex_input_rate_to_vk,
};

pub(super) fn create_shader_modules(
    device: &vulkanalia::Device,
    descriptor: &GraphicsKernelDescriptor<'_>,
) -> Result<Vec<(GraphicsShaderStage, vk::ShaderModule)>> {
    let mut out: Vec<(GraphicsShaderStage, vk::ShaderModule)> =
        Vec::with_capacity(descriptor.stages.len());
    for stage in descriptor.stages {
        let spirv: Vec<u32> = stage
            .spv
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let info = vk::ShaderModuleCreateInfo::builder().code(&spirv).build();
        match unsafe { device.create_shader_module(&info, None) } {
            Ok(m) => out.push((stage.stage, m)),
            Err(e) => {
                destroy_shader_modules(device, &out);
                return Err(Error::GpuError(format!(
                    "Graphics kernel '{}': failed to create {:?} shader module: {e}",
                    descriptor.label, stage.stage
                )));
            }
        }
    }
    Ok(out)
}

pub(super) fn destroy_shader_modules(
    device: &vulkanalia::Device,
    modules: &[(GraphicsShaderStage, vk::ShaderModule)],
) {
    for (_, m) in modules {
        unsafe { device.destroy_shader_module(*m, None) };
    }
}

pub(super) fn create_descriptor_set_layout(
    device: &vulkanalia::Device,
    bindings: &[GraphicsBindingSpec],
) -> Result<vk::DescriptorSetLayout> {
    let layout_bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|spec| {
            vk::DescriptorSetLayoutBinding::builder()
                .binding(spec.binding)
                .descriptor_type(descriptor_kind_to_vk(spec.kind))
                .descriptor_count(1)
                .stage_flags(shader_stage_flags_to_vk(spec.stages))
                .build()
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&layout_bindings)
        .build();
    unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create descriptor set layout: {e}")))
}

pub(super) fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push: crate::core::rhi::GraphicsPushConstants,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_ranges: Vec<vk::PushConstantRange> = if push.size > 0 {
        vec![
            vk::PushConstantRange::builder()
                .stage_flags(shader_stage_flags_to_vk(push.stages))
                .offset(0)
                .size(push.size)
                .build(),
        ]
    } else {
        Vec::new()
    };
    let info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_ranges)
        .build();
    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create pipeline layout: {e}")))
}

pub(super) fn create_descriptor_pool(
    device: &vulkanalia::Device,
    bindings: &[GraphicsBindingSpec],
    sets_in_flight: u32,
) -> Result<vk::DescriptorPool> {
    let mut counts: HashMap<vk::DescriptorType, u32> = HashMap::new();
    for spec in bindings {
        *counts.entry(descriptor_kind_to_vk(spec.kind)).or_insert(0) += sets_in_flight;
    }
    let pool_sizes: Vec<vk::DescriptorPoolSize> = counts
        .into_iter()
        .map(|(ty, count)| {
            vk::DescriptorPoolSize::builder()
                .type_(ty)
                .descriptor_count(count.max(1))
                .build()
        })
        .collect();
    let info = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(sets_in_flight)
        .pool_sizes(&pool_sizes)
        .build();
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create descriptor pool: {e}")))
}

pub(super) fn allocate_descriptor_sets(
    device: &vulkanalia::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    count: u32,
) -> Result<Vec<vk::DescriptorSet>> {
    let layouts: Vec<vk::DescriptorSetLayout> = (0..count).map(|_| layout).collect();
    let info = vk::DescriptorSetAllocateInfo::builder()
        .descriptor_pool(pool)
        .set_layouts(&layouts)
        .build();
    let sets = unsafe { device.allocate_descriptor_sets(&info) }
        .map_err(|e| Error::GpuError(format!("Failed to allocate descriptor sets: {e}")))?;
    Ok(sets)
}

pub(super) fn create_command_pool(
    device: &vulkanalia::Device,
    queue_family_index: u32,
) -> Result<vk::CommandPool> {
    let info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .build();
    unsafe { device.create_command_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create command pool: {e}")))
}

pub(super) fn allocate_command_buffer(
    device: &vulkanalia::Device,
    pool: vk::CommandPool,
) -> Result<vk::CommandBuffer> {
    let info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let buffers = unsafe { device.allocate_command_buffers(&info) }
        .map_err(|e| Error::GpuError(format!("Failed to allocate command buffer: {e}")))?;
    Ok(buffers[0])
}

pub(super) fn create_graphics_pipeline_with_cache(
    device: &vulkanalia::Device,
    shader_modules: &[(GraphicsShaderStage, vk::ShaderModule)],
    stages: &[GraphicsStage<'_>],
    pipeline_layout: vk::PipelineLayout,
    state: &GraphicsPipelineState,
    label: &str,
    pipeline_cache_directory_of_its_stream: Option<&Path>,
) -> Result<vk::Pipeline> {
    // Reject NV12 as a color attachment (planar-format output is not a
    // graphics-pipeline target).
    for (i, fmt) in state.attachment_formats.color.iter().enumerate() {
        if matches!(fmt, TextureFormat::Nv12) {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{label}': color attachment {i} format {fmt:?} is not a valid render target"
            )));
        }
    }
    if state.multisample.samples != 1 {
        return Err(Error::GpuError(format!(
            "Graphics kernel '{label}': multisample.samples = {} is not supported (only 1)",
            state.multisample.samples
        )));
    }

    // Hash the SPIR-V across all stages to key the pipeline cache.
    let cache_key = hash_stages(stages);
    let cache_path = pipeline_cache_file_path_keyed_by(
        PipelineCacheKernelKind::Graphics,
        &cache_key,
        pipeline_cache_directory_of_its_stream,
    );
    let initial_data = read_cache_blob(PipelineCacheKernelKind::Graphics, &cache_path);
    tracing::debug!(
        label,
        cache_path = ?cache_path,
        cache_hit = initial_data.is_some(),
        blob_bytes = initial_data.as_ref().map_or(0, |d| d.len()),
        "graphics pipeline cache lookup",
    );
    let pipeline_cache = create_pipeline_cache_handle(
        PipelineCacheKernelKind::Graphics,
        device,
        initial_data.as_deref(),
        label,
    );
    let cache_handle = pipeline_cache.unwrap_or(vk::PipelineCache::null());

    // Shader stages — match `shader_modules` order to `stages` order.
    let entry_names: Vec<std::ffi::CString> = stages
        .iter()
        .map(|s| std::ffi::CString::new(s.entry_point).expect("entry point must be ASCII"))
        .collect();
    let shader_stage_infos: Vec<vk::PipelineShaderStageCreateInfo> = stages
        .iter()
        .zip(shader_modules.iter())
        .zip(entry_names.iter())
        .map(|((stage_desc, (_, module)), name)| {
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(graphics_stage_to_vk(stage_desc.stage))
                .module(*module)
                .name(name.as_bytes_with_nul())
                .build()
        })
        .collect();

    // Vertex input.
    let (vertex_bindings, vertex_attributes) = match &state.vertex_input {
        VertexInputState::None => (Vec::new(), Vec::new()),
        VertexInputState::Buffers {
            bindings,
            attributes,
        } => {
            let vb: Vec<vk::VertexInputBindingDescription> = bindings
                .iter()
                .map(|b| {
                    vk::VertexInputBindingDescription::builder()
                        .binding(b.binding)
                        .stride(b.stride)
                        .input_rate(vertex_input_rate_to_vk(b.input_rate))
                        .build()
                })
                .collect();
            let va: Vec<vk::VertexInputAttributeDescription> = attributes
                .iter()
                .map(|a| {
                    vk::VertexInputAttributeDescription::builder()
                        .location(a.location)
                        .binding(a.binding)
                        .format(vertex_attribute_format_to_vk(a.format))
                        .offset(a.offset)
                        .build()
                })
                .collect();
            (vb, va)
        }
    };
    let vertex_input_info = vk::PipelineVertexInputStateCreateInfo::builder()
        .vertex_binding_descriptions(&vertex_bindings)
        .vertex_attribute_descriptions(&vertex_attributes)
        .build();

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::builder()
        .topology(primitive_topology_to_vk(state.topology))
        .primitive_restart_enable(false)
        .build();

    // Viewport state — declare 1 viewport + 1 scissor; either dynamic or
    // baked.
    let baked_viewport = [vk::Viewport::default()];
    let baked_scissor = [vk::Rect2D::default()];
    let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
        .viewports(&baked_viewport)
        .scissors(&baked_scissor)
        .build();

    let rasterizer = vk::PipelineRasterizationStateCreateInfo::builder()
        .polygon_mode(polygon_mode_to_vk(state.rasterization.polygon_mode))
        .cull_mode(cull_mode_to_vk(state.rasterization.cull_mode))
        .front_face(front_face_to_vk(state.rasterization.front_face))
        .line_width(state.rasterization.line_width)
        .depth_clamp_enable(false)
        .rasterizer_discard_enable(false)
        .depth_bias_enable(false)
        .build();

    let multisampling = vk::PipelineMultisampleStateCreateInfo::builder()
        .rasterization_samples(vk::SampleCountFlags::_1)
        .sample_shading_enable(false)
        .build();

    let depth_stencil_info = build_depth_stencil_info(&state.depth_stencil);

    let color_blend_attachments =
        build_color_blend_attachments(&state.color_blend, state.attachment_formats.color.len());
    let color_blend_state = vk::PipelineColorBlendStateCreateInfo::builder()
        .logic_op_enable(false)
        .logic_op(vk::LogicOp::COPY)
        .attachments(&color_blend_attachments)
        .blend_constants([0.0, 0.0, 0.0, 0.0])
        .build();

    let dynamic_states_buf: Vec<vk::DynamicState> = match state.dynamic_state {
        GraphicsDynamicState::None => Vec::new(),
        GraphicsDynamicState::ViewportScissor => {
            vec![vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR]
        }
    };
    let dynamic_state_info = vk::PipelineDynamicStateCreateInfo::builder()
        .dynamic_states(&dynamic_states_buf)
        .build();

    // Dynamic rendering: pNext on graphics pipeline create info.
    let color_attachment_formats: Vec<vk::Format> = state
        .attachment_formats
        .color
        .iter()
        .map(|f| texture_format_to_vk(*f))
        .collect();
    let mut rendering_info_builder = vk::PipelineRenderingCreateInfo::builder()
        .color_attachment_formats(&color_attachment_formats);
    if let Some(df) = state.attachment_formats.depth {
        rendering_info_builder =
            rendering_info_builder.depth_attachment_format(depth_format_to_vk(df));
    }
    let mut pipeline_rendering_info = rendering_info_builder.build();

    let mut pipeline_info_builder = vk::GraphicsPipelineCreateInfo::builder()
        .stages(&shader_stage_infos)
        .vertex_input_state(&vertex_input_info)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterizer)
        .multisample_state(&multisampling)
        .color_blend_state(&color_blend_state)
        .dynamic_state(&dynamic_state_info)
        .layout(pipeline_layout)
        .push_next(&mut pipeline_rendering_info);
    if matches!(state.depth_stencil, DepthStencilState::Enabled { .. }) {
        pipeline_info_builder = pipeline_info_builder.depth_stencil_state(&depth_stencil_info);
    }
    let pipeline_info = pipeline_info_builder.build();

    let pipelines_result =
        unsafe { device.create_graphics_pipelines(cache_handle, &[pipeline_info], None) };

    if pipeline_cache.is_some() {
        persist_pipeline_cache(
            PipelineCacheKernelKind::Graphics,
            device,
            cache_handle,
            &cache_path,
            label,
        );
        unsafe { device.destroy_pipeline_cache(cache_handle, None) };
    }

    let pipelines = pipelines_result.map_err(|e| {
        Error::GpuError(format!(
            "Graphics kernel '{label}': failed to create graphics pipeline: {e}"
        ))
    })?;
    Ok(pipelines.0[0])
}

fn build_depth_stencil_info(state: &DepthStencilState) -> vk::PipelineDepthStencilStateCreateInfo {
    match state {
        DepthStencilState::Disabled => vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(false)
            .depth_write_enable(false)
            .depth_compare_op(vk::CompareOp::ALWAYS)
            .depth_bounds_test_enable(false)
            .stencil_test_enable(false)
            .build(),
        DepthStencilState::Enabled {
            depth_test,
            depth_write,
        } => vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(true)
            .depth_write_enable(*depth_write)
            .depth_compare_op(depth_compare_op_to_vk(*depth_test))
            .depth_bounds_test_enable(false)
            .stencil_test_enable(false)
            .build(),
    }
}

fn build_color_blend_attachments(
    state: &ColorBlendState,
    count: usize,
) -> Vec<vk::PipelineColorBlendAttachmentState> {
    let template = match state {
        ColorBlendState::Disabled { color_write_mask } => {
            vk::PipelineColorBlendAttachmentState::builder()
                .blend_enable(false)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ZERO)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(color_write_mask_to_vk(*color_write_mask))
                .build()
        }
        ColorBlendState::Enabled(att) => vk::PipelineColorBlendAttachmentState::builder()
            .blend_enable(true)
            .src_color_blend_factor(blend_factor_to_vk(att.src_color_blend_factor))
            .dst_color_blend_factor(blend_factor_to_vk(att.dst_color_blend_factor))
            .color_blend_op(blend_op_to_vk(att.color_blend_op))
            .src_alpha_blend_factor(blend_factor_to_vk(att.src_alpha_blend_factor))
            .dst_alpha_blend_factor(blend_factor_to_vk(att.dst_alpha_blend_factor))
            .alpha_blend_op(blend_op_to_vk(att.alpha_blend_op))
            .color_write_mask(color_write_mask_to_vk(att.color_write_mask))
            .build(),
    };
    (0..count.max(1)).map(|_| template).collect()
}

// ---- Pipeline cache helpers (mirror compute kernel) -------------------------

fn hash_stages(stages: &[GraphicsStage<'_>]) -> String {
    let mut hasher = Sha256::new();
    for stage in stages {
        // Mix in the stage discriminant so two kernels with the same SPIR-V
        // bytes but different stage classification get distinct cache keys.
        hasher.update(match stage.stage {
            GraphicsShaderStage::Vertex => b"V".as_ref(),
            GraphicsShaderStage::Fragment => b"F".as_ref(),
        });
        hasher.update(stage.entry_point.as_bytes());
        hasher.update((stage.spv.len() as u64).to_le_bytes());
        hasher.update(stage.spv);
    }
    format!("{:x}", hasher.finalize())
}
