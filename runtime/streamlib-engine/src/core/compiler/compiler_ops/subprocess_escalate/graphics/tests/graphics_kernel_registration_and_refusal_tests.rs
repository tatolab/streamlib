// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Registering a graphics kernel, and the registration and draw requests the
//! handlers refuse.

use super::*;

/// Both hex fields are decoded before the escalate hop, so a malformed
/// one is refused without touching the GPU at all.
#[test]
fn register_with_invalid_vertex_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_vertex_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::RegisterGraphicsKernel(make_register_req(
            "req-bad-v",
            "xyz123",
            "cafebabe",
        )),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-bad-v");
            assert!(
                err.message.contains("vertex_spv_hex"),
                "got: {}",
                err.message
            );
        }
        other => panic!("expected Err for malformed vertex hex, got {other:?}"),
    }
}

#[test]
fn register_with_invalid_fragment_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_fragment_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::RegisterGraphicsKernel(make_register_req("req-bad-f", "deadbeef", "qq")),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-bad-f");
            assert!(
                err.message.contains("fragment_spv_hex"),
                "got: {}",
                err.message
            );
        }
        other => panic!("expected Err for malformed fragment hex, got {other:?}"),
    }
}

#[test]
fn run_with_invalid_push_constants_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("run_with_invalid_push_constants_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("req-bad-push", "kernel-x", "surface-y");
    req.push_constants_hex = "xyz".to_string();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::RunGraphicsDraw(req),
    )
    .expect("must produce a response");
    match response {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-bad-push");
            assert!(
                err.message.contains("push_constants_hex"),
                "got: {}",
                err.message
            );
        }
        other => panic!("expected Err for malformed push hex, got {other:?}"),
    }
}

/// The three shapes the wire carries that the host has no path for.
/// Each is refused rather than silently dropped: a caller who sent one
/// would otherwise get a draw that ignored half of what it asked for.
#[test]
fn a_draw_naming_a_resource_no_escalate_op_mints_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_naming_an_unmintable_resource: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();

    let mut with_vertex_buffer = make_run_req("req-vb", "kernel-x", "surface-y");
    with_vertex_buffer.vertex_buffers = vec![EscalateRequestRunGraphicsDrawVertexBuffer {
        binding: 0,
        surface_uuid: "vb-uuid".to_string(),
        offset: "128".to_string(),
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(with_vertex_buffer),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("no escalate op mints a VertexBuffer"),
        "must say what is missing, got: {message}"
    );

    let mut indexed = make_run_req("req-ib", "kernel-x", "surface-y");
    indexed.index_buffer = Some(EscalateRequestRunGraphicsDrawIndexBuffer {
        surface_uuid: "ib-uuid".to_string(),
        offset: "64".to_string(),
        index_type: EscalateRequestRunGraphicsDrawIndexBufferIndexType::Uint32,
    });
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(indexed),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("an indexed draw needs an IndexBuffer"),
        "must say what is missing, got: {message}"
    );

    // The index buffer is what a `draw_indexed` names its indices in,
    // so the draw kind alone is refused for the same reason.
    let mut indexed_without_a_buffer = make_run_req("req-ib-kind", "kernel-x", "surface-y");
    indexed_without_a_buffer.draw.kind = EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed;
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(indexed_without_a_buffer),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("an indexed draw needs an IndexBuffer"),
        "must say what is missing, got: {message}"
    );
}

/// The offscreen pass attaches colour targets only, so a depth target
/// would never be tested against — and a caller who set one is asking
/// for depth testing that would not happen.
#[test]
fn a_draw_naming_a_depth_target_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_naming_a_depth_target: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("req-depth", "kernel-x", "surface-y");
    req.depth_target_uuid = Some("depth-uuid".to_string());
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("depth_target_uuid is set"),
        "must name the field, got: {message}"
    );
    assert!(
        message.contains("colour targets only"),
        "must say why, got: {message}"
    );
}

#[test]
fn a_draw_naming_other_than_one_colour_target_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_draw_naming_other_than_one_colour_target: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("req-targets", "kernel-x", "surface-y");
    req.color_target_uuids = vec!["a".to_string(), "b".to_string()];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("exactly one colour attachment"),
        "got: {message}"
    );
}

#[test]
fn drawing_with_an_unregistered_kernel_id_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("drawing_with_an_unregistered_kernel_id: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunGraphicsDraw(make_run_req(
                "req-bad-id",
                "never-registered",
                "surface-y",
            )),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("no kernel registered under id") && message.contains("never-registered"),
        "got: {message}"
    );
}

/// A stage mask is a bitfield the caller writes by hand, and a bit
/// outside vertex|fragment names a stage a graphics pipeline has no
/// module for at all.
#[test]
fn a_binding_declared_for_a_stage_no_graphics_pipeline_has_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_binding_declared_for_an_unowned_stage: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = register_from_slang("req-stage", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG);
    req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
        kind: EscalateGraphicsBindingKind::SampledTexture,
        name: "source_image".to_string(),
        stages: 0b100,
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterGraphicsKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("no graphics stage owns"),
        "must say the bit belongs to no stage, got: {message}"
    );
}

/// Slang where bytes used to go: the engine compiles each stage itself,
/// and what it hands the pipeline is a module rather than the text.
#[test]
fn slang_for_each_stage_reaches_the_engine_as_compiled_spirv() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("slang_for_each_stage_reaches_the_engine: no GPU — skipping");
        return;
    };
    for (field_prefix, source, stage) in [
        (
            "vertex_",
            FULL_SCREEN_TRIANGLE_VERTEX_SLANG,
            ShaderCompilationTargetStage::Vertex,
        ),
        (
            "fragment_",
            INVERT_SAMPLED_INPUT_FRAGMENT_SLANG,
            ShaderCompilationTargetStage::Fragment,
        ),
    ] {
        let compiled = registered_shader_stage_source(field_prefix, source, "", stage, "")
            .expect("Slang alone is one of the two alternatives")
            .spirv(&sandbox)
            .expect("the engine compiles it");
        assert_eq!(
            compiled.get(..4),
            Some(&SPIRV_MAGIC_LE[..]),
            "the {stage:?} stage reached the pipeline as something other than SPIR-V"
        );
    }
}

/// Registration hands back the shape a draw needs: the shaders' own
/// names, each with the kind only the shaders know. No bridge is
/// installed — graphics is a capability the context always has.
#[test]
fn registration_answers_with_the_shaders_binding_names_and_kinds() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("registration_answers_with_the_shaders_bindings: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let ok = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    );
    let bindings = ok.bindings.expect("a register response carries the shape");
    assert_eq!(
        bindings
            .iter()
            .map(|binding| (binding.name.as_str(), binding.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![("source_image", "sampled_texture")],
        "the fragment shader's own binding, named and kinded as it declares it"
    );
}

/// Re-registering an identical kernel is free and keeps its id; a
/// different fragment stage is a different pipeline and gets its own.
#[test]
fn an_identical_registration_keeps_its_kernel_id_and_a_different_one_gets_its_own() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("an_identical_registration_keeps_its_kernel_id: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let first = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("a", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;
    let second = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("b", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;
    assert_eq!(
        first, second,
        "an identical descriptor must produce the same kernel_id"
    );

    let other = register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("c", HALVE_SAMPLED_INPUT_FRAGMENT_SLANG),
    )
    .handle_id;
    assert_ne!(
        first, other,
        "a different fragment stage must produce a different kernel_id"
    );

    let held = sandbox
        .escalate(|full| {
            Ok((
                full.graphics_kernel_by_id_held_for_one_draw(&first)
                    .map(|held_kernel| {
                        std::ptr::from_ref::<crate::vulkan::rhi::VulkanGraphicsKernel>(
                            held_kernel.kernel(),
                        )
                    }),
                full.graphics_kernel_by_id_held_for_one_draw(&second)
                    .map(|held_kernel| {
                        std::ptr::from_ref::<crate::vulkan::rhi::VulkanGraphicsKernel>(
                            held_kernel.kernel(),
                        )
                    }),
            ))
        })
        .expect("the cache answers inside an escalate scope");
    // Compared by address once each lock is let go: a cached kernel lives as
    // long as the context, so the address still names it.
    let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
    assert!(
        std::ptr::eq(a, b),
        "the second registration must reuse the first kernel, not build another"
    );
}

/// The cache key covers the shaders and the pipeline, not the caller's
/// assertion — so a wrong declaration refuses identically whether or
/// not somebody registered this kernel first.
#[test]
fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_wrong_declaration_is_refused_when_cached: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    register_graphics_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("warm", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG),
    );

    let mut req = register_from_slang("reg-wrong", INVERT_SAMPLED_INPUT_FRAGMENT_SLANG);
    req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
        kind: EscalateGraphicsBindingKind::StorageBuffer,
        name: "sharpen_amount".to_string(),
        stages: 0,
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterGraphicsKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("`sharpen_amount`") && message.contains("`source_image`"),
        "the refusal must name the bogus binding and the shaders' own: {message}"
    );
}
