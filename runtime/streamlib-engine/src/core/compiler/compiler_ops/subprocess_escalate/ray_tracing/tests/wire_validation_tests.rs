// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The acceleration-structure and ray-tracing escalate requests refused at
//! wire validation, before any device, and each wire stage compiled for the
//! stage it names.

use super::*;

/// Both blobs are decoded before the escalate hop, so a malformed one
/// is refused without touching the GPU at all.
#[test]
fn register_blas_with_invalid_vertex_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_blas_with_invalid_vertex_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "blas-bad-vertices",
                "xyz123",
                &index_hex(A_SCENES_TRIANGLE_INDICES),
            )),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("vertices_hex"), "got: {message}");
}

#[test]
fn register_blas_with_invalid_index_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_blas_with_invalid_index_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "blas-bad-indices",
                &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                "xyz123",
            )),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("indices_hex"), "got: {message}");
}

/// A blob that is not a whole number of vertices — or of triangles —
/// names geometry that does not exist, and is refused before a build.
#[test]
fn register_blas_with_a_partial_vertex_or_triangle_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_blas_with_a_partial_vertex_or_triangle: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    for (request_id, vertices_hex, indices_hex) in [
        (
            "blas-partial-vertex",
            "00".repeat(11),
            index_hex(A_SCENES_TRIANGLE_INDICES),
        ),
        (
            "blas-partial-triangle",
            vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            "00".repeat(8),
        ),
    ] {
        let message = refusal_message(
            handle_escalate_op(
                &sandbox,
                &registry,
                &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
                EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                    request_id,
                    &vertices_hex,
                    &indices_hex,
                )),
            )
            .expect("must produce a response"),
        );
        assert!(
            message.contains("multiple of 12"),
            "{request_id} got: {message}"
        );
    }
}

#[test]
fn register_tlas_with_no_instances_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_tlas_with_no_instances: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_tlas_req("tlas-empty", "unused");
    req.instances.clear();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureTlas(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("at least one instance"), "got: {message}");
}

#[test]
fn register_tlas_with_a_transform_that_is_not_a_row_major_3x4_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_tlas_with_a_wrong_length_transform: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_tlas_req("tlas-bad-transform", "unused");
    req.instances[0].transform = vec![1.0; 11];
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureTlas(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("transform"), "got: {message}");
}

#[test]
fn register_tlas_with_a_mask_wider_than_eight_bits_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_tlas_with_an_oversized_mask: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_tlas_req("tlas-bad-mask", "unused");
    req.instances[0].mask = 0xfff;
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureTlas(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("mask"), "got: {message}");
}

#[test]
fn run_with_invalid_push_constants_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("run_with_invalid_push_constants_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = make_run_req("trace-bad-push", "kernel-x", "tlas-x", "surface-x");
    req.push_constants_hex = "qq".to_string();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RunRayTracingKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("push_constants_hex"), "got: {message}");
}

/// A stage mask is a bitfield the caller writes by hand, and a bit
/// outside the six ray-tracing stages names a stage no pipeline has a
/// module for at all.
#[test]
fn a_stage_mask_naming_a_bit_no_ray_tracing_stage_owns_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_stage_mask_naming_an_unowned_bit: no GPU — skipping");
        return;
    };
    let bit_no_stage_owns = RayTracingShaderStageFlags::ALL.bits() + 1;

    let mut declaration = register_from_slang("k", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG);
    declaration.bindings[0].stages = bit_no_stage_owns;
    let message = prepare_ray_tracing_kernel_registration(&sandbox, declaration)
        .err()
        .expect("a binding declared for a stage no pipeline has must be refused");
    assert!(
        message.contains("scene_geometry") && message.contains("no ray-tracing stage owns"),
        "must name the binding and why the mask is wrong, got: {message}"
    );

    let mut push_constants = register_from_slang("k", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG);
    push_constants.push_constant_stages = bit_no_stage_owns;
    let message = prepare_ray_tracing_kernel_registration(&sandbox, push_constants)
        .err()
        .expect("a push-constant range declared for the same stage must be refused");
    assert!(
        message.contains("push_constant_stages"),
        "must name the field, got: {message}"
    );
}

/// A procedural hit group without an intersection stage is a group with
/// nothing to intersect, and the sentinel is what "absent" looks like on
/// a wire where the field is always present.
#[test]
fn a_procedural_group_leaving_intersection_at_the_sentinel_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_procedural_group_without_an_intersection: no GPU — skipping");
        return;
    };
    let mut req = register_from_slang("k-proc", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG);
    req.groups[2] = EscalateRequestRegisterRayTracingKernelGroup {
        kind: EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit,
        general_stage: RAY_TRACING_STAGE_INDEX_NONE,
        closest_hit_stage: 2,
        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
    };
    let message = prepare_ray_tracing_kernel_registration(&sandbox, req)
        .err()
        .expect("a procedural group with no intersection stage must be refused");
    assert!(message.contains("procedural_hit"), "got: {message}");
}

/// A stage's hex is decoded before the escalate hop, and the refusal
/// names the stage it came from rather than just "the kernel".
#[test]
fn register_with_invalid_stage_hex_returns_err() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register_with_invalid_stage_hex: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let mut req = register_from_slang("k-bad-hex", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG);
    req.stages[1].source = String::new();
    req.stages[1].spv_hex = "qq".to_string();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterRayTracingKernel(req),
        )
        .expect("must produce a response"),
    );
    assert!(message.contains("stages[1].spv_hex"), "got: {message}");
}

/// Every ray-tracing stage the wire can name compiles for the stage it
/// names, and reaches the kernel classified as that stage.
///
/// Two separate six-arm mappings run per stage —
/// `ray_tracing_pipeline_stage_from_wire` picks what the compiler
/// targets and `ray_tracing_stage_from_wire` picks what the shader
/// group is built from — so a swapped pair in either would build a miss
/// shader as a closest-hit without complaint. Slang compiles most of
/// these bodies for any ray-tracing stage, so the compiled module's own
/// `OpEntryPoint` execution model is what proves the compile target.
#[test]
fn every_ray_tracing_wire_stage_compiles_for_the_stage_it_names() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("every_ray_tracing_wire_stage_compiles: no GPU — skipping");
        return;
    };
    const SPIRV_EXECUTION_MODEL_RAY_GENERATION: u32 = 5313;
    const SPIRV_EXECUTION_MODEL_INTERSECTION: u32 = 5314;
    const SPIRV_EXECUTION_MODEL_ANY_HIT: u32 = 5315;
    const SPIRV_EXECUTION_MODEL_CLOSEST_HIT: u32 = 5316;
    const SPIRV_EXECUTION_MODEL_MISS: u32 = 5317;
    const SPIRV_EXECUTION_MODEL_CALLABLE: u32 = 5318;
    let payload = "struct RayPayload { float3 colour; };\n";
    let stages = [
        (
            EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
            RayTracingShaderStage::RayGen,
            SPIRV_EXECUTION_MODEL_RAY_GENERATION,
            "[[vk::binding(0, 0)]] [format(\"rgba8\")] RWTexture2D<float4> traced_output;\n\
             void main() { traced_output[DispatchRaysIndex().xy] = float4(1.0); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::Miss,
            RayTracingShaderStage::Miss,
            SPIRV_EXECUTION_MODEL_MISS,
            "void main(inout RayPayload payload) { payload.colour = float3(0.0); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
            RayTracingShaderStage::ClosestHit,
            SPIRV_EXECUTION_MODEL_CLOSEST_HIT,
            "void main(inout RayPayload payload, BuiltInTriangleIntersectionAttributes hit) \
             { payload.colour = float3(0.5); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::AnyHit,
            RayTracingShaderStage::AnyHit,
            SPIRV_EXECUTION_MODEL_ANY_HIT,
            "void main(inout RayPayload payload, BuiltInTriangleIntersectionAttributes hit) \
             { IgnoreHit(); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::Intersection,
            RayTracingShaderStage::Intersection,
            SPIRV_EXECUTION_MODEL_INTERSECTION,
            "struct SphereHit { float2 uv; };\n\
             void main() { SphereHit hit; hit.uv = float2(0.0); ReportHit(1.0, 0, hit); }",
        ),
        (
            EscalateRequestRegisterRayTracingKernelStageStage::Callable,
            RayTracingShaderStage::Callable,
            SPIRV_EXECUTION_MODEL_CALLABLE,
            "void main(inout RayPayload callable_payload) { callable_payload.colour = float3(1.0); }",
        ),
    ];
    for (index, (wire_stage, expected_stage, expected_execution_model, body)) in
        stages.into_iter().enumerate()
    {
        let mut req = register_from_slang(
            &format!("rt-slang-{index}"),
            TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_SLANG,
        );
        req.bindings = Vec::new();
        req.groups = vec![general_group(0)];
        req.stages = vec![stage_from_slang(wire_stage, &format!("{payload}{body}\n"))];
        let prepared = prepare_ray_tracing_kernel_registration(&sandbox, req)
            .unwrap_or_else(|e| panic!("{wire_stage:?} did not compile as itself: {e}"));
        assert_eq!(
            prepared.stages[0].spirv.get(..4),
            Some(&SPIRV_MAGIC_LE[..]),
            "{wire_stage:?} reached the kernel as something other than SPIR-V"
        );
        assert_eq!(
            spirv_entry_point_execution_model(&prepared.stages[0].spirv),
            Some(expected_execution_model),
            "{wire_stage:?} was compiled for the wrong pipeline stage"
        );
        assert_eq!(
            prepared.stages[0].stage, expected_stage,
            "{wire_stage:?} was classified as the wrong pipeline stage"
        );
    }
}

/// The execution model of a module's first `OpEntryPoint`.
fn spirv_entry_point_execution_model(spirv: &[u8]) -> Option<u32> {
    const SPIRV_HEADER_WORD_COUNT: usize = 5;
    const OP_ENTRY_POINT: u32 = 15;
    let words: Vec<u32> = spirv
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    let mut at = SPIRV_HEADER_WORD_COUNT;
    while at < words.len() {
        let word_count = (words[at] >> 16) as usize;
        if words[at] & 0xffff == OP_ENTRY_POINT {
            return words.get(at + 1).copied();
        }
        if word_count == 0 {
            return None;
        }
        at += word_count;
    }
    None
}
