// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use super::HostVulkanDevice;

impl HostVulkanDevice {
    pub(super) fn prewarm_pipeline_compiler(device: &Arc<Self>) {
        const PREWARM_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/prewarm.spv"));
        // Kitchen-sink layout — NOT modeled on any kernel. The driver's
        // lazy compiler init fires on the first pipeline whose shader needs
        // real codegen; a no-op shader can be fast-pathed and skip it. So
        // the probe touches every compute descriptor kind (storage buffers,
        // uniform buffer, sampled texture, storage image) + the spec-min-max
        // 128-byte push block, with enough work that the driver can't
        // fast-path it — forcing the init regardless of which feature
        // engages codegen. Mirror this in prewarm_compute.slang; don't trim to a
        // specific kernel's shape.
        let descriptor = crate::core::rhi::ComputeKernelDescriptor {
            entry_point: "main",
            label: "pipeline_compiler_prewarm",
            spv: PREWARM_SPV,
            bindings: &[
                crate::core::rhi::ComputeBindingSpec::storage_buffer(0),
                crate::core::rhi::ComputeBindingSpec::storage_buffer(1),
                crate::core::rhi::ComputeBindingSpec::uniform_buffer(2),
                crate::core::rhi::ComputeBindingSpec::sampled_texture(3),
                crate::core::rhi::ComputeBindingSpec::storage_image(4),
            ],
            push_constant_size: 128,
        };
        match crate::vulkan::rhi::VulkanComputeKernel::new(device, &descriptor) {
            // `_kernel` drops at the end of this arm — only the
            // compiler-init side effect persists process-globally.
            Ok(_kernel) => {
                tracing::info!("HostVulkanDevice pipeline-compiler pre-warmed");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "pipeline-compiler pre-warm failed (non-fatal — first real \
                     pipeline will trigger the driver's compiler init instead)"
                );
            }
        }
    }

    /// Build (and immediately drop) a trivial graphics pipeline at device
    /// construction so the driver's shader-compiler init for the
    /// `vkCreateGraphicsPipelines` path also runs single-threaded in a
    /// controlled state. Companion to [`Self::prewarm_pipeline_compiler`]
    /// (the compute path) — same provisional-precaution caveat; see it for
    /// why this is a precaution rather than a proven fix. Ray-tracing is a
    /// tracked follow-up. Trivial
    /// vertex + fragment stages, no vertex input, a dummy `Rgba8Unorm`
    /// color attachment via dynamic rendering, dynamic viewport/scissor —
    /// built and dropped purely for the compiler-init side effect.
    /// Non-fatal.
    pub(super) fn prewarm_graphics_pipeline(device: &Arc<Self>) {
        use crate::core::rhi::{
            AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState,
            GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor,
            GraphicsPipelineState, GraphicsPushConstants, GraphicsShaderStage,
            GraphicsShaderStageFlags, GraphicsStage, MultisampleState, PrimitiveTopology,
            RasterizationState, TextureFormat, VertexInputState,
        };
        const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/prewarm.vert.spv"));
        const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/prewarm.frag.spv"));
        const NO_BINDINGS: &[GraphicsBindingSpec] = &[];
        let stages = [
            GraphicsStage {
                stage: GraphicsShaderStage::Vertex,
                spv: VERT_SPV,
                entry_point: "main",
            },
            GraphicsStage {
                stage: GraphicsShaderStage::Fragment,
                spv: FRAG_SPV,
                entry_point: "main",
            },
        ];
        let descriptor = GraphicsKernelDescriptor {
            label: "pipeline_compiler_prewarm_graphics",
            stages: &stages,
            bindings: NO_BINDINGS,
            push_constants: GraphicsPushConstants {
                size: 4,
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
                attachment_formats: AttachmentFormats {
                    color: vec![TextureFormat::Rgba8Unorm],
                    depth: None,
                },
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 1,
        };
        match crate::vulkan::rhi::VulkanGraphicsKernel::new(device, &descriptor) {
            // `_kernel` drops here — only the compiler-init side effect persists.
            Ok(_kernel) => {
                tracing::info!("HostVulkanDevice graphics-pipeline compiler pre-warmed");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "graphics-pipeline pre-warm failed (non-fatal — first real \
                     graphics pipeline will trigger the compiler init instead)"
                );
            }
        }
    }
}
