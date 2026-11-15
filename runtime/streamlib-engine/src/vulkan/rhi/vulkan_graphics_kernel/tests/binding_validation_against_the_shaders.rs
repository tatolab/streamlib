// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const SUBGROUP_ELECTING_VERTEX_SLANG: &str = r#"
struct BlitVertexOutput {
[[vk::location(0)]] float2 uv : TEXCOORD0;
float4 position : SV_Position;
};
BlitVertexOutput main(uint vertex_index : SV_VulkanVertexID) {
BlitVertexOutput output;
output.uv = float2((vertex_index << 1) & 2, vertex_index & 2);
float elected_lane_depth = WaveIsFirstLane() ? 0.0 : 0.5;
output.position = float4(output.uv * 2.0 - 1.0, elected_lane_depth, 1.0);
return output;
}
"#;
#[test]
fn a_vertex_stage_subgroup_operation_is_built_where_the_driver_serves_it_and_refused_by_name_where_not()
 {
    let Some(device) = try_vulkan_device() else {
        return;
    };
    let vertex_spv = crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            SUBGROUP_ELECTING_VERTEX_SLANG,
            crate::core::rhi::ShaderCompilationTargetStage::Vertex,
            "main",
            "subgroup-electing-vertex",
        )
        .expect("the subgroup-electing vertex stage compiles");
    let stages = [
        GraphicsStage::vertex(&vertex_spv),
        GraphicsStage::fragment(frag_spv()),
    ];
    let (bindings, _) = derive_bindings_from_spirv_multistage(&stages).expect("derive bindings");
    let pipeline_state = default_pipeline_state();
    let built = VulkanGraphicsKernel::new(
        &device,
        &offscreen_blit_descriptor(&stages, &bindings, &pipeline_state),
    );
    let support = device.subgroup_operation_support();
    if support
        .supported_stages
        .contains(vk::ShaderStageFlags::VERTEX)
        && support
            .supported_operations
            .contains(vk::SubgroupFeatureFlags::BASIC)
    {
        built.expect("a driver serving vertex-stage subgroup operations builds the kernel");
    } else {
        let refusal = built
            .expect_err(
                "a driver not serving vertex-stage subgroup operations refuses at construction",
            )
            .to_string();
        assert!(refusal.contains(&support.driver_name), "{refusal}");
        assert!(refusal.contains("VERTEX"), "{refusal}");
        assert!(refusal.contains("display-blit"), "{refusal}");
    }
}

// ---- Multi-stage SPIR-V reflection ------------------------------------

#[test]
fn derives_bindings_from_offscreen_blit_stages() {
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let (bindings, push) = derive_bindings_from_spirv_multistage(&stages).expect("derive bindings");
    assert_eq!(
        bindings.len(),
        1,
        "offscreen_blit declares one fragment binding"
    );
    assert_eq!(bindings[0].binding, 0);
    assert_eq!(bindings[0].kind, GraphicsBindingKind::SampledTexture);
    assert!(
        bindings[0]
            .stages
            .contains(GraphicsShaderStageFlags::FRAGMENT),
        "binding 0 is consumed by fragment stage"
    );
    assert_eq!(
        push.size, 16,
        "push constants are scale (vec2) + offset (vec2)"
    );
    assert!(push.stages.contains(GraphicsShaderStageFlags::FRAGMENT));
}

// ---- Validation rejections (host-only, no GPU device required) --------

#[test]
fn rejects_one_slot_the_two_stages_spell_differently() {
    // offscreen_blit's vertex stage declares no binding, so the second
    // spelling of slot 0 has to come from a rewrite of the fragment blob.
    let respelled = rename_binding_in_spirv_module(frag_spv(), "cameraTexture", "cam");
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::VERTEX_FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(&respelled),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("one slot cannot carry two names");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0 is named `cam`")
            && msg.contains("`cameraTexture`")
            && msg.contains("one slot spelled two ways"),
        "expected both spellings of slot 0, got: {msg}"
    );
}

#[test]
fn a_stage_whose_only_set_is_not_set_0_is_refused_at_validation() {
    let moved_to_set_1 = move_binding_to_another_descriptor_set_in_spirv_module(frag_spv(), 0, 1);
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(&moved_to_set_1),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let refusal = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a binding outside set 0 cannot be bound, so it cannot be dropped in silence");
    let message = format!("{refusal}");
    assert!(
        message.contains("only descriptor set 0 is supported") && message.contains('1'),
        "the refusal must name the unsupported set: {message}"
    );
}

#[test]
fn rejects_a_name_stripped_stage() {
    let stripped = strip_every_debug_name_from_spirv_module(frag_spv());
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(&stripped),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a name-stripped blob cannot be bound by name");
    let msg = format!("{err}");
    assert!(
        msg.contains("carries no name") && msg.contains("Slang source"),
        "the refusal must name the cause and the fix, got: {msg}"
    );
}

#[test]
fn rejects_one_name_the_two_stages_put_on_two_slots() {
    let moved = move_binding_to_another_slot_in_spirv_module(frag_spv(), 0, 1);
    let bindings = [
        GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::VERTEX_FRAGMENT),
        GraphicsBindingSpec::sampled_texture(1, GraphicsShaderStageFlags::VERTEX_FRAGMENT),
    ];
    let stages = [
        GraphicsStage::vertex(&moved),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("one name cannot identify two slots");
    let msg = format!("{err}");
    assert!(
        msg.contains("bindings 0 and 1 are both named `cameraTexture`")
            && msg.contains("one name on two slots"),
        "expected both slots and the name, got: {msg}"
    );
}

#[test]
fn rejects_a_declared_name_the_shader_spells_differently() {
    let bindings = [
        GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT)
            .with_name("sourceTexture"),
    ];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("a declared name the shader does not use must be refused");
    let msg = format!("{err}");
    assert!(
        msg.contains("declared name `sourceTexture`")
            && msg.contains("SPIR-V names it `cameraTexture`"),
        "expected both the declared and the reflected name, got: {msg}"
    );
}

#[test]
fn rejects_descriptor_with_mismatched_binding_kind() {
    // SPIR-V binding 0 is SampledTexture; declaring it as StorageBuffer must fail.
    let bindings = [GraphicsBindingSpec::storage_buffer(
        0,
        GraphicsShaderStageFlags::FRAGMENT,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0")
            && (msg.contains("StorageBuffer") || msg.contains("STORAGE_BUFFER")),
        "expected mismatch error mentioning binding 0 and StorageBuffer, got: {msg}"
    );
}

#[test]
fn rejects_descriptor_with_extra_binding_not_in_shader() {
    // offscreen_blit declares only binding 0; extra binding 1 must fail.
    let bindings = [
        GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
        GraphicsBindingSpec::sampled_texture(1, GraphicsShaderStageFlags::FRAGMENT),
    ];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 1"),
        "expected error about binding 1, got: {msg}"
    );
}

#[test]
fn rejects_descriptor_missing_a_binding_the_shader_declares() {
    // Empty bindings ⇒ SPIR-V's declared binding 0 is missing.
    let bindings: [GraphicsBindingSpec; 0] = [];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0") && msg.contains("missing"),
        "expected error about missing binding 0, got: {msg}"
    );
}

#[test]
fn rejects_push_constant_size_mismatch() {
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
    descriptor.push_constants = GraphicsPushConstants {
        size: 64, // SPIR-V declares 16
        stages: GraphicsShaderStageFlags::FRAGMENT,
    };
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("push-constant size mismatch"),
        "expected push-constant size error, got: {msg}"
    );
}

#[test]
fn rejects_descriptor_with_stage_visibility_too_narrow() {
    // SPIR-V uses binding 0 in fragment; declaring stages = VERTEX must fail.
    let bindings = [GraphicsBindingSpec::sampled_texture(
        0,
        GraphicsShaderStageFlags::VERTEX,
    )];
    let stages = [
        GraphicsStage::vertex(vert_spv()),
        GraphicsStage::fragment(frag_spv()),
    ];
    let pipeline_state = default_pipeline_state();
    let descriptor = offscreen_blit_descriptor(&stages, &bindings, &pipeline_state);
    let err = validate_against_spirv(
        &descriptor,
        &VulkanSubgroupOperationSupport::serving_every_operation_in_every_stage(),
    )
    .expect_err("expected validation failure");
    let msg = format!("{err}");
    assert!(
        msg.contains("binding 0") && msg.contains("stage visibility"),
        "expected stage visibility error for binding 0, got: {msg}"
    );
}
