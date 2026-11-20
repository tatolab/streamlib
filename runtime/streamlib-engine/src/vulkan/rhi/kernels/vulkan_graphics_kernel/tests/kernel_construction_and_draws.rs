// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn new_rejects_zero_descriptor_sets_in_flight() {
    // descriptor_sets_in_flight = 0 must fail before any Vulkan call.
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let mut descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    descriptor.descriptor_sets_in_flight = 0;
    let err =
        VulkanGraphicsKernel::new(&device, &descriptor).expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("descriptor_sets_in_flight"),
        "expected zero-ring-depth error, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn new_rejects_descriptor_without_vertex_stage() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [GraphicsStage::fragment(frag_spv())]; // no vertex
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err =
        VulkanGraphicsKernel::new(&device, &descriptor).expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("Vertex stage"),
        "expected error about missing Vertex stage, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn new_rejects_descriptor_without_fragment_stage() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings: [GraphicsBindingSpec; 0] = [];
    let stages = [GraphicsStage::vertex(vert_spv())]; // no fragment
    let pipeline_state = default_pipeline_state();
    let mut descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    descriptor.push_constants = GraphicsPushConstants::NONE;
    let err =
        VulkanGraphicsKernel::new(&device, &descriptor).expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("Fragment stage"),
        "expected error about missing Fragment stage, got: {msg}"
    );
}

// ---- Pipeline construction (GPU device required) ----------------------

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn constructs_offscreen_blit_kernel() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let kernel = VulkanGraphicsKernel::new(&device, &descriptor).expect("kernel must construct");
    assert_eq!(kernel.bindings().len(), 1);
    assert_eq!(kernel.push_constant_size(), 16);
    assert_eq!(kernel.descriptor_sets_in_flight(), 2);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_sampled_texture_with_an_anisotropic_repeating_sampler_constructs() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let anisotropy =
        crate::vulkan::rhi::kernels::vulkan_kernel_samplers::the_most_anisotropy_the_rig_serves(
            &device,
        );
    let bindings = [
        GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT).with_sampler(
            crate::core::rhi::SamplerDescriptor {
                address_modes: [crate::core::rhi::SamplerAddressMode::Repeat; 3],
                mipmap_mode: crate::core::rhi::SamplerMipmapMode::Linear,
                anisotropy,
                ..crate::core::rhi::SamplerDescriptor::LINEAR_CLAMP
            },
        ),
    ];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let kernel =
        VulkanGraphicsKernel::new(&device, &descriptor).expect("the kernel must construct");
    let texture_descriptor =
        crate::core::rhi::TextureDescriptor::new(16, 16, TextureFormat::Bgra8Unorm)
            .with_usage(crate::core::rhi::TextureUsages::TEXTURE_BINDING);
    let texture =
        crate::vulkan::rhi::HostVulkanTexture::new_device_local(&device, &texture_descriptor)
            .expect("test texture allocation");
    kernel
        .set_sampled_texture(0, 0, &crate::core::rhi::Texture::from_vulkan(texture))
        .expect("the binding's sampler is created and bound");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn constructs_kernel_with_depth_stencil_enabled() {
    // Smoke test: pipeline creation succeeds when both
    // `depth_stencil = Enabled` and `attachment_formats.depth =
    // Some(...)` are set. This locks the *creation* path threading
    // `VkPipelineDepthStencilStateCreateInfo` through to the
    // pipeline builder — but does NOT lock depth-correctness
    // rendering, because depth Texture allocation depends on
    // `streamlib-consumer-rhi::TextureFormat` gaining depth
    // variants (tracked under the Graphics Kernel Buildout
    // milestone). A driver bug that silently ignored
    // `depth_test_enable` while accepting the pipeline create call
    // would still pass this test; the offscreen depth-ordering
    // test that would catch it is the deferred follow-up.
    // Validation layers (`VK_LOADER_LAYERS_ENABLE=*validation*`)
    // exercise the spec-VUID side of pipeline creation and cover
    // the "did we forget to plumb depth state" failure mode.
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let mut pipeline_state = default_pipeline_state();
    pipeline_state.depth_stencil = DepthStencilState::Enabled {
        depth_test: DepthCompareOp::LessOrEqual,
        depth_write: true,
    };
    pipeline_state.attachment_formats = AttachmentFormats {
        color: vec![TextureFormat::Bgra8Unorm],
        depth: Some(crate::core::rhi::DepthFormat::D32Sfloat),
    };
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let kernel = VulkanGraphicsKernel::new(&device, &descriptor)
        .expect("depth-stencil kernel must construct");
    assert_eq!(kernel.bindings().len(), 1);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn constructs_kernel_with_alpha_blending() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let mut pipeline_state = default_pipeline_state();
    pipeline_state.color_blend =
        ColorBlendState::Enabled(crate::core::rhi::ColorBlendAttachment::ALPHA_OVER);
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let _ = VulkanGraphicsKernel::new(&device, &descriptor)
        .expect("alpha-blending kernel must construct");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn constructs_kernel_with_vertex_input_buffers() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    // A pipeline that *declares* vertex input buffers — the offscreen_blit
    // shader doesn't read them, but Vulkan permits unused vertex input
    // declarations. We're locking the pipeline-creation path here, not
    // shader-vs-input correctness.
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let mut pipeline_state = default_pipeline_state();
    pipeline_state.vertex_input = VertexInputState::Buffers {
        bindings: vec![VertexInputBinding {
            binding: 0,
            stride: 32,
            input_rate: VertexInputRate::Vertex,
        }],
        attributes: vec![
            VertexInputAttribute {
                location: 1, // location 0 is the vertex shader's gl_VertexIndex-derived UV
                binding: 0,
                format: VertexAttributeFormat::Rgb32Float,
                offset: 0,
            },
            VertexInputAttribute {
                location: 2,
                binding: 0,
                format: VertexAttributeFormat::Rg32Float,
                offset: 12,
            },
        ],
    };
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let _ = VulkanGraphicsKernel::new(&device, &descriptor)
        .expect("vertex-input kernel must construct");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn frame_index_out_of_range_fails_loud() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let kernel = VulkanGraphicsKernel::new(&device, &descriptor).expect("kernel construction");

    // Allocate a dummy texture for the call. Doesn't matter what it is —
    // the frame_index check happens before the texture is dereferenced.
    let dummy_desc = crate::core::rhi::TextureDescriptor::new(16, 16, TextureFormat::Bgra8Unorm)
        .with_usage(
            crate::core::rhi::TextureUsages::TEXTURE_BINDING
                | crate::core::rhi::TextureUsages::COPY_DST,
        );
    let texture = crate::vulkan::rhi::HostVulkanTexture::new_device_local(&device, &dummy_desc)
        .expect("test texture allocation");
    let stream_texture = crate::core::rhi::Texture::from_vulkan(texture);

    // Ring depth is 2; index 2 must fail.
    let err = kernel
        .set_sampled_texture(2, 0, &stream_texture)
        .expect_err("expected out-of-range error");
    let msg = format!("{err}");
    assert!(
        msg.contains("frame_index 2 out of range"),
        "expected out-of-range error, got: {msg}"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dispatch_without_setting_bindings_fails_loud() {
    let device = match try_vulkan_device() {
        Some(d) => d,
        None => return,
    };
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let kernel = VulkanGraphicsKernel::new(&device, &descriptor).expect("kernel construction");
    let color_target_descriptor =
        crate::core::rhi::TextureDescriptor::new(16, 16, TextureFormat::Bgra8Unorm)
            .with_usage(crate::core::rhi::TextureUsages::RENDER_ATTACHMENT);
    let color_target = crate::core::rhi::Texture::from_vulkan(
        crate::vulkan::rhi::HostVulkanTexture::new_device_local(&device, &color_target_descriptor)
            .expect("color target allocation"),
    );
    // Nothing staged before the draw — it must fail with a clear error,
    // not corrupt GPU state.
    let draw = crate::core::rhi::DrawCall {
        vertex_count: 3,
        instance_count: 1,
        first_vertex: 0,
        first_instance: 0,
        viewport: Some(Viewport::full(16, 16)),
        scissor: Some(ScissorRect::full(16, 16)),
    };
    let err = kernel
        .offscreen_render(
            0,
            &[OffscreenColorTarget {
                texture: &color_target,
                clear_color: None,
            }],
            (16, 16),
            OffscreenDraw::Draw(draw),
        )
        .expect_err("expected missing-binding error");
    let msg = format!("{err}");
    assert!(
        msg.contains("not set before draw"),
        "expected missing-binding error, got: {msg}"
    );
}
