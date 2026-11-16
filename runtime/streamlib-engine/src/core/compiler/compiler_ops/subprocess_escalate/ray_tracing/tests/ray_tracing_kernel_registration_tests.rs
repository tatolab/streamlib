// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Registering a ray-tracing kernel over a ray-tracing device: the binding
//! names and kinds it answers with, kernel-id reuse, and refused declarations.

use super::*;

/// Registration hands back the shape a trace needs: the shaders' own
/// names, each with the kind only the shaders know.
#[test]
fn registration_answers_with_the_shaders_binding_names_and_kinds() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("registration_answers_with_the_shaders_bindings: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let ok = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("reg", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG),
    );
    let bindings = ok.bindings.expect("a register response carries the shape");
    assert_eq!(
        bindings
            .iter()
            .map(|binding| (binding.name.as_str(), binding.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("scene_geometry", "acceleration_structure"),
            ("traced_output", "storage_image"),
        ],
        "the raygen's own bindings, named and kinded as it declares them"
    );
}

/// Re-registering an identical kernel is free and keeps its id; a
/// different raygen stage is a different pipeline and gets its own.
#[test]
fn an_identical_registration_keeps_its_kernel_id_and_a_different_one_gets_its_own() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("an_identical_registration_keeps_its_kernel_id: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let first = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("a", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG),
    )
    .handle_id;
    let second = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("b", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG),
    )
    .handle_id;
    assert_eq!(
        first, second,
        "an identical descriptor must produce the same kernel_id"
    );

    let other = register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("c", TRACE_AND_INVERT_RAY_GEN_SLANG),
    )
    .handle_id;
    assert_ne!(
        first, other,
        "a different raygen stage must produce a different kernel_id"
    );

    let held = sandbox
        .escalate(|full| {
            Ok((
                full.ray_tracing_kernel_by_id_held_for_one_trace(&first)
                    .map(|held_kernel| {
                        std::ptr::from_ref::<crate::vulkan::rhi::VulkanRayTracingKernel>(
                            held_kernel.kernel(),
                        )
                    }),
                full.ray_tracing_kernel_by_id_held_for_one_trace(&second)
                    .map(|held_kernel| {
                        std::ptr::from_ref::<crate::vulkan::rhi::VulkanRayTracingKernel>(
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
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("a_wrong_declaration_is_refused_when_cached: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    register_ray_tracing_kernel_or_panic(
        &sandbox,
        &registry,
        register_from_slang("warm", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG),
    );

    let mut req = register_from_slang("reg-wrong", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG);
    req.bindings = vec![EscalateRequestRegisterRayTracingKernelBinding {
        kind: EscalateRayTracingBindingKind::StorageBuffer,
        name: "scene_parameters".to_string(),
        stages: 0,
    }];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatAHelperPlacedNodeReaches::default(),
            EscalateRequest::RegisterRayTracingKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("`scene_parameters`") && message.contains("`scene_geometry`"),
        "the refusal must name the bogus binding and the shaders' own: {message}"
    );
}
