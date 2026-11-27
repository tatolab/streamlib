// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A compute kernel a helper registered, released through `release_handle` or
//! with the helper's teardown: the cache lets it go with its last registration,
//! and its pipeline goes with it.

use std::sync::{Arc, Weak};

use super::super::super::handle_escalate_op;
use super::super::super::handle_lifecycle::release_everything_a_torn_down_helper_held;
use super::*;
use crate::core::compiler::compiler_ops::subprocess_escalate::WhatAHelperPlacedNodeReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;

/// A kernel whose workgroup width is `width`, as a kernel sized by an input is:
/// each width is a different shader, so a different kernel id.
fn fill_kernel_of_workgroup_width(width: u32) -> String {
    format!(
        "\
[[vk::binding(0, 0)]] [format(\"rgba8\")] RWTexture2D<float4> filled_image;
[numthreads({width}, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {{
    filled_image[invocation.xy] = float4(1.0, 0.0, 0.0, 1.0);
}}
"
    )
}

fn register_through_the_escalate_op(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    source: &str,
) -> String {
    let response = handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::RegisterComputeKernel(register_from_slang(source, "compute")),
    )
    .expect("a register op is answered");
    match response {
        EscalateResponse::Ok(ok) => ok.handle_id,
        other => panic!("registering the kernel failed: {other:?}"),
    }
}

fn release_through_the_escalate_op(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    kernel_id: &str,
) -> EscalateResponse {
    handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "release-kernel".to_string(),
            handle_id: kernel_id.to_string(),
        }),
    )
    .expect("a release op is answered")
}

/// The kernel cached under `kernel_id`, held weakly so holding it keeps nothing
/// alive; `None` once no registration holds it.
fn the_cached_kernel_held_weakly(
    sandbox: &GpuContextLimitedAccess,
    kernel_id: &str,
) -> Option<Weak<crate::vulkan::rhi::VulkanComputeKernel>> {
    sandbox
        .escalate(|full| {
            Ok(full
                .compute_kernel_by_id(kernel_id)
                .map(|kernel| Arc::downgrade(&kernel)))
        })
        .expect("looking a kernel up never fails")
}

#[test]
fn a_registered_compute_kernel_is_released_through_release_handle() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_registered_compute_kernel_is_released: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::default();
    let kernel_id =
        register_through_the_escalate_op(&sandbox, &registry, &fill_kernel_of_workgroup_width(8));
    let kernel =
        the_cached_kernel_held_weakly(&sandbox, &kernel_id).expect("a registered kernel is cached");

    let released = release_through_the_escalate_op(&sandbox, &registry, &kernel_id);
    assert!(
        matches!(released, EscalateResponse::Ok(_)),
        "releasing a registered kernel must succeed: {released:?}"
    );
    assert!(
        the_cached_kernel_held_weakly(&sandbox, &kernel_id).is_none(),
        "the cache still holds a kernel no registration holds"
    );
    assert!(
        kernel.upgrade().is_none(),
        "the released kernel, and its pipeline with it, outlived its last registration"
    );

    let message = refusal_message(release_through_the_escalate_op(
        &sandbox, &registry, &kernel_id,
    ));
    assert!(message.contains("not found in registry"), "{message}");
}

/// One id names every registration of one shader, so a kernel two nodes built
/// from the same source lives until both let it go.
#[test]
fn a_kernel_registered_twice_lives_until_both_registrations_are_released() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_kernel_registered_twice: no GPU — skipping");
        return;
    };
    let first_helper = EscalateHandleRegistry::default();
    let second_helper = EscalateHandleRegistry::default();
    let source = fill_kernel_of_workgroup_width(16);
    let kernel_id = register_through_the_escalate_op(&sandbox, &first_helper, &source);
    assert_eq!(
        register_through_the_escalate_op(&sandbox, &second_helper, &source),
        kernel_id,
        "one shader is one kernel id"
    );

    let released = release_through_the_escalate_op(&sandbox, &first_helper, &kernel_id);
    assert!(matches!(released, EscalateResponse::Ok(_)), "{released:?}");
    let kernel = the_cached_kernel_held_weakly(&sandbox, &kernel_id)
        .expect("the second helper's registration still holds the kernel");

    let message = refusal_message(release_through_the_escalate_op(
        &sandbox,
        &first_helper,
        &kernel_id,
    ));
    assert!(
        message.contains("not found in registry"),
        "a helper released a registration only another helper held: {message}"
    );

    let released = release_through_the_escalate_op(&sandbox, &second_helper, &kernel_id);
    assert!(matches!(released, EscalateResponse::Ok(_)), "{released:?}");
    assert!(
        kernel.upgrade().is_none(),
        "the kernel outlived both registrations"
    );
}

/// A kernel rebuilt for each of a hundred sizes, each replaced one released as
/// a computed value's close releases it: only the newest is left, and every
/// replaced pipeline is gone.
#[test]
fn a_hundred_replaced_kernels_leave_only_the_newest_alive() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_hundred_replaced_kernels: no GPU — skipping");
        return;
    };
    let registry = EscalateHandleRegistry::default();
    let mut replaced_kernels = Vec::new();
    let mut held_kernel_id: Option<String> = None;
    for width in 1..=100 {
        let kernel_id = register_through_the_escalate_op(
            &sandbox,
            &registry,
            &fill_kernel_of_workgroup_width(width),
        );
        if let Some(replaced_kernel_id) = held_kernel_id.replace(kernel_id) {
            replaced_kernels.push(
                the_cached_kernel_held_weakly(&sandbox, &replaced_kernel_id)
                    .expect("the replaced kernel is cached until it is released"),
            );
            let released =
                release_through_the_escalate_op(&sandbox, &registry, &replaced_kernel_id);
            assert!(matches!(released, EscalateResponse::Ok(_)), "{released:?}");
        }
    }

    let still_alive = replaced_kernels
        .iter()
        .filter(|kernel| kernel.upgrade().is_some())
        .count();
    assert_eq!(still_alive, 0, "replaced kernels outlived their release");
    assert!(
        the_cached_kernel_held_weakly(&sandbox, &held_kernel_id.expect("a kernel was built"))
            .is_some(),
        "the newest kernel, still held, was released"
    );
}

/// A helper that crashes never sends `release_handle`, so every registration it
/// held goes with its bridge, while another helper's registration of the same
/// kernel keeps it.
#[test]
fn a_torn_down_helpers_kernel_registrations_are_released_and_another_helpers_kept() {
    let Some(sandbox) = make_gpu_sandbox_if_available() else {
        println!("a_torn_down_helpers_kernel_registrations: no GPU — skipping");
        return;
    };
    let crashed_helper = EscalateHandleRegistry::default();
    let other_helper = EscalateHandleRegistry::default();
    let shared_source = fill_kernel_of_workgroup_width(32);
    let only_the_crashed_helpers = register_through_the_escalate_op(
        &sandbox,
        &crashed_helper,
        &fill_kernel_of_workgroup_width(64),
    );
    // Registered twice, as two kernel objects of one node hold it.
    let shared = register_through_the_escalate_op(&sandbox, &crashed_helper, &shared_source);
    register_through_the_escalate_op(&sandbox, &crashed_helper, &shared_source);
    register_through_the_escalate_op(&sandbox, &other_helper, &shared_source);
    let crashed_helpers_kernel =
        the_cached_kernel_held_weakly(&sandbox, &only_the_crashed_helpers).expect("cached");

    release_everything_a_torn_down_helper_held(&sandbox, &crashed_helper);

    assert!(
        crashed_helpers_kernel.upgrade().is_none(),
        "the crashed helper's kernel outlived it"
    );
    let shared_kernel = the_cached_kernel_held_weakly(&sandbox, &shared)
        .expect("the other helper's registration still holds the shared kernel");
    let released = release_through_the_escalate_op(&sandbox, &other_helper, &shared);
    assert!(matches!(released, EscalateResponse::Ok(_)), "{released:?}");
    assert!(
        shared_kernel.upgrade().is_none(),
        "the crashed helper's two registrations were not both released"
    );
}
