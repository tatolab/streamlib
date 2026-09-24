// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compute-kernel handler tests.
//!
//! The named-binding cases are the point: a dispatch supplies every
//! binding the shader declares, by the shader's own name, exactly once.
//! They run against the binding planner directly rather than through a
//! GPU, because that is the layer the rules live at — and because
//! **duplicate is not expressible in a Python mapping**, so the wire
//! array is the only place it can be tested at all.

use super::super::kernel_shader_stage_source::registered_shader_stage_source;
use super::linux::{
    handle_register_compute_kernel, handle_run_compute_kernel, handle_run_compute_kernel_batch,
    plan_supplied_compute_bindings,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateComputeBindingKind, EscalateRequestRegisterComputeKernel,
    EscalateRequestRegisterComputeKernelBinding, EscalateRequestRunComputeKernel,
    EscalateRequestRunComputeKernelBatch, EscalateRequestRunComputeKernelBinding,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;
use crate::core::context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuContext,
    GpuContextLimitedAccess, TexturePoolDescriptor, TextureRegistration,
};
use crate::core::rhi::{
    ComputeBindingSpec, GlslCompilationTargetStage, SurfaceBoundKernelBindingKind, TextureFormat,
    TextureUsages,
};
use crate::host_rhi::HostTextureExt;

/// Compute is an always-present capability now, so there is no bridge
/// to install — only a device to have or not have.
fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
    GpuContext::init_for_platform_sync()
        .ok()
        .map(GpuContextLimitedAccess::new)
}

/// The GLSL the wire now carries instead of bytes: the same
/// read-one-write-another pass, as an author would write it.
const READ_ONE_WRITE_ANOTHER_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D output_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    imageStore(output_image, at, texelFetch(source_image, at, 0));
}
";

fn register_from_glsl(source: &str, stage: &str) -> EscalateRequestRegisterComputeKernel {
    EscalateRequestRegisterComputeKernel {
        bindings: Vec::new(),
        push_constant_size: 0,
        request_id: "rid-glsl".to_string(),
        source: source.to_string(),
        stage: stage.to_string(),
        entry_point: String::new(),
        spv_hex: String::new(),
    }
}

fn refusal_message(response: EscalateResponse) -> String {
    match response {
        EscalateResponse::Err(err) => err.message,
        other => panic!("expected Err, got {other:?}"),
    }
}

/// Neither and both are the two ways to get the alternatives wrong, and
/// each names the pair rather than picking one. Pure wire validation —
/// no device, so it runs everywhere CI does.
#[test]
fn a_register_op_supplying_neither_source_nor_spirv_is_refused_naming_both() {
    let message =
        registered_shader_stage_source("", "", "", GlslCompilationTargetStage::Compute, "")
            .err()
            .expect("a register op with no shader at all must be refused");
    assert!(message.contains("source"), "{message}");
    assert!(message.contains("spv_hex"), "{message}");
}

#[test]
fn a_register_op_supplying_both_source_and_spirv_is_refused_naming_both() {
    let message = registered_shader_stage_source(
        "vertex_",
        READ_ONE_WRITE_ANOTHER_GLSL,
        "0badc0de",
        GlslCompilationTargetStage::Vertex,
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
        "rid-glsl".to_string(),
        register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "vertex"),
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
        "rid-glsl".to_string(),
        register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "commpute"),
    ));
    assert!(message.contains("commpute"), "{message}");
    for stage in GlslCompilationTargetStage::ALL {
        assert!(message.contains(stage.wire_name()), "{message}");
    }
}

/// The ticket's demo, at the wire: GLSL text where bytes used to go,
/// with the binding names reflection found handed back.
#[test]
fn a_glsl_source_registers_a_kernel_and_reports_its_binding_names() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("register from GLSL: no GPU — skipping");
        return;
    };
    let response = handle_register_compute_kernel(
        &sandbox,
        "rid-glsl".to_string(),
        register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "compute"),
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
fn registering_the_same_glsl_twice_compiles_it_once() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("GLSL compile cache: no GPU — skipping");
        return;
    };
    let before = sandbox.host_inner().glsl_shader_compiler_invocation_count();
    for _ in 0..2 {
        let response = handle_register_compute_kernel(
            &sandbox,
            "rid-glsl".to_string(),
            register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "compute"),
        );
        assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
    }
    assert_eq!(
        sandbox.host_inner().glsl_shader_compiler_invocation_count() - before,
        1
    );
}

/// A two-binding kernel shaped like the read-one-write-another pass
/// this whole change exists to make possible.
fn blur_kernel_bindings() -> Vec<ComputeBindingSpec> {
    vec![
        ComputeBindingSpec::sampled_texture(0).with_name("source_image"),
        ComputeBindingSpec::storage_image(1).with_name("output_image"),
    ]
}

fn supplied(
    entries: &[(&str, EscalateComputeBindingKind, &str)],
) -> Vec<EscalateRequestRunComputeKernelBinding> {
    entries
        .iter()
        .map(
            |(name, kind, target_id)| EscalateRequestRunComputeKernelBinding {
                kind: *kind,
                name: (*name).to_string(),
                target_id: (*target_id).to_string(),
            },
        )
        .collect()
}

fn plan_error(supplied: &[EscalateRequestRunComputeKernelBinding]) -> String {
    let declared = blur_kernel_bindings();
    let err = plan_supplied_compute_bindings(supplied, &declared)
        .err()
        .expect("expected the plan to be refused");
    format!("{err}")
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

/// The SPIR-V for the pass the v1 wire could not express — one
/// sampled input, one storage output, deliberately different kinds.
const READ_ONE_WRITE_ANOTHER_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/test_read_one_write_another.spv"));

fn read_one_write_another_spv_hex() -> String {
    READ_ONE_WRITE_ANOTHER_SPV
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn register_read_one_write_another(sandbox: &GpuContextLimitedAccess) -> EscalateResponseOk {
    let response = handle_register_compute_kernel(
        sandbox,
        "reg".to_string(),
        EscalateRequestRegisterComputeKernel {
            entry_point: "".to_string(),
            source: "".to_string(),
            stage: "".to_string(),
            bindings: Vec::new(),
            push_constant_size: 4,
            request_id: "reg".to_string(),
            spv_hex: read_one_write_another_spv_hex(),
        },
    );
    match response {
        EscalateResponse::Ok(ok) => ok,
        other => panic!("registering the conformance kernel failed: {other:?}"),
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

/// Each seed channel inverts exactly in unorm8: out = 255 - in.
const SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
const INVERTED_RGBA: [u8; 4] = [245, 235, 225, 255];

/// The whole point of the change: two surfaces, bound by the shader's
/// own names, and the output pixels prove the source was read.
///
/// Not just "the dispatch was accepted": the source is seeded with a
/// known value and the output is read back and compared against the
/// shader's own arithmetic, so binding the two names backwards — the
/// exact failure a by-slot resolution would produce, since both
/// textures share extent, format and usage — fails the assertion
/// rather than passing silently.
///
/// The textures are registered in-process rather than acquired over
/// the escalate op, because an escalate-acquired texture is published
/// to the surface-share service and resolves through it — and this
/// test has no service. The subject here is the dispatch.
#[test]
fn a_dispatch_reads_one_surface_and_writes_another() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_dispatch_reads_one_surface_and_writes_another: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;

    // Held for the dispatch: dropping a pooled handle hands its slot
    // back, and the registration would then name a recycled texture.
    let held = sandbox
        .escalate(|full| {
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING
                    | TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST,
            );
            let source = full.acquire_texture(&desc)?;
            let output = full.acquire_texture(&desc)?;
            full.register_texture("conformance-source", source.texture().clone());
            full.register_texture("conformance-output", output.texture().clone());

            // Seed the source with a constant the shader's arithmetic
            // transforms recognizably.
            let (_pool_id, seed_buffer) =
                full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(64 * 64) {
                    std::ptr::copy_nonoverlapping(SEED_RGBA.as_ptr(), plane.add(pixel * 4), 4);
                }
            }
            full.copy_pixel_buffer_to_texture(
                &seed_buffer,
                source.texture(),
                "conformance-source",
                64,
                64,
            )?;
            Ok((source, output))
        })
        .expect("two pooled textures, the source seeded");
    let (source_id, output_id) = ("conformance-source", "conformance-output");

    let response = handle_run_compute_kernel(
        &sandbox,
        "run".to_string(),
        EscalateRequestRunComputeKernel {
            // Supplied in the reverse of declaration order, so a
            // resolution that walked slots instead of names would bind
            // the two backwards and fail the pixel assertion below.
            bindings: vec![
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::StorageImage,
                    name: "output_image".to_string(),
                    target_id: output_id.to_string(),
                },
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::SampledTexture,
                    name: "source_image".to_string(),
                    target_id: source_id.to_string(),
                },
            ],
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id: kernel_id.clone(),
            push_constants_hex: "00000000".to_string(),
            request_id: "run".to_string(),
        },
    );
    match response {
        EscalateResponse::Ok(ok) => assert_eq!(ok.handle_id, kernel_id),
        other => panic!("the read-one-write-another dispatch failed: {other:?}"),
    }

    // The dispatch retired before the response, so the output is
    // readable now — compute leaves a storage image in GENERAL.
    let output_pixels = sandbox
        .escalate(|full| {
            let readback = full.create_texture_readback(
                "conformance-readback",
                64,
                64,
                TextureFormat::Rgba8Unorm,
            )?;
            let ticket = readback.submit(
                held.1.texture(),
                crate::core::rhi::TextureSourceLayout::General,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the output texture reads back");

    for (pixel_index, pixel) in output_pixels.chunks_exact(4).enumerate() {
        assert_eq!(
            pixel, INVERTED_RGBA,
            "pixel {pixel_index} must be the inverted seed — the kernel read \
             `source_image` and wrote `output_image`, by name"
        );
    }
    drop(held);
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

/// A pixel-buffer surface is a legal id a Python caller can hold, and
/// binding it must refuse by name — not fall into the buffer→texture
/// synthesis path with a zero extent.
///
/// The second half is the guard's real substance: a legitimate
/// resolver's cached canvas for the same slot must survive the refused
/// dispatch. Without the zero-extent guard the refusal still fires
/// (the 0×0 create fails), but only after evicting that canvas — so
/// this test resolves the surface at its real extent before and after,
/// and asserts the same texture comes back.
#[test]
fn binding_a_buffer_backed_surface_is_refused_by_name() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("binding_a_buffer_backed_surface: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;

    let (buffer_surface_id, held_buffer) = sandbox
        .escalate(|full| {
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING);
            let output = full.acquire_texture(&desc)?;
            full.register_texture("buffer-refusal-output", output.texture().clone());
            let (pool_id, buffer) =
                full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
            Ok((pool_id.to_string(), (buffer, output)))
        })
        .expect("a pixel buffer and an output texture");

    // A legitimate resolve at the real extent populates the slot's
    // cached canvas — the thing the refused dispatch must not evict.
    // The registration is held alive across the test: were the canvas
    // evicted and recreated, the driver could hand the replacement a
    // recycled handle value, and comparing dead handles would lie.
    let registration_before = sandbox
        .escalate(|full| {
            full.resolve_texture_registration_by_surface_id(&buffer_surface_id, None, 64, 64)
        })
        .expect("the buffer surface resolves at its real extent");
    let canvas_before = registration_before.texture().vulkan_inner().image();

    let response = handle_run_compute_kernel(
        &sandbox,
        "run-buffer".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: vec![
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::SampledTexture,
                    name: "source_image".to_string(),
                    target_id: buffer_surface_id.clone(),
                },
                EscalateRequestRunComputeKernelBinding {
                    kind: EscalateComputeBindingKind::StorageImage,
                    name: "output_image".to_string(),
                    target_id: "buffer-refusal-output".to_string(),
                },
            ],
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id,
            push_constants_hex: "00000000".to_string(),
            request_id: "run-buffer".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("`source_image`")
                && err.message.contains("cannot resolve to a device texture"),
            "must name the binding and refuse it as a non-texture: {}",
            err.message
        ),
        other => panic!("a buffer-backed binding must refuse, got {other:?}"),
    }

    let canvas_after = sandbox
        .escalate(|full| {
            let registration =
                full.resolve_texture_registration_by_surface_id(&buffer_surface_id, None, 64, 64)?;
            Ok(registration.texture().vulkan_inner().image())
        })
        .expect("the buffer surface still resolves after the refused dispatch");
    assert_eq!(
        canvas_before, canvas_after,
        "the refused dispatch must not evict the slot's cached canvas — a fresh \
         texture here means the zero-extent guard fired after the eviction, not before"
    );
    drop(registration_before);
    drop(held_buffer);
}

#[test]
fn dispatching_an_unregistered_kernel_id_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("dispatching_an_unregistered_kernel_id: no GPU — skipping");
        return;
    };
    let response = handle_run_compute_kernel(
        &sandbox,
        "rid-3".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: Vec::new(),
            group_count_x: 1,
            group_count_y: 1,
            group_count_z: 1,
            kernel_id: "never-registered".to_string(),
            push_constants_hex: String::new(),
            request_id: "rid-3".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("no kernel registered under id"),
            "got: {}",
            err.message
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestRunComputeKernelBatchDispatch;

/// Pass 1 of the chain: every channel gains 40/255.
const BRIGHTEN_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D unbrightened_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D brightened_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    vec4 source = texelFetch(unbrightened_image, at, 0);
    imageStore(brightened_image, at, vec4(source.rgb + 40.0 / 255.0, source.a));
}
";

/// Pass 2 of the chain: every channel doubles. Deliberately not
/// commutative with pass 1, so running them in the wrong order — or
/// running pass 2 against pass 1's *input* — lands on different pixels.
const DOUBLE_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D brightened_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D doubled_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    vec4 source = texelFetch(brightened_image, at, 0);
    imageStore(doubled_image, at, vec4(source.rgb * 2.0, source.a));
}
";

/// 10,20,30 brightened by 40 is 50,60,70; doubled is 100,120,140.
const CHAIN_SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
const CHAIN_BRIGHTENED_RGBA: [u8; 4] = [50, 60, 70, 255];
const CHAIN_DOUBLED_RGBA: [u8; 4] = [100, 120, 140, 255];

fn register_glsl_kernel(sandbox: &GpuContextLimitedAccess, source: &str) -> String {
    let response = handle_register_compute_kernel(
        sandbox,
        "reg-chain".to_string(),
        register_from_glsl(source, "compute"),
    );
    match response {
        EscalateResponse::Ok(ok) => ok.handle_id,
        other => panic!("registering a chain kernel failed: {other:?}"),
    }
}

fn batched_dispatch(
    kernel_id: &str,
    source_binding: (&str, &str),
    output_binding: (&str, &str),
) -> EscalateRequestRunComputeKernelBatchDispatch {
    EscalateRequestRunComputeKernelBatchDispatch {
        bindings: vec![
            EscalateRequestRunComputeKernelBinding {
                kind: EscalateComputeBindingKind::SampledTexture,
                name: source_binding.0.to_string(),
                target_id: source_binding.1.to_string(),
            },
            EscalateRequestRunComputeKernelBinding {
                kind: EscalateComputeBindingKind::StorageImage,
                name: output_binding.0.to_string(),
                target_id: output_binding.1.to_string(),
            },
        ],
        group_count_x: 8,
        group_count_y: 8,
        group_count_z: 1,
        kernel_id: kernel_id.to_string(),
        push_constants_hex: String::new(),
    }
}

/// The requested 64×64 textures registered under fixed ids, the first
/// seeded with [`CHAIN_SEED_RGBA`]. Returned held: dropping a pooled
/// handle hands its slot back, and the registration would then name a
/// recycled texture.
fn seeded_chain_textures<const TEXTURE_COUNT: usize>(
    sandbox: &GpuContextLimitedAccess,
    ids: [&str; TEXTURE_COUNT],
) -> Vec<crate::core::context::PooledTextureHandle> {
    sandbox
        .escalate(|full| {
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING
                    | TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST,
            );
            let mut held = Vec::with_capacity(ids.len());
            for id in ids {
                let texture = full.acquire_texture(&desc)?;
                full.register_texture(id, texture.texture().clone());
                held.push(texture);
            }

            let (_pool_id, seed_buffer) =
                full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
            let plane = seed_buffer.buffer_ref().plane_base_address(0);
            unsafe {
                for pixel in 0..(64 * 64) {
                    std::ptr::copy_nonoverlapping(
                        CHAIN_SEED_RGBA.as_ptr(),
                        plane.add(pixel * 4),
                        4,
                    );
                }
            }
            full.copy_pixel_buffer_to_texture(&seed_buffer, held[0].texture(), ids[0], 64, 64)?;
            Ok(held)
        })
        .expect("the pooled textures, the first seeded")
}

/// Read a surface back, sourcing the readback barrier from the layout
/// the surface is actually tracked in.
///
/// Not a hardcoded `General`: a batch leaves a sampled binding in
/// SHADER_READ_ONLY_OPTIMAL, and a barrier whose `oldLayout` disagrees
/// with the image makes the contents undefined by spec — so a test
/// asserting on those pixels would be reading what no driver owes it.
fn read_back_rgba8(
    sandbox: &GpuContextLimitedAccess,
    surface_id: &str,
    texture: &crate::core::rhi::Texture,
    label: &str,
) -> Vec<u8> {
    use crate::core::rhi::TextureSourceLayout;
    sandbox
        .escalate(|full| {
            let resting_layout = full
                .resolve_texture_registration_by_surface_id(surface_id, None, 64, 64)?
                .current_layout();
            let source_layout = if resting_layout
                == streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL
            {
                TextureSourceLayout::ShaderReadOnly
            } else {
                TextureSourceLayout::General
            };
            let readback =
                full.create_texture_readback(label, 64, 64, TextureFormat::Rgba8Unorm)?;
            let ticket = readback.submit(texture, source_layout)?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the texture reads back")
}

/// The layout a surface's registration is tracked in, read through a
/// fresh resolve at the chain tests' fixed 64×64 extent.
fn tracked_layout_of_surface(
    sandbox: &GpuContextLimitedAccess,
    surface_id: &str,
) -> streamlib_consumer_rhi::VulkanLayout {
    sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id(surface_id, None, 64, 64)?
                .current_layout())
        })
        .expect("the surface still resolves")
}

fn assert_every_pixel_is(pixels: &[u8], expected: [u8; 4], what: &str) {
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(pixel, expected, "pixel {index} of {what}");
    }
}

/// The claim the whole op rests on: a later pass reads what an earlier
/// pass wrote, inside one recording.
///
/// The intermediate is written as a storage image and read as a sampled
/// texture, so the batch owes it both a memory dependency and a layout
/// transition — and a barrier taken from the texture's *pre-batch*
/// layout would discard the very writes pass 2 is there for. The two
/// shaders do not commute, so a swapped order fails on the pixels
/// rather than passing quietly.
#[test]
fn a_later_pass_in_a_batch_reads_what_an_earlier_pass_wrote() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batched chain: no GPU — skipping");
        return;
    };
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let double = register_glsl_kernel(&sandbox, DOUBLE_GLSL);
    let held = seeded_chain_textures(
        &sandbox,
        ["chain-seed", "chain-brightened", "chain-doubled"],
    );

    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "chain".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "chain-seed"),
                    ("brightened_image", "chain-brightened"),
                ),
                batched_dispatch(
                    &double,
                    ("brightened_image", "chain-brightened"),
                    ("doubled_image", "chain-doubled"),
                ),
            ],
            request_id: "chain".to_string(),
        },
    );
    assert!(
        matches!(response, EscalateResponse::Ok(_)),
        "the two-pass chain failed: {response:?}"
    );

    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "chain-doubled",
            held[2].texture(),
            "chain-readback",
        ),
        CHAIN_DOUBLED_RGBA,
        "the chain's output — pass 2 must have read pass 1's writes, not the \
         seed and not an undefined intermediate",
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "chain-brightened",
            held[1].texture(),
            "chain-intermediate-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        "the intermediate — pass 1's own output",
    );

    // Each texture's tracked layout is published as the layout its
    // *last* use in the batch left it in, which is where the next
    // batch's barrier starts from. Asserted because the pixels above
    // cannot check it: a source layout of UNDEFINED licenses the driver
    // to discard contents, and this one declines to, so a batch that
    // barriered every pass from the pre-batch layout would still read
    // back correctly here while being wrong by the spec.
    assert_eq!(
        tracked_layout_of_surface(&sandbox, "chain-brightened"),
        streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "the intermediate was written as a storage image and then read as a sampled \
         texture, so it ends in the layout its last use required"
    );
    assert_eq!(
        tracked_layout_of_surface(&sandbox, "chain-doubled"),
        streamlib_consumer_rhi::VulkanLayout::GENERAL,
        "the final output was only ever written, so it ends in GENERAL"
    );
    drop(held);
}

/// The reason the op exists, counted rather than timed: N passes
/// batched cost one submission and one stall, where N separate
/// `run_compute_kernel` ops cost one of each per pass.
///
/// Both arms run the same dispatches on the same device in the same
/// test, so the comparison is against the path this op replaces —
/// not against a remembered number.
#[test]
fn a_batch_costs_one_submission_and_one_stall_where_separate_dispatches_cost_n() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batch submission count: no GPU — skipping");
        return;
    };
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let double = register_glsl_kernel(&sandbox, DOUBLE_GLSL);
    let held = seeded_chain_textures(
        &sandbox,
        ["counted-seed", "counted-brightened", "counted-doubled"],
    );

    let dispatches = vec![
        batched_dispatch(
            &brighten,
            ("unbrightened_image", "counted-seed"),
            ("brightened_image", "counted-brightened"),
        ),
        batched_dispatch(
            &double,
            ("brightened_image", "counted-brightened"),
            ("doubled_image", "counted-doubled"),
        ),
    ];

    // Measured on the second run, not the first: a per-frame claim is
    // about the steady state, and the opening batch on a fresh context
    // also builds the recorder and finds its fence already signaled.
    let mut batched_submissions = 0;
    let mut batched_stalls = 0;
    for run in 0..2 {
        let submissions_before = sandbox.host_inner().queue_submission_count();
        let stalls_before = sandbox
            .host_inner()
            .recorder_and_compute_kernel_fence_wait_count();
        let response = handle_run_compute_kernel_batch(
            &sandbox,
            format!("counted-{run}"),
            EscalateRequestRunComputeKernelBatch {
                dispatches: dispatches.clone(),
                request_id: format!("counted-{run}"),
            },
        );
        assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
        batched_submissions = sandbox.host_inner().queue_submission_count() - submissions_before;
        batched_stalls = sandbox
            .host_inner()
            .recorder_and_compute_kernel_fence_wait_count()
            - stalls_before;
    }

    assert_eq!(
        batched_submissions, 1,
        "two batched dispatches must go out as one command buffer"
    );
    assert_eq!(
        batched_stalls, 1,
        "and cost the caller exactly one fence wait — a second would mean the \
         recorder waits again at the next begin() on a fence it already drained"
    );

    let submissions_before = sandbox.host_inner().queue_submission_count();
    let stalls_before = sandbox
        .host_inner()
        .recorder_and_compute_kernel_fence_wait_count();
    for dispatch in &dispatches {
        let response = handle_run_compute_kernel(
            &sandbox,
            "separate".to_string(),
            EscalateRequestRunComputeKernel {
                bindings: dispatch.bindings.clone(),
                group_count_x: dispatch.group_count_x,
                group_count_y: dispatch.group_count_y,
                group_count_z: dispatch.group_count_z,
                kernel_id: dispatch.kernel_id.clone(),
                push_constants_hex: dispatch.push_constants_hex.clone(),
                request_id: "separate".to_string(),
            },
        );
        assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
    }
    let separate_submissions = sandbox.host_inner().queue_submission_count() - submissions_before;
    let separate_stalls = sandbox
        .host_inner()
        .recorder_and_compute_kernel_fence_wait_count()
        - stalls_before;

    assert_eq!(
        separate_submissions,
        dispatches.len(),
        "a single dispatch rides the batch machinery as a recording of one, so N \
         separate ops cost exactly N submissions — and if this is zero the counter \
         is not counting and the batched assertion above proves nothing"
    );
    assert_eq!(
        separate_stalls,
        dispatches.len(),
        "and exactly N fence waits, one per op — paying this once instead of N \
         times is the batch's whole advantage: {separate_stalls} vs {batched_stalls}"
    );
    drop(held);
}

/// The single op rides the same machinery as a batch of one: barriers
/// and dispatch in one recording, one submission, one fence wait —
/// where the kernel-fence path it replaced paid up to two submissions
/// and three waits — and each binding rests in the layout its
/// descriptor requires, which the pixels cannot check on this driver.
#[test]
fn a_single_dispatch_costs_one_submission_and_one_stall_and_rests_its_layouts() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("single dispatch machinery: no GPU — skipping");
        return;
    };
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let held = seeded_chain_textures(&sandbox, ["single-seed", "single-brightened"]);
    let run_single_dispatch = |request_id: &str| {
        handle_run_compute_kernel(
            &sandbox,
            request_id.to_string(),
            EscalateRequestRunComputeKernel {
                bindings: vec![
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::SampledTexture,
                        name: "unbrightened_image".to_string(),
                        target_id: "single-seed".to_string(),
                    },
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageImage,
                        name: "brightened_image".to_string(),
                        target_id: "single-brightened".to_string(),
                    },
                ],
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: brighten.clone(),
                push_constants_hex: String::new(),
                request_id: request_id.to_string(),
            },
        )
    };

    // Warmed up before measuring: a per-frame claim is about the
    // steady state, and the opening dispatch on a fresh context also
    // builds the shared recorder.
    let warm_up = run_single_dispatch("single-warm-up");
    assert!(matches!(warm_up, EscalateResponse::Ok(_)), "{warm_up:?}");
    let submissions_before = sandbox.host_inner().queue_submission_count();
    let stalls_before = sandbox
        .host_inner()
        .recorder_and_compute_kernel_fence_wait_count();
    let measured = run_single_dispatch("single-measured");
    assert!(matches!(measured, EscalateResponse::Ok(_)), "{measured:?}");
    let submissions = sandbox.host_inner().queue_submission_count() - submissions_before;
    let stalls = sandbox
        .host_inner()
        .recorder_and_compute_kernel_fence_wait_count()
        - stalls_before;
    assert_eq!(
        submissions, 1,
        "the barriers and the dispatch must go out as one command buffer — a \
         second submission means a separate transition recording is back"
    );
    assert_eq!(
        stalls, 1,
        "and cost the caller exactly one fence wait — more means the kernel's \
         own fence or a transition recorder's wait is back in the path"
    );

    assert_eq!(
        tracked_layout_of_surface(&sandbox, "single-seed"),
        streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "the sampled source rests in the layout its descriptor requires"
    );
    assert_eq!(
        tracked_layout_of_surface(&sandbox, "single-brightened"),
        streamlib_consumer_rhi::VulkanLayout::GENERAL,
        "the storage output rests in GENERAL"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "single-brightened",
            held[1].texture(),
            "single-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        "the single dispatch's output — the machinery change must not move pixels",
    );
    drop(held);
}

/// The one single-dispatch refusal that fires inside an open
/// recording: push constants the kernel does not declare are refused
/// by `set_push_constants` after the barriers are recorded, so the
/// abort must leave the shared recorder usable for whatever records
/// next.
#[test]
fn a_single_dispatch_failing_inside_the_recording_leaves_the_recorder_usable() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("single dispatch abort: no GPU — skipping");
        return;
    };
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let held = seeded_chain_textures(&sandbox, ["abort-seed", "abort-brightened"]);
    let run_single_dispatch = |request_id: &str, push_constants_hex: &str| {
        handle_run_compute_kernel(
            &sandbox,
            request_id.to_string(),
            EscalateRequestRunComputeKernel {
                bindings: vec![
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::SampledTexture,
                        name: "unbrightened_image".to_string(),
                        target_id: "abort-seed".to_string(),
                    },
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageImage,
                        name: "brightened_image".to_string(),
                        target_id: "abort-brightened".to_string(),
                    },
                ],
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: brighten.clone(),
                push_constants_hex: push_constants_hex.to_string(),
                request_id: request_id.to_string(),
            },
        )
    };

    let message = refusal_message(run_single_dispatch("abort-refused", "00000000"));
    assert!(
        message.contains("push-constant size mismatch"),
        "the refusal must be the in-recording one, or this proves nothing: {message}"
    );
    let recovered = run_single_dispatch("abort-recovered", "");
    assert!(
        matches!(recovered, EscalateResponse::Ok(_)),
        "the shared recorder must survive a refused single dispatch: {recovered:?}"
    );
    drop(held);
}

/// Both slots name one image, at one kind — aliasing the resolver
/// permits, since only differing kinds clash.
const TWO_SLOTS_ONE_IMAGE_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba8) uniform readonly image2D image_slot_a;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D image_slot_b;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    imageStore(image_slot_b, at, imageLoad(image_slot_a, at));
}
";

/// A cross-process resolve synthesizes a fresh registration per call,
/// so two slots naming one image hold two layout cells. The publish
/// walks bindings, not first-touch images: a cell left behind would
/// hand the surface-share service a pre-dispatch layout, and the next
/// import would barrier from a layout the image has already left.
#[test]
fn every_registration_cell_naming_one_image_learns_the_landed_layout() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("per-cell layout publish: no GPU — skipping");
        return;
    };
    let kernel_id = register_glsl_kernel(&sandbox, TWO_SLOTS_ONE_IMAGE_GLSL);
    sandbox
        .escalate(|full| {
            let kernel = full
                .compute_kernel_by_id(&kernel_id)
                .expect("the kernel just registered");
            let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::STORAGE_BINDING);
            let held = full.acquire_texture(&desc)?;
            let cell_a = TextureRegistration::new(
                held.texture().clone(),
                streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
            );
            let cell_b = TextureRegistration::new(
                held.texture().clone(),
                streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
            );
            let recording = [BatchedComputeKernelDispatch {
                kernel,
                bindings: vec![
                    BatchedComputeKernelDispatchBinding {
                        binding: 0,
                        kind: SurfaceBoundKernelBindingKind::StorageImage,
                        registration: cell_a.clone(),
                    },
                    BatchedComputeKernelDispatchBinding {
                        binding: 1,
                        kind: SurfaceBoundKernelBindingKind::StorageImage,
                        registration: cell_b.clone(),
                    },
                ],
                push_constants: Vec::new(),
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
            }];
            full.dispatch_compute_kernel_batch(&recording)?;
            assert_eq!(
                cell_a.current_layout(),
                streamlib_consumer_rhi::VulkanLayout::GENERAL,
                "the cell the first touch barriered from learns the landed layout"
            );
            assert_eq!(
                cell_b.current_layout(),
                streamlib_consumer_rhi::VulkanLayout::GENERAL,
                "and so does the second cell over the same image, which was never \
                 a barrier's source"
            );
            drop(held);
            Ok(())
        })
        .expect("a recording over two cells of one image dispatches");
}

/// A kernel owns one descriptor set, so the second bind would hand the
/// first recorded dispatch this dispatch's bindings — and nothing has
/// executed yet, so it would do it silently.
#[test]
fn a_batch_naming_one_kernel_twice_is_refused_saying_why() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("batch duplicate kernel: no GPU — skipping");
        return;
    };
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let held = seeded_chain_textures(&sandbox, ["twice-seed", "twice-middle", "twice-out"]);

    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "twice".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "twice-seed"),
                    ("brightened_image", "twice-middle"),
                ),
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "twice-middle"),
                    ("brightened_image", "twice-out"),
                ),
            ],
            request_id: "twice".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => {
            assert!(
                err.message.contains("descriptor set"),
                "the refusal must say why one kernel cannot appear twice: {}",
                err.message
            );
            assert!(
                err.message.contains("dispatch 1") && err.message.contains("dispatch 0"),
                "and must name both dispatches: {}",
                err.message
            );
        }
        other => panic!("naming one kernel twice must refuse, got {other:?}"),
    }
    drop(held);
}

/// A batch runs whole or not at all, and never strands the recorder.
///
/// The refused batch here fails on its *second* dispatch, so the first
/// one was already planned when the refusal fired. Nothing may reach
/// the GPU — asserted on the first dispatch's output pixels, which stay
/// at what they held — and the recorder must still take the next batch,
/// which is what `begin()` would refuse if the failure had left a
/// recording open.
#[test]
fn a_refused_batch_submits_nothing_and_leaves_the_recorder_usable() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("refused batch: no GPU — skipping");
        return;
    };
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let double = register_glsl_kernel(&sandbox, DOUBLE_GLSL);
    let held = seeded_chain_textures(
        &sandbox,
        ["refused-seed", "refused-brightened", "refused-doubled"],
    );

    // The intermediate starts undefined; one batch establishes it, so
    // the assertion below is against known content rather than
    // whatever the allocator handed over.
    let established = handle_run_compute_kernel_batch(
        &sandbox,
        "establish".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![batched_dispatch(
                &brighten,
                ("unbrightened_image", "refused-seed"),
                ("brightened_image", "refused-brightened"),
            )],
            request_id: "establish".to_string(),
        },
    );
    assert!(
        matches!(established, EscalateResponse::Ok(_)),
        "{established:?}"
    );

    let submissions_before = sandbox.host_inner().queue_submission_count();
    let refused = handle_run_compute_kernel_batch(
        &sandbox,
        "refused".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "refused-seed"),
                    ("brightened_image", "refused-brightened"),
                ),
                batched_dispatch(
                    &double,
                    ("brightened_image", "no-such-surface"),
                    ("doubled_image", "refused-doubled"),
                ),
            ],
            request_id: "refused".to_string(),
        },
    );
    match refused {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("dispatch 1") && err.message.contains("no-such-surface"),
            "the refusal must name the dispatch and the surface: {}",
            err.message
        ),
        other => panic!("an unresolvable binding must refuse, got {other:?}"),
    }
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before,
        "a refused batch submits nothing — not even the dispatches ahead of the \
         one that was refused"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "refused-brightened",
            held[1].texture(),
            "refused-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        // Weak on its own — dispatch 0 would have recomputed this same
        // value — so the claim that nothing ran rests on the
        // submission count above. This checks the refusal did not
        // leave the surface unreadable.
        "the intermediate, still readable after the refused batch",
    );

    // The recorder survived: a fresh batch runs, which begin() would
    // refuse outright if the refusal above had left a recording open.
    let after = handle_run_compute_kernel_batch(
        &sandbox,
        "after".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![batched_dispatch(
                &double,
                ("brightened_image", "refused-brightened"),
                ("doubled_image", "refused-doubled"),
            )],
            request_id: "after".to_string(),
        },
    );
    assert!(
        matches!(after, EscalateResponse::Ok(_)),
        "the recorder must still be usable after a refused batch: {after:?}"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "refused-doubled",
            held[2].texture(),
            "after-readback",
        ),
        CHAIN_DOUBLED_RGBA,
        "the batch that ran after the refused one",
    );
    drop(held);
}

/// The one failure that lands *inside* an open recording, and the only
/// test that reaches `abort_recording`.
///
/// Every other refusal — an unknown kernel, a binding the shader does
/// not declare, a surface that will not resolve, one kernel twice —
/// fires while resolving, before `begin()`. This one cannot: the
/// push-constant size is the kernel's own business and is only checked
/// when the payload is staged, which happens after the barriers are
/// recorded. So the recording is open when it fails, and a batch that
/// did not abort it would strand the recorder — the next `begin()`
/// refuses outright while a recording is in progress.
#[test]
fn a_batch_failing_inside_the_recording_aborts_it_and_the_recorder_survives() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("mid-recording batch failure: no GPU — skipping");
        return;
    };
    // The conformance kernel declares 4 push-constant bytes; the batch
    // below sends none.
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;
    let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
    let held = seeded_chain_textures(&sandbox, ["aborted-seed", "aborted-middle", "aborted-out"]);

    let submissions_before = sandbox.host_inner().queue_submission_count();
    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "aborted".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![EscalateRequestRunComputeKernelBatchDispatch {
                bindings: supplied(&[
                    (
                        "source_image",
                        EscalateComputeBindingKind::SampledTexture,
                        "aborted-seed",
                    ),
                    (
                        "output_image",
                        EscalateComputeBindingKind::StorageImage,
                        "aborted-middle",
                    ),
                ]),
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id,
                push_constants_hex: String::new(),
            }],
            request_id: "aborted".to_string(),
        },
    );
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("push-constant size mismatch")
                && err.message.contains("kernel declares 4"),
            "the refusal must name the size the kernel wanted: {}",
            err.message
        ),
        other => panic!("a kernel needing push constants must refuse, got {other:?}"),
    }
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before,
        "a batch that failed while recording submits nothing"
    );

    // The assertion this test exists for: the recorder took the next
    // batch. Without the abort it is still in `Recording`, and
    // `begin()` refuses a recording already in progress.
    let after = handle_run_compute_kernel_batch(
        &sandbox,
        "after-abort".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![batched_dispatch(
                &brighten,
                ("unbrightened_image", "aborted-seed"),
                ("brightened_image", "aborted-middle"),
            )],
            request_id: "after-abort".to_string(),
        },
    );
    assert!(
        matches!(after, EscalateResponse::Ok(_)),
        "the recorder must be usable after a batch aborted mid-recording: {after:?}"
    );
    assert_every_pixel_is(
        &read_back_rgba8(
            &sandbox,
            "aborted-middle",
            held[1].texture(),
            "after-abort-readback",
        ),
        CHAIN_BRIGHTENED_RGBA,
        "the batch that ran after the aborted one",
    );
    drop(held);
}

/// A dispatch that reads and writes one surface is refused, because the
/// two descriptors want the image in two layouts at once.
///
/// Reachable from Python — `bindings={"src": s, "dst": s}` is an
/// ordinary-looking mapping — and the shader's own names differ, so the
/// duplicate-name rule does not catch it. Left unrefused, the batch
/// records two contradictory barriers and the dispatch runs with one
/// descriptor's layout wrong; the single-dispatch path records no
/// barriers at all and is wrong the same way. Refused for both.
#[test]
fn one_surface_bound_as_two_kinds_in_one_dispatch_is_refused() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("one surface at two kinds: no GPU — skipping");
        return;
    };
    let kernel_id = register_read_one_write_another(&sandbox).handle_id;
    let held = seeded_chain_textures(&sandbox, ["both-seed", "both-b", "both-c"]);

    let read_and_written = supplied(&[
        (
            "source_image",
            EscalateComputeBindingKind::SampledTexture,
            "both-seed",
        ),
        (
            "output_image",
            EscalateComputeBindingKind::StorageImage,
            "both-seed",
        ),
    ]);

    // Refused on the single-dispatch path...
    let single = handle_run_compute_kernel(
        &sandbox,
        "both-single".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: read_and_written.clone(),
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id: kernel_id.clone(),
            push_constants_hex: "00000000".to_string(),
            request_id: "both-single".to_string(),
        },
    );
    let message = refusal_message(single);
    assert!(
        message.contains("both-seed")
            && message.contains("`source_image`")
            && message.contains("`output_image`"),
        "the refusal must name the surface and both bindings: {message}"
    );

    // ...and on the batch path, which shares the resolver.
    let submissions_before = sandbox.host_inner().queue_submission_count();
    let batched = handle_run_compute_kernel_batch(
        &sandbox,
        "both-batched".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: vec![EscalateRequestRunComputeKernelBatchDispatch {
                bindings: read_and_written,
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: kernel_id.clone(),
                push_constants_hex: "00000000".to_string(),
            }],
            request_id: "both-batched".to_string(),
        },
    );
    assert!(
        refusal_message(batched).contains("both-seed"),
        "the batch shares the resolver, so it refuses the same shape"
    );
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before,
        "and submits nothing"
    );

    // Two spellings, one texture. A published frame id resolves through
    // its pool slot's cache entry, so `both-seed` and `both-seed#3` are
    // the same image — and comparing the ids as strings would let this
    // pair through to the dispatch the arms above refuse.
    let two_spellings = handle_run_compute_kernel(
        &sandbox,
        "both-spellings".to_string(),
        EscalateRequestRunComputeKernel {
            bindings: supplied(&[
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "both-seed",
                ),
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageImage,
                    "both-seed#3",
                ),
            ]),
            group_count_x: 8,
            group_count_y: 8,
            group_count_z: 1,
            kernel_id,
            push_constants_hex: "00000000".to_string(),
            request_id: "both-spellings".to_string(),
        },
    );
    let message = refusal_message(two_spellings);
    assert!(
        message.contains("both-seed\"") && message.contains("both-seed#3"),
        "the refusal must name both spellings, since neither is wrong on its own: \
         {message}"
    );
    drop(held);
}

/// An empty batch is not an error — there is simply nothing to submit,
/// and a caller who opened a scope and dispatched nothing has not made
/// a mistake worth raising on.
#[test]
fn an_empty_batch_submits_nothing_and_is_not_an_error() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("empty batch: no GPU — skipping");
        return;
    };
    let submissions_before = sandbox.host_inner().queue_submission_count();
    let response = handle_run_compute_kernel_batch(
        &sandbox,
        "empty".to_string(),
        EscalateRequestRunComputeKernelBatch {
            dispatches: Vec::new(),
            request_id: "empty".to_string(),
        },
    );
    assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
    assert_eq!(
        sandbox.host_inner().queue_submission_count(),
        submissions_before
    );
}
