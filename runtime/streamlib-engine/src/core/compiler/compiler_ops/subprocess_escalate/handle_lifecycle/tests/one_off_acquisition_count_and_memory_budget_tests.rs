// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One-off acquisitions: each node's count of one shape, refused by name at
//! its own count and handed back on release or teardown; the device memory
//! budget an acquisition is refused at once against; and what a released
//! one-off leaves behind.

use super::*;

/// One node holding every frame its output pool may grow to, and every
/// one-off of a shape it may hold, refuses only itself: a second node's
/// pooled frame and one-off of the same shape are both served.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[allow(
    clippy::disallowed_macros,
    reason = "the test prints its measured numbers or its skip reason for whoever reads the run"
)]
fn one_nodes_held_frames_never_refuse_another_nodes_frames_of_the_same_shape() {
    use uuid::Uuid;

    use crate::core::context::texture_pools_and_rings::node_output_surface_pool::NODE_OUTPUT_SURFACE_POOL_CAPACITY;
    use crate::core::context::texture_pools_and_rings::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let node_a = EscalateHandleRegistry::new();
    let node_b = EscalateHandleRegistry::new();
    let node_a_pool_key = format!("node-a-pool-{}", Uuid::new_v4().simple());
    let node_b_pool_key = format!("node-b-pool-{}", Uuid::new_v4().simple());
    let consumer = live_service.check_out_leases().mint_holder_id();

    let node_a_frames_the_consumer_holds: Vec<String> = (0..NODE_OUTPUT_SURFACE_POOL_CAPACITY)
        .map(|_| {
            let frame =
                next_node_output_frame_over_the_escalate_op(&sandbox, &node_a, &node_a_pool_key, 2)
                    .expect("node A's output pool grows while a consumer holds its frames");
            live_service
                .check_out_leases()
                .record_check_out_lease(&frame, consumer)
                .expect("the consumer holds every frame node A publishes");
            frame
        })
        .collect();
    let node_a_pool_refusal =
        next_node_output_frame_over_the_escalate_op(&sandbox, &node_a, &node_a_pool_key, 2)
            .expect_err("node A's seventeenth held frame is refused");
    assert!(
        node_a_pool_refusal.contains(&node_a_pool_key),
        "the refusal names node A's own pool: {node_a_pool_refusal}"
    );

    for _ in 0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE {
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &node_a)
            .expect("node A acquires one-offs up to its own count of the shape");
    }
    let node_a_one_off_refusal =
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &node_a)
            .expect_err("node A's seventeenth one-off of the shape is refused");
    assert!(
        node_a_one_off_refusal.contains("64x32 rgba8_unorm")
            && node_a_one_off_refusal.contains("16"),
        "the refusal names the shape and the count: {node_a_one_off_refusal}"
    );

    next_node_output_frame_over_the_escalate_op(&sandbox, &node_b, &node_b_pool_key, 2)
        .expect("node B's pooled frame of the same shape is served while node A holds its own");
    acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &node_b)
        .expect("node B's one-off of the same shape is served while node A holds its own");

    for frame in &node_a_frames_the_consumer_holds {
        live_service
            .check_out_leases()
            .release_one_check_out_lease(frame, consumer)
            .expect("the consumer lets each frame go");
    }
    release_everything_the_helpers_teardown_owes(&sandbox, &node_a);
    release_everything_the_helpers_teardown_owes(&sandbox, &node_b);
    live_service.stop();
}

/// At the device's memory budget every escalate acquisition that has to
/// allocate — a one-off, a pooled frame, a pixel buffer, a tensor — is
/// refused at once by the budget, never sitting out a wait for a holder to let
/// go. Another node holds every one-off of the tested node's requested shape
/// one node may hold, so no texture of that shape is free and the request
/// must allocate; they are released only after every answer is in, so an
/// acquire that waited on them would miss its answer's deadline rather than
/// be rescued.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[allow(
    clippy::disallowed_macros,
    reason = "the test prints its measured numbers or its skip reason for whoever reads the run"
)]
fn an_acquisition_at_the_budget_refuses_at_once_and_never_waits() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use uuid::Uuid;

    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireStorageBuffer,
        EscalateRequestAcquireTexture, EscalateRequestNodeOutputPool,
    };
    use crate::core::context::texture_pools_and_rings::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    const LONGEST_AN_ANSWER_AT_THE_BUDGET_MAY_TAKE: Duration = Duration::from_millis(500);
    const LONGEST_THE_TEST_WAITS_FOR_AN_ANSWER: Duration = Duration::from_secs(2);

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let another_node = EscalateHandleRegistry::new();
    let one_offs_another_node_holds: Vec<String> = (0
        ..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
        .map(|_| {
            acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(
                &sandbox,
                &another_node,
            )
            .expect("another node acquires the one-offs it holds through the test")
        })
        .collect();
    let requested_shape = the_64x32_rgba8_unorm_one_off_texture_shape();

    let _frame_allocation_budget_left_with_no_room = sandbox
        .host_inner()
        .device()
        .inner
        .leave_no_room_in_any_frame_allocation_budget_for_this_test();

    let a_storage_buffer_request = |shape: Vec<u64>, pool_key: Option<String>| {
        EscalateRequest::AcquireStorageBuffer(EscalateRequestAcquireStorageBuffer {
            request_id: "req-tensor-at-the-budget".to_string(),
            shape,
            dtype: "float32".to_string(),
            node_output_pool: pool_key.map(|pool_key| EscalateRequestNodeOutputPool {
                pool_key,
                rotation_depth: 2,
            }),
        })
    };
    let requests_at_the_budget: Vec<(&'static str, EscalateRequest)> = vec![
        (
            "a one-off texture of the shape another node holds every one-off of",
            a_one_off_64x32_rgba8_unorm_texture_request(),
        ),
        (
            "a pooled texture of a shape nobody holds",
            EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
                request_id: "req-pooled-at-the-budget".to_string(),
                width: 96,
                height: 48,
                format: "rgba8_unorm".to_string(),
                node_output_pool: Some(EscalateRequestNodeOutputPool {
                    pool_key: format!("budget-texture-pool-{}", Uuid::new_v4().simple()),
                    rotation_depth: 2,
                }),
                usage: vec!["texture_binding".to_string()],
            }),
        ),
        (
            "a pixel buffer of a fresh shape",
            EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
                request_id: "req-pixel-buffer-at-the-budget".to_string(),
                width: 48,
                height: 16,
                format: "bgra".to_string(),
            }),
        ),
        (
            "a one-off tensor",
            a_storage_buffer_request(vec![1, 3, 16, 16], None),
        ),
        (
            "a pooled tensor",
            a_storage_buffer_request(
                vec![1, 3, 32, 32],
                Some(format!("budget-tensor-pool-{}", Uuid::new_v4().simple())),
            ),
        ),
    ];
    let request_count = requests_at_the_budget.len();

    std::thread::scope(|scope| {
        // The body owns the release sender and the answer receiver, so a failed
        // assertion drops both before the scope joins: the other node's
        // one-offs are let go, the requester stops at its next answer, and the
        // test fails rather than hangs.
        let (release_signal_tx, release_signal_rx) = mpsc::channel::<()>();
        let (answer_tx, answer_rx) =
            mpsc::channel::<(&'static str, Duration, std::result::Result<String, String>)>();
        let releasing_sandbox = sandbox.clone();
        let releasing_registry_of_another_node = std::sync::Arc::clone(&another_node);
        let one_offs_another_node_holds = &one_offs_another_node_holds;
        scope.spawn(move || {
            let _signalled_or_the_body_failed = release_signal_rx.recv();
            for one_off in one_offs_another_node_holds {
                release_a_handle_over_the_escalate_op(
                    &releasing_sandbox,
                    &releasing_registry_of_another_node,
                    one_off,
                )
                .expect("another node's one-off releases");
            }
        });
        let requesting_sandbox = sandbox.clone();
        let requesting_registry = std::sync::Arc::clone(&registry);
        scope.spawn(move || {
            for (what_is_requested, request) in requests_at_the_budget {
                let requested_at = Instant::now();
                let answer = handle_id_or_refusal_of(super::super::super::handle_escalate_op(
                    &requesting_sandbox,
                    &requesting_registry,
                    &WhatAHelperPlacedNodeReaches::default(),
                    request,
                ));
                if answer_tx
                    .send((what_is_requested, requested_at.elapsed(), answer))
                    .is_err()
                {
                    return;
                }
            }
        });

        for _ in 0..request_count {
            let (what_is_requested, answered_after, answer) = answer_rx
                .recv_timeout(LONGEST_THE_TEST_WAITS_FOR_AN_ANSWER)
                .expect("every acquisition at the budget is answered, never left waiting");
            let refusal = answer.expect_err(what_is_requested);
            assert!(
                refusal.contains("refused a")
                    && refusal.contains("frame allocation before allocating")
                    && !refusal.contains("timeout")
                    && !refusal.contains("exhausted"),
                "{what_is_requested}: the refusal is the device memory budget's, by name: \
                 {refusal}"
            );
            assert!(
                answered_after < LONGEST_AN_ANSWER_AT_THE_BUDGET_MAY_TAKE,
                "{what_is_requested}: answered after {answered_after:?}, so it waited"
            );
        }
        assert_eq!(registry.handle_count(), 0, "the tested node holds nothing");
        assert_eq!(
            registry.one_off_acquisitions_held_or_reserved_of_shape(&requested_shape),
            0,
            "the refused one-off handed its reservation back"
        );
        assert_eq!(
            another_node.handle_count(),
            ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE,
            "another node still holds every one-off it acquired"
        );
        release_signal_tx
            .send(())
            .expect("the releasing thread is waiting for the signal");
    });

    assert_eq!(another_node.handle_count(), 0);
    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
    release_everything_the_helpers_teardown_owes(&sandbox, &another_node);
    live_service.stop();
}

/// A one-off's count goes back to its node when the helper releases it, and
/// every count it holds goes back at teardown.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[allow(
    clippy::disallowed_macros,
    reason = "the test prints its measured numbers or its skip reason for whoever reads the run"
)]
fn a_released_or_torn_down_one_off_hands_its_count_back_to_its_node() {
    use super::super::RegisteredHandle;
    use crate::core::context::texture_pools_and_rings::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();
    let shape = the_64x32_rgba8_unorm_one_off_texture_shape();
    let acquire_every_one_off_the_node_may_hold = || -> Vec<String> {
        (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
            .map(|_| {
                acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(
                    &sandbox, &registry,
                )
                .expect("the node acquires up to its own count of the shape")
            })
            .collect()
    };

    let held = acquire_every_one_off_the_node_may_hold();
    assert!(
        registry.read_the_one_off_held_under(&held[0], |held_one_off| {
            matches!(held_one_off, Some(RegisteredHandle::Texture { .. }))
        }),
        "a one-off texture is held under the id the helper was answered with"
    );
    let refusal =
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &registry)
            .expect_err("the seventeenth is refused");
    assert!(refusal.contains("64x32 rgba8_unorm"), "got: {refusal}");

    release_a_handle_over_the_escalate_op(&sandbox, &registry, &held[0])
        .expect("the helper releases one");
    assert_eq!(
        registry.one_off_acquisitions_held_or_reserved_of_shape(&shape),
        ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE - 1
    );
    acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &registry)
        .expect("a released one-off's count makes room for one more");

    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
    assert_eq!(registry.handle_count(), 0);
    assert_eq!(
        registry.one_off_acquisitions_held_or_reserved_of_shape(&shape),
        0
    );
    acquire_every_one_off_the_node_may_hold();
    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
}

/// A torn-down helper's one-off textures are freed as soon as nothing holds
/// them, without waiting out the idle window an explicit release waits.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_torn_down_helpers_textures_are_freed_without_waiting_out_the_idle_window() {
    use crate::core::context::{GpuContext, GpuContextLimitedAccess, TexturePoolDescriptor};
    use crate::core::rhi::TextureFormat;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        tracing::warn!("no GPU device, so the teardown freeing test skips");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let texture_pool = gpu.texture_pool().clone();
    let sandbox = GpuContextLimitedAccess::new(gpu.clone());
    let registry = EscalateHandleRegistry::new();
    for _ in 0..2 {
        acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(&sandbox, &registry)
            .expect("the helper acquires a one-off texture");
    }
    assert_eq!(texture_pool.stats().total_textures, 2);

    release_everything_the_helpers_teardown_owes(&sandbox, &registry);
    crate::core::test_support::wait_for_every_gpu_submission_made_so_far(&gpu);
    let _engines_own = texture_pool
        .acquire(
            &TexturePoolDescriptor::new(16, 16, TextureFormat::Rgba8Unorm),
            None,
        )
        .expect("an acquire after the teardown");
    let stats = texture_pool.stats();
    assert_eq!(
        (stats.total_textures, stats.textures_in_use),
        (1, 1),
        "a torn-down helper's texture waited out the idle window"
    );
    live_service.stop();
}

/// A render-target image is a one-off like any other: held under the id the
/// helper was answered with, counted under its shape, and its count handed
/// back when the helper releases it.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[allow(
    clippy::disallowed_macros,
    reason = "the test prints its measured numbers or its skip reason for whoever reads the run"
)]
fn a_render_target_image_is_held_and_counted_under_its_shape_until_the_helper_releases_it() {
    use super::super::{OneOffAcquisitionShape, RegisteredHandle};
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::TextureFormat;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let format = TextureFormat::Bgra8Unorm;
    let device_has_a_render_target_modifier_for_the_format = sandbox
        .escalate(|full| {
            Ok(full
                .host_vulkan_device_arc()?
                .has_render_target_modifier_for_texture_format(format))
        })
        .expect("the device answers whether it has a render-target modifier");
    if skip_unless_this_machine_has(
        RigOnlyCapability::DmaBufModifiers,
        device_has_a_render_target_modifier_for_the_format,
    ) {
        return;
    }
    let registry = EscalateHandleRegistry::new();
    let shape = OneOffAcquisitionShape::RenderTargetImage {
        width: 64,
        height: 32,
        format,
    };

    let handle_id = handle_id_or_refusal_of(super::super::super::handle_escalate_op(
        &sandbox,
        &registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquireImage(EscalateRequestAcquireImage {
            request_id: "req-render-target-image".to_string(),
            width: 64,
            height: 32,
            format: format.wire_name().to_string(),
        }),
    ))
    .expect("the helper acquires a render-target image");
    assert!(
        registry.read_the_one_off_held_under(&handle_id, |held_one_off| {
            matches!(held_one_off, Some(RegisteredHandle::Image { .. }))
        }),
        "the image is held under the id the helper was answered with"
    );
    assert_eq!(
        registry.one_off_acquisitions_held_or_reserved_of_shape(&shape),
        1
    );

    release_a_handle_over_the_escalate_op(&sandbox, &registry, &handle_id)
        .expect("the helper releases the image");
    assert_eq!(registry.handle_count(), 0);
    assert_eq!(
        registry.one_off_acquisitions_held_or_reserved_of_shape(&shape),
        0
    );
}

/// Releasing a one-off pixel buffer evicts the lookup its check-in id cached
/// and leaves its slot's published frame cached: on macOS the one-off's id is
/// that published frame's, which the pool alone retires.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[allow(
    clippy::disallowed_macros,
    reason = "the test prints its measured numbers or its skip reason for whoever reads the run"
)]
fn releasing_a_one_off_pixel_buffer_evicts_its_lookup_and_leaves_its_slots_published_frame_cached()
{
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquirePixelBuffer;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        println!("no GPU device — skipping");
        return;
    };
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    let handle_id = handle_id_or_refusal_of(super::super::super::handle_escalate_op(
        &sandbox,
        &registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
            request_id: "req-one-off-pixel-buffer".to_string(),
            width: 64,
            height: 32,
            format: "bgra".to_string(),
        }),
    ))
    .expect("a one-off pixel buffer is acquired");
    let published_frame_ids = sandbox
        .host_inner()
        .published_pixel_buffer_frame_ids_cached_in_this_process();
    let [published_frame_id] = published_frame_ids.as_slice() else {
        panic!("one hand-off publishes one frame: {published_frame_ids:?}");
    };
    #[cfg(target_os = "linux")]
    {
        assert_ne!(
            &handle_id, published_frame_id,
            "on Linux a one-off is checked in under its own id"
        );
        sandbox
            .host_inner()
            .get_pixel_buffer(&handle_id)
            .expect("the check-in id resolves through the surface store");
        assert!(
            sandbox
                .host_inner()
                .pooled_backing_held_in_this_process(&handle_id)
                .is_some(),
            "resolving the check-in id caches its lookup"
        );
    }
    #[cfg(target_os = "macos")]
    assert_eq!(
        &handle_id, published_frame_id,
        "on macOS a one-off's id is its slot's published frame"
    );

    release_a_handle_over_the_escalate_op(&sandbox, &registry, &handle_id)
        .expect("the helper releases the one-off");

    assert!(
        sandbox
            .host_inner()
            .pooled_backing_held_in_this_process(published_frame_id)
            .is_some(),
        "releasing a one-off leaves its slot's published frame cached"
    );
    #[cfg(target_os = "linux")]
    assert!(
        sandbox
            .host_inner()
            .pooled_backing_held_in_this_process(&handle_id)
            .is_none(),
        "releasing a one-off evicts the lookup its check-in id cached"
    );
    live_service.stop();
}

/// A node's one-offs of one shape are refused by name at the node's own
/// count, while another shape, and the same shape on another node, still
/// acquire — and handing a reservation back makes room for one more.
#[test]
fn a_nodes_one_off_acquisitions_are_refused_at_its_own_count_by_name() {
    use crate::core::context::texture_pools_and_rings::one_off_acquisitions_held_for_one_node::ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE;
    use crate::core::context::texture_pools_and_rings::one_off_acquisitions_held_for_one_node::one_off_acquisition_shapes_for_tests::{
        a_640x480_bgra32_pixel_buffer, a_1280x720_rgba8_unorm_texture,
    };

    let node_a = EscalateHandleRegistry::new();
    let node_b = EscalateHandleRegistry::new();

    let mut node_a_reservations: Vec<_> = (0..ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE)
        .map(|_| {
            node_a
                .reserve_one_off_acquisition_count(a_1280x720_rgba8_unorm_texture())
                .expect("a node acquires up to its own count of one shape")
        })
        .collect();
    assert_eq!(node_a_reservations.len(), 16);

    let Err(refusal) = node_a.reserve_one_off_acquisition_count(a_1280x720_rgba8_unorm_texture())
    else {
        panic!("the node's seventeenth one-off of one shape is refused");
    };
    assert!(
        matches!(
            refusal,
            crate::core::Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld {
                one_off_acquisitions_one_node_may_hold_of_one_shape: 16,
                ..
            }
        ),
        "the refusal is the node's own one-off count, not a pool's: {refusal:?}"
    );
    let refusal_text = refusal.to_string();
    assert!(
        refusal_text.contains("1280x720 rgba8_unorm") && refusal_text.contains("16"),
        "the refusal names the shape and the count: {refusal_text}"
    );
    assert!(
        !refusal_text.contains("node output pool"),
        "a one-off refusal is not the output pool's: {refusal_text}"
    );

    let _node_a_reservation_of_another_shape = node_a
        .reserve_one_off_acquisition_count(a_640x480_bgra32_pixel_buffer())
        .expect("another shape on the same node is counted on its own");
    let _node_b_reservation_of_the_same_shape = node_b
        .reserve_one_off_acquisition_count(a_1280x720_rgba8_unorm_texture())
        .expect("another node's one-offs of the same shape are never refused by this node's");

    node_a_reservations.pop();
    let _node_a_reservation_in_the_room_handed_back = node_a
        .reserve_one_off_acquisition_count(a_1280x720_rgba8_unorm_texture())
        .expect("a reservation handed back unheld makes room for one more");
}

/// A one-off 64x32 `rgba8_unorm` `texture_binding` texture request, the
/// shape the per-node count tests acquire.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_one_off_64x32_rgba8_unorm_texture_request()
-> crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest {
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireTexture;

    EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
        request_id: "req-one-off-64x32".to_string(),
        width: 64,
        height: 32,
        format: crate::core::rhi::TextureFormat::Rgba8Unorm
            .wire_name()
            .to_string(),
        node_output_pool: None,
        usage: vec!["texture_binding".to_string()],
    })
}

/// The shape a one-off 64x32 `rgba8_unorm` `texture_binding` texture is
/// counted under, the wire's implied copy bits included.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn the_64x32_rgba8_unorm_one_off_texture_shape() -> super::super::OneOffAcquisitionShape {
    super::super::OneOffAcquisitionShape::Texture {
        width: 64,
        height: 32,
        format: crate::core::rhi::TextureFormat::Rgba8Unorm,
        usage: super::super::super::acquisition::parse_texture_usages(&[
            "texture_binding".to_string()
        ])
        .expect("a known usage"),
    }
}

/// Acquire a one-off 64x32 `rgba8_unorm` texture over the real escalate op,
/// answering its handle id or the refusal.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn acquire_a_one_off_64x32_rgba8_unorm_texture_over_the_escalate_op(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
) -> std::result::Result<String, String> {
    handle_id_or_refusal_of(super::super::super::handle_escalate_op(
        sandbox,
        registry,
        &WhatAHelperPlacedNodeReaches::default(),
        a_one_off_64x32_rgba8_unorm_texture_request(),
    ))
}

/// Run every release a helper's teardown owes, as its bridge's drop does.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn release_everything_the_helpers_teardown_owes(
    sandbox: &crate::core::context::GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
) {
    super::super::release_everything_a_torn_down_helper_held(sandbox, registry);
}
