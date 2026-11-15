// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::rhi::spirv_module_rewriting_for_tests::{
    move_binding_to_another_descriptor_set_in_spirv_module,
    move_binding_to_another_slot_in_spirv_module, rename_binding_in_spirv_module,
    strip_every_debug_name_from_spirv_module,
};
use crate::core::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthCompareOp, DepthStencilState,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor, GraphicsPipelineState,
    GraphicsPushConstants, GraphicsShaderStageFlags, GraphicsStage, MultisampleState,
    PrimitiveTopology, RasterizationState, ScissorRect, TextureFormat, VertexAttributeFormat,
    VertexInputAttribute, VertexInputBinding, VertexInputRate, VertexInputState, Viewport,
    derive_bindings_from_spirv_multistage,
};
use crate::host_rhi::HostTextureExt;

use super::spirv_validation::validate_against_spirv;
use crate::vulkan::rhi::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;

fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
    match HostVulkanDevice::new() {
        Ok(d) => Some(d),
        Err(_) => {
            println!("Skipping - no Vulkan device available");
            None
        }
    }
}

fn vert_spv() -> &'static [u8] {
    include_bytes!(concat!(env!("OUT_DIR"), "/offscreen_blit_vertex.slang.spv"))
}
fn frag_spv() -> &'static [u8] {
    include_bytes!(concat!(
        env!("OUT_DIR"),
        "/offscreen_blit_fragment.slang.spv"
    ))
}

fn default_pipeline_state() -> GraphicsPipelineState {
    GraphicsPipelineState {
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
    }
}

fn offscreen_blit_descriptor<'a>(
    stages: &'a [GraphicsStage<'a>],
    bindings: &'a [GraphicsBindingSpec],
    pipeline_state: &'a GraphicsPipelineState,
) -> GraphicsKernelDescriptor<'a> {
    GraphicsKernelDescriptor {
        label: "display-blit",
        stages,
        bindings,
        push_constants: GraphicsPushConstants {
            size: 16,
            stages: GraphicsShaderStageFlags::FRAGMENT,
        },
        pipeline_state: pipeline_state.clone(),
        descriptor_sets_in_flight: 2,
    }
}

mod binding_validation_against_the_shaders;
mod kernel_construction_and_draws;
