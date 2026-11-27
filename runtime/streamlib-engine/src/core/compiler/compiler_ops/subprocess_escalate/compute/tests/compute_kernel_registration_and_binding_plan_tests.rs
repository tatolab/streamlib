// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Registering a compute kernel and the requests refused at registration, and
//! the binding plan that resolves each supplied name to the shader's slot.
//!
//! The named-binding cases are the point: a dispatch supplies every
//! binding the shader declares, by the shader's own name, exactly once.
//! They run against the binding planner directly rather than through a
//! GPU, because that is the layer the rules live at — and because
//! **duplicate is not expressible in a Python mapping**, so the wire
//! array is the only place it can be tested at all.

use super::*;

/// The Slang the wire carries instead of bytes: a read-one-write-another
/// pass, as an author would write it.
const READ_ONE_WRITE_ANOTHER_SLANG: &str = "\
[[vk::binding(0, 0)]] Sampler2D source_image;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> output_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    output_image[invocation.xy] = source_image.Load(int3(invocation.xy, 0));
}
";

/// A two-binding kernel shaped like the read-one-write-another pass
/// this whole change exists to make possible.
fn blur_kernel_bindings() -> Vec<ComputeBindingSpec> {
    vec![
        ComputeBindingSpec::sampled_texture(0).with_name("source_image"),
        ComputeBindingSpec::storage_image(1).with_name("output_image"),
    ]
}

fn plan_error(supplied: &[EscalateRequestRunComputeKernelBinding]) -> String {
    let declared = blur_kernel_bindings();
    let err = plan_supplied_compute_bindings(supplied, &declared)
        .expect_err("expected the plan to be refused");
    format!("{err}")
}

/// Neither and both are the two ways to get the alternatives wrong, and
/// each names the pair rather than picking one. Pure wire validation —
/// no device, so it runs everywhere CI does.
#[test]
fn a_register_op_supplying_neither_source_nor_spirv_is_refused_naming_both() {
    let message =
        registered_shader_stage_source("", "", "", ShaderCompilationTargetStage::Compute, "")
            .err()
            .expect("a register op with no shader at all must be refused");
    assert!(message.contains("source"), "{message}");
    assert!(message.contains("spv_hex"), "{message}");
}

#[test]
fn a_register_op_supplying_both_source_and_spirv_is_refused_naming_both() {
    let message = registered_shader_stage_source(
        "vertex_",
        READ_ONE_WRITE_ANOTHER_SLANG,
        "0badc0de",
        ShaderCompilationTargetStage::Vertex,
        "",
    )
    .err()
    .expect("supplying both alternatives must be refused");
    assert!(message.contains("vertex_source"), "{message}");
    assert!(message.contains("vertex_spv_hex"), "{message}");
}

/// A stage that disagrees with the op it arrived on is a caller
/// mistake, and the refusal has to name the only stage this op means.
#[test]
fn a_compute_register_op_carrying_another_stage_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("compute register op stage mismatch: no GPU — skipping");
        return;
    };
    let message = refusal_message(handle_register_compute_kernel(
        &sandbox,
        &EscalateHandleRegistry::default(),
        "rid-slang".to_string(),
        register_from_slang(READ_ONE_WRITE_ANOTHER_SLANG, "vertex"),
    ));
    assert!(message.contains("vertex"), "{message}");
    assert!(message.contains("compute"), "{message}");
}

/// A misspelling and a real-but-wrong stage are different mistakes and
/// get different answers — the first needs the list of stages that
/// exist, the second needs to know which one this op means.
#[test]
fn a_stage_that_is_not_a_stage_at_all_is_refused_naming_the_ones_that_are() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("compute register op unknown stage: no GPU — skipping");
        return;
    };
    let message = refusal_message(handle_register_compute_kernel(
        &sandbox,
        &EscalateHandleRegistry::default(),
        "rid-slang".to_string(),
        register_from_slang(READ_ONE_WRITE_ANOTHER_SLANG, "commpute"),
    ));
    assert!(message.contains("commpute"), "{message}");
    for stage in ShaderCompilationTargetStage::ALL {
        assert!(message.contains(stage.wire_name()), "{message}");
    }
}

/// At the wire: Slang text where bytes used to go,
/// with the binding names reflection found handed back.
#[test]
fn a_slang_source_registers_a_kernel_and_reports_its_binding_names() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register from Slang: no GPU — skipping");
        return;
    };
    let response = handle_register_compute_kernel(
        &sandbox,
        &EscalateHandleRegistry::default(),
        "rid-slang".to_string(),
        register_from_slang(READ_ONE_WRITE_ANOTHER_SLANG, "compute"),
    );
    let EscalateResponse::Ok(ok) = response else {
        panic!("expected Ok, got {response:?}");
    };
    let names: Vec<String> = ok
        .bindings
        .expect("a registered kernel reports its binding shape")
        .into_iter()
        .map(|binding| binding.name)
        .collect();
    assert!(
        names.contains(&"source_image".to_string()) && names.contains(&"output_image".to_string()),
        "expected the shader\'s own binding names, got {names:?}"
    );
}

/// Re-registering the same source costs no second compilation — the
/// assertion counts compiler invocations, never elapsed time, because
/// re-creation is free of compilation while still allocating handles.
#[test]
fn registering_the_same_slang_twice_compiles_it_once() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("Slang compile cache: no GPU — skipping");
        return;
    };
    let before = sandbox
        .host_inner()
        .slang_shader_compiler_invocation_count();
    for _ in 0..2 {
        let response = handle_register_compute_kernel(
            &sandbox,
            &EscalateHandleRegistry::default(),
            "rid-slang".to_string(),
            register_from_slang(READ_ONE_WRITE_ANOTHER_SLANG, "compute"),
        );
        assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
    }
    assert_eq!(
        sandbox
            .host_inner()
            .slang_shader_compiler_invocation_count()
            - before,
        1
    );
}

#[test]
fn a_complete_dispatch_resolves_every_name_to_its_slot() {
    let entries = supplied(&[
        (
            "output_image",
            EscalateComputeBindingKind::StorageImage,
            "surface-out",
        ),
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "surface-in",
        ),
    ]);
    let declared = blur_kernel_bindings();
    let planned = plan_supplied_compute_bindings(&entries, &declared)
        .expect("a complete, correctly-typed dispatch");

    // Resolution is by name, so the order the caller supplied them in
    // is not the order the shader declared them in — and that is fine.
    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].name, "output_image");
    assert_eq!(planned[0].binding, 1);
    assert_eq!(planned[0].kind, SurfaceBoundKernelBindingKind::StorageImage);
    assert_eq!(planned[0].target_id, "surface-out");
    assert_eq!(planned[1].name, "source_image");
    assert_eq!(planned[1].binding, 0);
    assert_eq!(planned[1].target_id, "surface-in");
}

/// Not expressible in a Python mapping — a dict cannot carry one key
/// twice — so the wire array is the only layer that can guard it.
#[test]
fn a_name_supplied_twice_is_refused() {
    let message = plan_error(&supplied(&[
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "surface-in",
        ),
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "surface-other",
        ),
        (
            "output_image",
            EscalateComputeBindingKind::StorageImage,
            "surface-out",
        ),
    ]));
    assert!(
        message.contains("`source_image` was supplied twice"),
        "must name the duplicate, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `output_image`"),
        "must name the shader's declared bindings, got: {message}"
    );
}

#[test]
fn a_name_the_shader_does_not_declare_is_refused() {
    let message = plan_error(&supplied(&[
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "surface-in",
        ),
        (
            "output_image",
            EscalateComputeBindingKind::StorageImage,
            "surface-out",
        ),
        (
            "sharpen_amount",
            EscalateComputeBindingKind::UniformBuffer,
            "surface-x",
        ),
    ]));
    assert!(
        message.contains("`sharpen_amount` is not one this shader declares"),
        "must name the unknown binding, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `output_image`"),
        "must name the shader's declared bindings, got: {message}"
    );
}

/// No implicit default and no carried-over value: the kernel holds no
/// binding state between dispatches to fall back on.
#[test]
fn a_declared_binding_left_out_is_refused() {
    let message = plan_error(&supplied(&[(
        "source_image",
        EscalateComputeBindingKind::SampledTexture,
        "surface-in",
    )]));
    assert!(
        message.contains("`output_image` was not supplied"),
        "must name the missing binding, got: {message}"
    );
    assert!(
        message.contains("do not persist between dispatches"),
        "must say why there is no fallback, got: {message}"
    );
    assert!(
        message.contains("`source_image`, `output_image`"),
        "must name the shader's declared bindings, got: {message}"
    );
}

#[test]
fn a_binding_supplied_as_the_wrong_kind_is_refused() {
    let message = plan_error(&supplied(&[
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "surface-in",
        ),
        (
            "output_image",
            EscalateComputeBindingKind::StorageBuffer,
            "surface-out",
        ),
    ]));
    assert!(
        message.contains("`output_image` was supplied as StorageBuffer"),
        "must name the binding and the kind supplied, got: {message}"
    );
    assert!(
        message.contains("declares it StorageImage"),
        "must name the kind the shader declares, got: {message}"
    );
}

/// A tensor storage buffer is a surface a dispatch binds, by the id the
/// engine minted for it.
#[test]
fn a_storage_buffer_binding_plans_as_a_storage_buffer() {
    let declared = vec![
        ComputeBindingSpec::sampled_texture(0).with_name("source_image"),
        ComputeBindingSpec::storage_buffer(1).with_name("model_input_tensor"),
    ];
    let entries = supplied(&[
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "surface-in",
        ),
        (
            "model_input_tensor",
            EscalateComputeBindingKind::StorageBuffer,
            "tensor-out",
        ),
    ]);
    let planned = plan_supplied_compute_bindings(&entries, &declared)
        .expect("a storage buffer is a bindable kind");
    assert_eq!(planned[1].name, "model_input_tensor");
    assert_eq!(planned[1].binding, 1);
    assert_eq!(
        planned[1].kind,
        SurfaceBoundKernelBindingKind::StorageBuffer
    );
    assert_eq!(planned[1].target_id, "tensor-out");
}

/// A uniform buffer is refused naming its kind, and the refusal does not
/// mention storage buffers.
#[test]
fn a_uniform_buffer_binding_is_refused_naming_its_kind() {
    let declared = vec![
        ComputeBindingSpec::storage_buffer(0).with_name("model_input_tensor"),
        ComputeBindingSpec::uniform_buffer(1).with_name("fit_parameters"),
    ];
    let entries = supplied(&[
        (
            "model_input_tensor",
            EscalateComputeBindingKind::StorageBuffer,
            "tensor-out",
        ),
        (
            "fit_parameters",
            EscalateComputeBindingKind::UniformBuffer,
            "surface-x",
        ),
    ]);
    let message = plan_supplied_compute_bindings(&entries, &declared)
        .expect_err("a uniform buffer is refused")
        .to_string();
    assert!(
        message.contains("`fit_parameters` is uniform_buffer, which a dispatch cannot bind"),
        "must name the binding and its kind, got: {message}"
    );
    assert!(
        !message.contains("storage_buffer"),
        "the refusal must not blame the storage buffer, got: {message}"
    );
}

/// A kernel with no bindings at all dispatches — the empty case is not
/// an error, and the "missing" rule has nothing to fire on.
#[test]
fn a_kernel_declaring_nothing_needs_nothing_supplied() {
    let planned = plan_supplied_compute_bindings(&[], &[]).expect("an unbound kernel dispatches");
    assert!(planned.is_empty());
}

/// Both hex fields are decoded before the escalate hop, so a malformed
/// one is refused without touching the GPU at all.
#[test]
fn register_with_invalid_spv_hex_is_refused_without_escalating() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_spv_hex: no GPU — skipping");
        return;
    };
    let response = handle_register_compute_kernel(
        &sandbox,
        &EscalateHandleRegistry::default(),
        "rid-1".to_string(),
        EscalateRequestRegisterComputeKernel {
            entry_point: "".to_string(),
            source: "".to_string(),
            stage: "".to_string(),
            bindings: Vec::new(),
            push_constant_size: 0,
            request_id: "rid-1".to_string(),
            spv_hex: "not-hex".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("spv_hex decode"),
            "got: {}",
            err.message
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}

#[test]
fn run_with_invalid_push_constants_hex_is_refused_without_escalating() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("run_with_invalid_push_constants_hex: no GPU — skipping");
        return;
    };
    let response = handle_run_compute_kernel(
        &sandbox,
        "rid-2".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: Vec::new(),
            group_count_x: 1,
            group_count_y: 1,
            group_count_z: 1,
            kernel_id: "whatever".to_string(),
            push_constants_hex: "zz".to_string(),
            request_id: "rid-2".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("push_constants_hex decode"),
            "got: {}",
            err.message
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}

/// Registration hands back the shape a dispatch needs: the shader's own
/// names, each with the kind only the shader knows.
#[test]
fn registration_answers_with_the_shaders_binding_names_and_kinds() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("registration_answers_with_the_shaders_bindings: no GPU — skipping");
        return;
    };
    let ok = register_read_one_write_another(&sandbox);
    let bindings = ok.bindings.expect("a register response carries the shape");
    assert_eq!(
        bindings
            .iter()
            .map(|b| (b.name.as_str(), b.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("source_image", "sampled_texture"),
            ("output_image", "storage_image"),
        ],
        "the two bindings differ in name and in kind, so binding by slot order \
                 rather than by name would swap them"
    );
}

/// Re-creating an identical kernel is free: same id, and the very same
/// kernel — counted by identity, never by elapsed time.
#[test]
fn re_registering_an_identical_kernel_is_a_cache_hit() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("re_registering_an_identical_kernel: no GPU — skipping");
        return;
    };
    let first = register_read_one_write_another(&sandbox);
    let second = register_read_one_write_another(&sandbox);
    assert_eq!(
        first.handle_id, second.handle_id,
        "an identical kernel keeps its id"
    );

    let held = sandbox
        .escalate(|full| {
            let a = full.compute_kernel_by_id(&first.handle_id);
            let b = full.compute_kernel_by_id(&second.handle_id);
            Ok((a, b))
        })
        .expect("the cache answers inside an escalate scope");
    let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "the second registration must reuse the first kernel, not build another"
    );
}

/// The cache key covers the blob, not the declaration — so the
/// declaration is checked on the hit path too, and a wrong assertion
/// refuses identically whether or not the blob was registered before.
#[test]
fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_wrong_declaration_is_refused_when_cached: no GPU — skipping");
        return;
    };
    // First registration warms the cache.
    register_read_one_write_another(&sandbox);

    let response = handle_register_compute_kernel(
        &sandbox,
        &EscalateHandleRegistry::default(),
        "reg-wrong".to_string(),
        EscalateRequestRegisterComputeKernel {
            entry_point: "".to_string(),
            source: "".to_string(),
            stage: "".to_string(),
            bindings: vec![EscalateRequestRegisterComputeKernelBinding {
                kind: EscalateComputeBindingKind::StorageBuffer,
                name: "sharpen_amount".to_string(),
            }],
            push_constant_size: 4,
            request_id: "reg-wrong".to_string(),
            spv_hex: read_one_write_another_spv_hex(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("`sharpen_amount`") && err.message.contains("`source_image`"),
            "the refusal must name the bogus binding and the shader's own: {}",
            err.message
        ),
        other => panic!("a wrong declaration must refuse on a cache hit, got {other:?}"),
    }
}
