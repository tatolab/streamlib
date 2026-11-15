// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// =====================================================================
// The pool's cross-process taken-until-released test
//
// In-process, a held slot is an Arc above its baseline. These cover the
// other holder the pool could not see until #1866: a helper child that
// checked the surface out over the surface-share socket. See
// the surface-id lifetime contract.
// =====================================================================

/// The contract itself: a frame a child holds is never handed back to the
/// producer to overwrite, and comes back the moment the child lets go.
///
/// Mental-revert: dropping the lease consult from `acquire` makes the
/// first loop rehand the leased slot on its very next ring cycle — #1755's
/// reproduction, at pool level.
/// GPU-gated: skips when no device is present.
#[test]
fn a_leased_slot_is_never_rehanded_to_its_producer() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let ring = pool_slot_ids_in_ring_order(&gpu);
    let held_by_a_child = ring[0].clone();
    let child = check_out_leases.mint_holder_id();
    check_out_leases
        .record_check_out_lease(&held_by_a_child, child)
        .expect("the child checks the frame out");

    for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 2) {
        let handed = acquire_one_pool_slot_id(&gpu).expect("free slots remain");
        assert!(
            !same_pool_slot(&handed, &held_by_a_child),
            "the producer was handed back the slot a child is still reading"
        );
    }

    check_out_leases
        .release_one_check_out_lease(&held_by_a_child, child)
        .expect("the child releases the frame");
    let comes_back = (0..(POOL_PRE_ALLOCATE_COUNT * 4)).any(|_| {
        acquire_one_pool_slot_id(&gpu).is_ok_and(|handed| same_pool_slot(&handed, &held_by_a_child))
    });
    assert!(
        comes_back,
        "a released slot must return to the producer, or the lease is a leak"
    );
}

/// Under lease pressure the pool grows rather than making the producer
/// wait or making a consumer's frame change — the "slow consumer costs
/// memory first" half of the ruling.
/// GPU-gated: skips when no device is present.
#[test]
fn the_pool_grows_rather_than_rehanding_any_leased_slot() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let ring = pool_slot_ids_in_ring_order(&gpu);
    let child = check_out_leases.mint_holder_id();
    for slot in &ring {
        check_out_leases
            .record_check_out_lease(slot, child)
            .expect("the child checks every published frame out");
    }

    let grown_slot = acquire_one_pool_slot_id(&gpu)
        .expect("with every slot leased the pool grows instead of refusing");
    assert!(
        !ring
            .iter()
            .any(|leased| same_pool_slot(leased, &grown_slot)),
        "the pool handed back a leased slot instead of growing"
    );
}

/// The shared pool caps no shape by count: while a consumer holds every
/// frame it is handed, each next acquire grows the pool. Past the heap
/// budget the producer drops its own frame, refused at once and naming the
/// heap — the consumer's cost is memory and then frames, never the
/// producer's cadence.
/// GPU-gated: skips when no device is present.
#[test]
fn past_the_heap_budget_the_shared_pixel_buffer_pool_refuses_its_producer_at_once_rather_than_making_it_wait()
 {
    const FRAMES_THE_CHILD_HOLDS_WHILE_THE_SHARED_POOL_GROWS: usize = 68;

    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let child = check_out_leases.mint_holder_id();
    let leased_slots: Vec<String> = (0..FRAMES_THE_CHILD_HOLDS_WHILE_THE_SHARED_POOL_GROWS)
        .map(|_| {
            let slot = acquire_one_pool_slot_id(&gpu)
                .expect("with every slot leased the shared pool grows");
            check_out_leases
                .record_check_out_lease(&slot, child)
                .expect("the child holds on to every frame it is given");
            slot
        })
        .collect();
    let pool_memory_type_index = gpu
        .get_pixel_buffer(&leased_slots[0])
        .expect("a leased frame still resolves")
        .buffer_ref()
        .inner
        .vma_allocation_memory_type_index()
        .expect("a pool slot is the allocator's own allocation");
    let pool_heap_index = gpu
        .device()
        .inner
        .device_memory_heap_of_memory_type(pool_memory_type_index)
        .expect("the slot's memory type is one of the device's");

    let _frame_allocation_budget_left_with_no_room = gpu
        .device()
        .inner
        .leave_no_room_in_any_frame_allocation_budget_for_this_test();
    let acquire_started = std::time::Instant::now();
    let refusal = acquire_one_pool_slot_id(&gpu)
        .expect_err("past the heap budget the producer is refused rather than served");
    let acquire_took = acquire_started.elapsed();

    match &refusal {
        Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget { heap_index, .. } => {
            assert_eq!(
                *heap_index, pool_heap_index,
                "the refusal names the heap the pool's buffers come from: {refusal}"
            );
        }
        other => panic!("expected the heap-budget refusal, got {other:?}"),
    }
    assert!(
        acquire_took < std::time::Duration::from_millis(250),
        "the refusal came after {acquire_took:?}: the producer was made to wait"
    );
}

/// Fail closed. A lease table that cannot be read cannot prove any slot is
/// free, and a slot reused on a guess is a frame changing under a reader.
/// Growth still serves the producer — a slot that has never existed cannot
/// be checked out — so the observable rule is "never reuse", not "never
/// acquire".
/// GPU-gated: skips when no device is present.
#[test]
fn an_unreadable_lease_table_stops_the_pool_reusing_any_slot() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let ring = pool_slot_ids_in_ring_order(&gpu);

    let poisoning = Arc::clone(&check_out_leases);
    let _ = std::thread::spawn(move || {
        let _held = poisoning.hold_for_pool_slot_hand_off().unwrap();
        panic!("poison the lease table");
    })
    .join();

    for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 2) {
        let handed = acquire_one_pool_slot_id(&gpu).expect("growth still serves the producer");
        assert!(
            !ring.iter().any(|known| same_pool_slot(known, &handed)),
            "a slot was reused while the lease table was unreadable"
        );
    }
}

/// #1872 itself, at pool level: one slot, two acquisitions, two ids —
/// and the first id stops resolving the moment the slot is recycled,
/// in-process (cache eviction) and in the lease registry (generation
/// ledger) alike.
/// GPU-gated: skips when no device is present.
#[test]
fn recycling_a_slot_retires_the_id_the_previous_frame_published() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };

    let first_published = acquire_one_pool_slot_id(&gpu).expect("first acquisition");
    gpu.resolve_pixel_buffer_by_surface_id(&first_published)
        .expect("a live published id resolves");

    // Cycle the whole ring so the first slot is recycled.
    let second_published = (0..POOL_PRE_ALLOCATE_COUNT)
        .map(|_| acquire_one_pool_slot_id(&gpu).expect("free slots remain"))
        .find(|later| same_pool_slot(later, &first_published))
        .expect("cycling the ring once rehands the first slot");

    assert_ne!(
        second_published, first_published,
        "recycling the slot must publish a new frame id"
    );
    gpu.resolve_pixel_buffer_by_surface_id(&second_published)
        .expect("the current frame id resolves");
    assert!(
        gpu.resolve_pixel_buffer_by_surface_id(&first_published)
            .is_err(),
        "a recycled slot's previous id must stop resolving — resolving it silently \
         serves somebody else's pixels, which is #1872"
    );
    let child = check_out_leases.mint_holder_id();
    assert!(
        matches!(
            check_out_leases.record_check_out_lease(&first_published, child),
            Err(Error::SurfaceFrameRecycled { .. })
        ),
        "a cross-process checkout of the retired id must be refused"
    );
}

/// The refusal is the pool's own, not the lease registry's: a context
/// with no surface store wired in — no service, no registry — still
/// refuses a retired id on the texture path, whose slot-keyed cache
/// would otherwise serve the slot's current texture under the dead id.
/// GPU-gated: skips when no device is present.
#[test]
fn a_retired_id_is_refused_even_with_no_lease_registry() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        println!("Skipping - no GPU device available");
        return;
    };

    let first_published = acquire_one_pool_slot_id(&gpu).expect("first acquisition");
    let desc = TextureDescriptor::new(
        LEASE_TEST_SURFACE_WIDTH,
        LEASE_TEST_SURFACE_HEIGHT,
        TextureFormat::Rgba8Unorm,
    )
    .with_usage(TextureUsages::TEXTURE_BINDING);
    let producer_texture = gpu
        .device()
        .create_texture(&desc)
        .expect("texture creation");
    gpu.register_texture_with_layout(
        &first_published,
        producer_texture,
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
    );
    gpu.resolve_texture_registration_by_surface_id(
        &first_published,
        None,
        LEASE_TEST_SURFACE_WIDTH,
        LEASE_TEST_SURFACE_HEIGHT,
    )
    .expect("the live published id resolves its producer's texture");

    let second_published = (0..POOL_PRE_ALLOCATE_COUNT)
        .map(|_| acquire_one_pool_slot_id(&gpu).expect("free slots remain"))
        .find(|later| same_pool_slot(later, &first_published))
        .expect("cycling the ring once rehands the first slot");

    assert!(
        matches!(
            gpu.resolve_texture_registration_by_surface_id(
                &first_published,
                None,
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
            ),
            Err(Error::SurfaceFrameRecycled { .. })
        ),
        "with no registry anywhere, the retired id must still be refused"
    );
    gpu.resolve_texture_registration_by_surface_id(
        &second_published,
        None,
        LEASE_TEST_SURFACE_WIDTH,
        LEASE_TEST_SURFACE_HEIGHT,
    )
    .expect("the current frame id keeps resolving");
}

/// A slot handed back while a consumer still leases the node's last frame
/// from it stays off the producer's ring until the consumer lets go, and
/// is then freed rather than handed out again.
/// GPU-gated: skips when no device is present.
#[test]
fn a_returned_slot_is_never_rehanded_while_its_last_frame_is_leased_and_is_freed_once_let_go() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let mut native_node =
        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node("a-leased-node");
    let leased_frame =
        acquire_one_native_node_frame_id(&mut native_node).expect("the node's first frame");
    let consumer = check_out_leases.mint_holder_id();
    check_out_leases
        .record_check_out_lease(&leased_frame, consumer)
        .expect("a consumer holds the node's frame");
    drop(native_node);

    for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 2) {
        let handed = acquire_one_pool_slot_id(&gpu).expect("free slots remain");
        assert!(
            !same_pool_slot(&handed, &leased_frame),
            "the shared ring rehanded a returned slot whose last frame a consumer holds"
        );
    }
    gpu.resolve_pixel_buffer_by_surface_id(&leased_frame)
        .expect("the leased frame still resolves to its own pixels");

    check_out_leases
        .release_one_check_out_lease(&leased_frame, consumer)
        .expect("the consumer lets the frame go");
    wait_for_every_gpu_submission_made_so_far(&gpu);
    for _ in 0..(POOL_PRE_ALLOCATE_COUNT * 4) {
        let handed = acquire_one_pool_slot_id(&gpu).expect("free slots remain");
        assert!(
            !same_pool_slot(&handed, &leased_frame),
            "a returned slot let go of was handed out again rather than freed"
        );
    }
    assert!(
        !shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE)
            .iter()
            .any(|(pool_slot_key, _)| pool_slot_key == pool_slot_key_of_surface_id(&leased_frame)),
        "a returned slot let go of stayed in the shared ring"
    );
    assert!(is_refused_as_recycled_everywhere(
        &gpu,
        &check_out_leases,
        &leased_frame
    ));
}
