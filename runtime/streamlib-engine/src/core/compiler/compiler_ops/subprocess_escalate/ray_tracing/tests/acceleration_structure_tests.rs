// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Bottom- and top-level acceleration structures registered over a
//! ray-tracing device: their ids, their release, and the instances refused.

use super::*;

/// Unlike a kernel, a structure holds device memory proportional to its
/// mesh — so every registration mints its own id rather than colliding
/// on content, and a TLAS is a different structure from its BLAS.
#[test]
fn every_acceleration_structure_registration_gets_its_own_id() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("every_acceleration_structure_registration: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let first_blas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "blas-a",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );
    let second_blas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "blas-b",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );
    assert_ne!(
        first_blas, second_blas,
        "an identical mesh registered twice is two structures, not one"
    );

    let tlas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req("tlas-a", &first_blas)),
    );
    assert_ne!(tlas, first_blas);
    assert_ne!(tlas, second_blas);
}

/// A structure is the one escalate-minted resource whose device memory
/// is proportional to what the caller supplied, so a long-running helper
/// has to be able to hand it back — the same `release_handle` a surface
/// is handed back through, since nothing else would reach the registry.
#[test]
fn a_registered_acceleration_structure_is_released_through_release_handle() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("a_registered_acceleration_structure_is_released: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let blas = register_acceleration_structure_or_panic(
        &sandbox,
        &registry,
        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
            "blas-to-release",
            &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
            &index_hex(A_SCENES_TRIANGLE_INDICES),
        )),
    );

    let released = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "release-blas".to_string(),
            handle_id: blas.clone(),
        }),
    )
    .expect("must produce a response");
    assert!(
        matches!(released, EscalateResponse::Ok(_)),
        "releasing a registered structure must succeed: {released:?}"
    );

    // The id is gone, not merely unreferenced: a second release finds
    // nothing, and a trace naming it would too.
    let released_twice = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "release-blas-again".to_string(),
            handle_id: blas,
        }),
    )
    .expect("must produce a response");
    let message = refusal_message(released_twice);
    assert!(message.contains("not found in registry"), "{message}");
}

#[test]
fn a_tlas_instance_naming_an_unregistered_structure_is_refused() {
    let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
        println!("a_tlas_instance_naming_an_unregistered_structure: no RT device — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::new();
    let message = refusal_message(
        handle_escalate_op(
            &sandbox,
            &registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "tlas-unknown",
                "definitely-not-a-registered-structure",
            )),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("names no acceleration structure registered under id"),
        "got: {message}"
    );
}

/// A TLAS instance references a bottom-level structure. Naming a
/// top-level one is a caller mistake the registry can catch, and every
/// id looks alike from the outside.
#[test]
fn a_tlas_instance_naming_a_top_level_structure_is_refused() {
    let Some(scene) = make_ray_traced_scene_if_available() else {
        println!("a_tlas_instance_naming_a_top_level_structure: no RT device — skipping");
        return;
    };
    let message = refusal_message(
        handle_escalate_op(
            &scene.sandbox,
            &scene.registry,
            &WhatTheNodeOfAHelperPlacedProcessorReaches::default(),
            EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "tlas-over-tlas",
                &scene.tlas_id,
            )),
        )
        .expect("must produce a response"),
    );
    assert!(
        message.contains("is a top-level structure"),
        "got: {message}"
    );
}
