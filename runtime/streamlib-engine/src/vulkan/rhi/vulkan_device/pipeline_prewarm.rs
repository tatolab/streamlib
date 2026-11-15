// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use std::sync::Arc;

use super::HostVulkanDevice;

impl HostVulkanDevice {
    /// Build (and immediately drop) a trivial compute pipeline at device
    /// construction so the GPU driver's shader-compiler global init runs
    /// single-threaded in a controlled state.
    ///
    /// **Provisional precaution, not a proven fix.** On NVIDIA Linux,
    /// `libnvidia-gpucomp` does a lazy `pthread_once` init on the first
    /// `vkCreatePipelineLayout` in a process; the working hypothesis is
    /// that running it concurrently with other multi-plugin processor setup
    /// can corrupt the driver's heap. Forcing it here — before any plugin
    /// setup runs — closes that one window, mirroring how the VMA pool
    /// pre-warm above sidesteps NVIDIA's exportable-memory cap. It does NOT
    /// fix the broader concurrent pipeline-creation contention: only the
    /// FIRST init is serialized, while later concurrent `vkCreate*Pipelines`
    /// calls still race. The canonical fix is funneling every pipeline
    /// creation through one dedicated compile thread (the concurrent-setup
    /// race, tracked separately); this pre-warm is the cheap precaution kept
    /// until that lands. Its causal necessity is unproven — the crash that
    /// originally motivated it was later diagnosed as an IPC/wiring failure,
    /// and the residual driver race is latent (not reproduced outside a
    /// debugger). Unconditional on Linux today (only NVIDIA is suspected to
    /// need it; vendor-id gating is a tracked consideration). Non-fatal: a
    /// pre-warm failure only forgoes the precaution — the first real
    /// pipeline triggers the init instead.
    ///
    /// **Boxed-in — treat as a removal candidate, not load-bearing code.**
    /// To prove necessity: delete the two `prewarm_*` calls in `new()`, run
    /// a multi-plugin NVIDIA setup (the drone-racer host) across a
    /// dozen-plus cold starts, and watch for a `libnvidia-glcore` /
    /// `vkCreate*Pipelines` SIGSEGV during setup. No crash ⇒ dead weight:
    /// remove the calls and the `prewarm_compute.slang` / `prewarm_{vertex,fragment}.slang`
    /// shaders. The single-threaded pipeline-create funnel supersedes it
    /// outright; don't grow it (e.g. a ray-tracing probe) on speculation
    /// before necessity is shown.
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
