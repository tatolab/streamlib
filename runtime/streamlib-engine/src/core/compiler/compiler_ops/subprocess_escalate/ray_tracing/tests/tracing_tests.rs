// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Tracing over a ray-tracing device: the bindings a trace resolves by name,
//! the ones it refuses, and the escalate gate a trace every frame never enters.

use super::*;

#[test]
fn tracing_with_an_unregistered_kernel_id_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("tracing_with_an_unregistered_kernel_id: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-unknown-kernel");
    req.kernel_id = "definitely-not-a-registered-kernel".to_string();
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("no kernel registered under id"),
        "got: {message}"
    );
}

/// An acceleration-structure binding resolves through the structure
/// registry rather than through a surface, so an id no registration
/// minted is refused there rather than falling through to the surface
/// planner and getting a surface's error text.
#[test]
fn a_trace_naming_an_unregistered_acceleration_structure_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_naming_an_unregistered_structure: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-unknown-structure");
    req.bindings[0].target_id = "definitely-not-a-registered-structure".to_string();
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` names no acceleration structure"),
        "must name the binding, got: {message}"
    );
}

/// The structure a trace binds is the top-level one a
/// `register_acceleration_structure_tlas` returned; a BLAS id is the
/// same shape of string and would otherwise reach the descriptor.
#[test]
fn a_trace_binding_a_bottom_level_structure_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_binding_a_bottom_level_structure: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-blas");
    req.bindings[0].target_id = scene.blas_id.clone();
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("is a bottom-level structure"),
        "got: {message}"
    );
}

/// The acceleration structure never reaches the surface planner, so its
/// missing-binding rule is enforced separately — and has to fire.
#[test]
fn a_declared_acceleration_structure_left_out_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_declared_acceleration_structure_left_out: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-no-structure");
    req.bindings
        .retain(|binding| binding.name != "scene_geometry");
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` was not supplied"),
        "must name the missing binding, got: {message}"
    );
    assert!(
        message.contains("do not persist between traces"),
        "must say why there is no fallback, got: {message}"
    );
}

/// Supplying the structure under another kind would otherwise send it
/// to the surface planner, which would look for a surface named by an
/// `as_id`.
#[test]
fn an_acceleration_structure_supplied_as_another_kind_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("an_acceleration_structure_supplied_as_another_kind: no RT — skipping");
        return;
    };
    let mut req = scene.run_req("trace-wrong-kind");
    req.bindings[0].kind = EscalateRayTracingBindingKind::StorageImage;
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` was supplied as storage_image"),
        "must name the binding and the kind supplied, got: {message}"
    );
    assert!(
        message.contains("declares it acceleration_structure"),
        "must name the kind the kernel declares, got: {message}"
    );
}

/// The whole array is checked before the acceleration structures are
/// split out of it, so a name supplied twice is refused whichever half
/// the second copy would land in — and by the one rule the surface
/// planner spells, not a second wording of it.
#[test]
fn a_name_supplied_twice_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_name_supplied_twice: no RT device — skipping");
        return;
    };
    let mut req = scene.run_req("trace-twice");
    let structure_binding = req.bindings[0].clone();
    assert_eq!(
        structure_binding.name, "scene_geometry",
        "the duplicate has to be the structure, which the surface planner never sees"
    );
    req.bindings.push(structure_binding);
    let message = refusal_message(scene.trace(req));
    assert!(
        message.contains("binding `scene_geometry` was supplied twice"),
        "must name the duplicate, got: {message}"
    );
    assert!(
        message.contains("`traced_output`"),
        "must name every binding this kernel declares, got: {message}"
    );
    assert!(
        message.contains("exactly once per trace"),
        "must state the rule in the caller's own noun, got: {message}"
    );
}

/// The op end to end, over a real device: a trace resolves the scene
/// and the storage image by the raygen's own names for them, launches
/// the grid, and leaves the engine's layout record agreeing with the
/// layout the trace left the image in.
///
/// The storage image is seeded by a transfer with a sentinel no stage
/// can produce, so a trace that bound nothing fails on the pixels
/// rather than passing on undefined contents — and because that seed
/// leaves the image in `TRANSFER_DST_OPTIMAL`, a trace that did not
/// barrier its bound inputs would write it through a descriptor its
/// layout does not satisfy. Both the hit and the miss stage must have
/// run, which is what proves the structure reached the descriptor.
#[test]
fn a_trace_resolves_its_bindings_by_name_and_writes_the_storage_image() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_resolves_its_bindings_by_name: no RT device — skipping");
        return;
    };
    match scene.trace(scene.run_req("trace-scene")) {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "trace-scene");
            assert_eq!(
                ok.handle_id, scene.kernel_id,
                "the trace response echoes the kernel_id"
            );
            assert!(
                ok.timeline_value.is_none(),
                "run_ray_tracing_kernel responses carry no timeline"
            );
        }
        other => panic!("the trace failed: {other:?}"),
    }

    // Asserted before the readback, which transitions the image itself:
    // an unpublished layout would leave the next consumer's barrier
    // naming an oldLayout the image has already left.
    let published = scene
        .sandbox
        .escalate(|full| {
            Ok(full
                .resolve_texture_registration_by_surface_id(
                    A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                    None,
                    TRACED_GRID_WIDTH,
                    TRACED_GRID_HEIGHT,
                )?
                .current_layout())
        })
        .expect("the storage image still resolves");
    assert_eq!(
        published,
        streamlib_consumer_rhi::VulkanLayout::GENERAL,
        "the trace must publish the layout it left the storage image in"
    );

    let traced = scene
        .sandbox
        .escalate(|full| {
            let readback = full.create_texture_readback(
                "trace-readback",
                TRACED_GRID_WIDTH,
                TRACED_GRID_HEIGHT,
                TextureFormat::Rgba8Unorm,
            )?;
            let ticket = readback.submit(
                scene._held_output.texture(),
                crate::core::rhi::TextureSourceLayout::General,
            )?;
            Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
        })
        .expect("the storage image reads back");

    let mut hit_pixels = 0usize;
    let mut missed_pixels = 0usize;
    for (pixel_index, pixel) in traced.as_chunks::<4>().0.iter().enumerate() {
        if *pixel == HIT_RGBA {
            hit_pixels += 1;
        } else if *pixel == MISSED_RGBA {
            missed_pixels += 1;
        } else {
            panic!(
                "pixel {pixel_index} is {pixel:?}, which no stage of this kernel writes — \
                         the trace left the seeded sentinel, so `traced_output` was never written"
            );
        }
    }
    assert!(
        hit_pixels > 0,
        "no pixel hit the scene's triangle — `scene_geometry` did not reach the descriptor"
    );
    assert!(
        missed_pixels > 0,
        "every pixel hit, so the launch grid never left the triangle and the miss stage \
                 proved nothing"
    );
}

/// A trace refuses a storage buffer by name; a tensor buffer binds only at a
/// dispatch or a draw.
#[test]
fn a_trace_refuses_a_storage_buffer_binding_by_name() {
    use super::super::super::surface_bound_kernel_binding::{
        DeclaredKernelBindingUnderPlanning, SuppliedKernelBindingUnderPlanning,
        plan_supplied_surface_bound_kernel_bindings,
    };
    use super::super::linux_and_macos::surface_bound_ray_tracing_binding_kind;
    use crate::core::rhi::RayTracingBindingKind;

    let declared = [DeclaredKernelBindingUnderPlanning {
        binding_slot: 0,
        name: Some("scene_parameters"),
        kind_wire_name: EscalateRayTracingBindingKind::StorageBuffer.wire_name(),
        surface_bound_kind: surface_bound_ray_tracing_binding_kind(
            RayTracingBindingKind::StorageBuffer,
        ),
    }];
    let supplied = [SuppliedKernelBindingUnderPlanning {
        name: "scene_parameters",
        target_id: "tensor-in",
        kind_wire_name: EscalateRayTracingBindingKind::StorageBuffer.wire_name(),
    }];
    let message = plan_supplied_surface_bound_kernel_bindings("trace", &supplied, &declared)
        .err()
        .expect("a trace refuses a storage buffer")
        .to_string();
    assert!(
        message.contains(
            "binding `scene_parameters` is storage_buffer, which a trace cannot bind by surface id"
        ),
        "must name the binding and its kind, got: {message}"
    );
}

/// A frame's trace is per-frame work: it takes neither the process-wide
/// escalate gate nor the whole-device idle wait that comes with it (#2730).
#[test]
fn a_trace_every_frame_never_enters_the_escalate_gate() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_every_frame_never_enters_the_escalate_gate: no ray tracing — skipping");
        return;
    };
    super::super::super::run_frames_with_the_escalate_gate_held(
        &scene.sandbox,
        "trace",
        |frame_index| scene.trace(scene.run_req(&format!("frame-{frame_index}"))),
    );
}

/// With the escalate gate gone from a trace, the held kernel is what keeps a
/// second trace off a cached kernel whose bindings another holder is staging:
/// the trace waits until that holder lets go, then runs.
#[test]
fn a_trace_waits_while_another_holder_stages_bindings_on_its_kernel() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_trace_waits_while_another_holder_stages_bindings: no ray tracing — skipping");
        return;
    };
    let held_kernel = scene
        .sandbox
        .host_inner()
        .ray_tracing_kernel_by_id_held_for_one_trace(&scene.kernel_id)
        .expect("the registered kernel is cached");
    super::super::super::assert_a_per_frame_op_waits_for_its_held_kernel_then_succeeds(
        "trace",
        held_kernel,
        || scene.trace(scene.run_req("held-trace")),
    );
}
