// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// =====================================================================
// Native nodes' output pools over the shared pixel-buffer pool
//
// A native built-in's per-frame pixel buffers come from an output pool of
// its own, whose slots it borrows from the shared ring of their shape: the
// count is the node's, the memory the runtime's.
// =====================================================================

/// A native node whose consumers hold every frame its pool may is refused
/// only its own next frame: a second native node publishing the same
/// shape, and the shared pool itself, go on being served.
/// GPU-gated: skips when no device is present.
#[test]
fn a_native_nodes_held_frames_never_refuse_a_second_native_nodes_frames_of_the_same_shape() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let consumer = check_out_leases.mint_holder_id();
    let mut first_native_node =
        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("first-native-node");
    let mut second_native_node =
        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("second-native-node");

    lease_every_frame_a_native_node_may_hold(&mut first_native_node, &check_out_leases, consumer);
    assert!(
        matches!(
            acquire_one_native_node_frame_id(&mut first_native_node),
            Err(Error::EverySlotInTheProcessorOutputPoolIsInUse { .. })
        ),
        "the first node is refused at its own cap"
    );

    let second_nodes_frames: Vec<String> = (0
        ..crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY)
        .map(|_| {
            let frame = acquire_one_native_node_frame_id(&mut second_native_node)
                .expect("another node's frames of the same shape are served");
            check_out_leases
                .record_check_out_lease(&frame, consumer)
                .expect("a consumer holds the second node's frame");
            frame
        })
        .collect();
    assert_eq!(
        second_nodes_frames.len(),
        crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY,
        "the second node reaches its own cap, untouched by the first's"
    );
    acquire_one_pool_slot_id(&gpu)
        .expect("the shared pool serves a frame of the same shape while both nodes hold theirs");
}

/// A node at its cap is refused at once by the processor output pool's
/// refusal, naming the node and the shape, before anything is allocated;
/// a frame its consumer lets go of serves the next one.
/// GPU-gated: skips when no device is present.
#[test]
fn a_native_nodes_seventeenth_held_frame_is_refused_naming_its_pool() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let consumer = check_out_leases.mint_holder_id();
    let mut native_node =
        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-held-native-node");
    let held_frames =
        lease_every_frame_a_native_node_may_hold(&mut native_node, &check_out_leases, consumer);
    let shared_ring_before_the_refusal =
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);

    let refusal_started = std::time::Instant::now();
    let refusal = acquire_one_native_node_frame_id(&mut native_node)
        .expect_err("a node holding every frame its pool may is refused");
    let refusal_took = refusal_started.elapsed();

    let expected_pool_key = format!(
        "native-node-output-pixel-buffer-pool-a-held-native-node-{LEASE_TEST_SURFACE_WIDTH}x{LEASE_TEST_SURFACE_HEIGHT}-{}",
        PixelFormat::Rgba32.wire_name()
    );
    match &refusal {
        Error::EverySlotInTheProcessorOutputPoolIsInUse {
            pool_key,
            pool_capacity,
        } => {
            assert_eq!(pool_key, &expected_pool_key);
            assert_eq!(
                *pool_capacity,
                crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
            );
        }
        other => panic!("expected the node output pool's refusal, got {other:?}"),
    }
    assert!(
        refusal.to_string().contains(&expected_pool_key),
        "the refusal reads the node's pool: {refusal}"
    );
    assert!(
        refusal_took < std::time::Duration::from_millis(250),
        "the refusal came after {refusal_took:?}: the node was made to wait"
    );
    assert_eq!(
        native_node.slot_count(),
        crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY
    );
    assert_eq!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE),
        shared_ring_before_the_refusal,
        "a refusal borrows and allocates nothing"
    );

    check_out_leases
        .release_one_check_out_lease(&held_frames[0], consumer)
        .expect("the consumer lets one frame go");
    let after_the_release = acquire_one_native_node_frame_id(&mut native_node)
        .expect("a frame let go of serves the node's next");
    assert!(same_pool_slot(&after_the_release, &held_frames[0]));
}

/// Each frame a native node's output pool hands out names the claim-table
/// row of its slot at the generation it published, so a reader in another
/// process claims exactly that frame.
/// GPU-gated: skips when no device is present.
#[test]
fn a_native_nodes_frame_names_its_slots_claim_table_row_at_the_generation_it_published() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the native node claim-table row test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let mut native_node =
        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-described-node");
    for _ in 0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD + 1 {
        let frame = native_node
            .acquire_the_next_output_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a node under its cap is served");
        let gpu_pool_slot_frame = frame
            .gpu_pool_slot_frame
            .expect("a slot the surface-share service shares leases a claim-table row");
        let claim_table = live_service
            .check_out_leases()
            .gpu_pool_claim_tables()
            .table_named(&gpu_pool_slot_frame.pool)
            .expect("the frame names a claim table the service holds");
        let pool_slot_key = claim_table
            .rows_leased_to_slots()
            .into_iter()
            .find(|leased_row| leased_row.row_index == gpu_pool_slot_frame.slot)
            .map(|leased_row| leased_row.pool_slot_key)
            .expect("the frame names a leased row");
        let published_frame_id = frame.published_frame_id.to_string();
        assert!(
            same_pool_slot(&published_frame_id, &pool_slot_key),
            "row {} is slot {pool_slot_key}, and the frame is {published_frame_id}",
            gpu_pool_slot_frame.slot
        );
        assert_eq!(
            gpu_pool_slot_frame.generation,
            frame.published_frame_id.frame_generation()
        );
        assert_eq!(
            claim_table.shared_rows().rows()[gpu_pool_slot_frame.slot as usize]
                .published_generation(),
            gpu_pool_slot_frame.generation,
            "the row publishes the generation the frame names"
        );
    }
}

/// Native nodes may borrow every slot a shape's shared ring holds; the
/// ring then grows rather than refusing its own producers.
/// GPU-gated: skips when no device is present.
#[test]
fn the_shared_pixel_buffer_pool_grows_when_native_nodes_have_borrowed_every_slot_of_a_shape() {
    let Some((gpu, _check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let native_nodes_that_borrow_every_pre_allocated_slot =
        POOL_PRE_ALLOCATE_COUNT.div_ceil(SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD);
    let mut native_nodes: Vec<_> = (0..native_nodes_that_borrow_every_pre_allocated_slot)
        .map(|native_node_index| {
            gpu.create_processor_output_pixel_buffer_pool_for_a_native_node(&format!(
                "a-borrowing-node-{native_node_index}"
            ))
        })
        .collect();
    let borrowed_frames: Vec<String> = native_nodes
        .iter_mut()
        .flat_map(|native_node| {
            (0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD)
                .map(|_| acquire_one_native_node_frame_id(native_node).expect("the node's frame"))
                .collect::<Vec<String>>()
        })
        .collect();
    assert!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
        "the nodes borrowed every slot the shared ring pre-allocated"
    );

    let shared_frame = acquire_one_pool_slot_id(&gpu)
        .expect("the shared ring grows when every slot of the shape is borrowed");
    assert!(
        !borrowed_frames
            .iter()
            .any(|borrowed| same_pool_slot(borrowed, &shared_frame)),
        "the shared ring handed out a slot a node has borrowed"
    );
    assert!(
        !shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
        "the shared ring grew"
    );
}

/// A node whose frames change shape hands every slot of the old shape
/// back to that shape's shared ring and borrows the new shape's.
/// GPU-gated: skips when no device is present.
#[test]
fn a_native_nodes_shape_change_returns_the_old_shapes_slots() {
    let Some((gpu, _check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let mut native_node =
        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-resized-node");
    let old_shapes_frames: Vec<String> = (0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD)
        .map(|_| acquire_one_native_node_frame_id(&mut native_node).expect("the node's frame"))
        .collect();
    let slots_the_shared_ring_kept =
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    assert_eq!(
        slots_the_shared_ring_kept.len(),
        PRE_ALLOCATED_SLOTS_THE_SHARED_RING_KEEPS_PAST_ONE_NATIVE_NODES_ROTATION
    );

    let crate::core::context::NativeNodeOutputPixelBufferFrame {
        published_frame_id: new_shapes_frame,
        pixel_buffer: new_shapes_buffer,
        ..
    } = native_node
        .acquire_the_next_output_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH * 2,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("the node's first frame of its new shape");
    assert_eq!(
        (new_shapes_buffer.width, new_shapes_buffer.height),
        (LEASE_TEST_SURFACE_WIDTH * 2, LEASE_TEST_SURFACE_HEIGHT)
    );
    assert!(
        !old_shapes_frames
            .iter()
            .any(|old| same_pool_slot(old, &new_shapes_frame.to_string())),
        "a frame of the new shape is published from a slot of the new shape"
    );
    assert_eq!(native_node.slot_count(), 1);

    let shared_ring_slots = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    let mut shared_ring_slot_keys: Vec<&str> = shared_ring_slots
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    let mut kept_and_old_shapes_slot_keys: Vec<&str> = slots_the_shared_ring_kept
        .iter()
        .map(|(key, _)| key.as_str())
        .chain(
            old_shapes_frames
                .iter()
                .map(|frame| pool_slot_key_of_surface_id(frame)),
        )
        .collect();
    shared_ring_slot_keys.sort_unstable();
    kept_and_old_shapes_slot_keys.sort_unstable();
    assert_eq!(
        shared_ring_slot_keys, kept_and_old_shapes_slot_keys,
        "every slot of the old shape is back in its shared ring"
    );
}
