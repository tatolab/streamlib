// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A torn-down node's pool frees each slot nothing holds — registration
/// released, last frame refused as recycled — and hands each held slot
/// back to the shared ring, generation kept, freed by the next acquire
/// once it is let go rather than after the idle window.
/// GPU-gated: skips when no device is present.
#[test]
fn a_torn_down_native_nodes_unheld_slots_are_freed_and_its_held_ones_handed_back_until_let_go() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the native node teardown freeing test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let check_out_leases = live_service.check_out_leases();
    let mut native_node =
        gpu.create_node_output_pixel_buffer_pool_for_a_native_node("a-torn-down-node");
    let mut frames_the_node_published = Vec::new();
    let mut buffer_held_in_this_process = None;
    for _ in 0..SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD {
        let crate::core::context::NativeNodeOutputPixelBufferFrame {
            published_frame_id,
            pixel_buffer: buffer,
            ..
        } = native_node
            .acquire_the_next_output_pixel_buffer(
                LEASE_TEST_SURFACE_WIDTH,
                LEASE_TEST_SURFACE_HEIGHT,
                PixelFormat::Rgba32,
            )
            .expect("a node under its cap is served");
        buffer_held_in_this_process.get_or_insert(buffer);
        frames_the_node_published.push(published_frame_id.to_string());
    }
    let [held_in_this_process, leased, unheld @ ..] = frames_the_node_published.as_slice() else {
        panic!("the node published from one borrowed slot per frame of its rotation");
    };
    let consumer = check_out_leases.mint_holder_id();
    check_out_leases
        .record_check_out_lease(leased, consumer)
        .expect("a consumer holds one of the node's frames");
    for frame in &frames_the_node_published {
        assert!(live_service.is_registered(pool_slot_key_of_surface_id(frame)));
    }
    wait_for_every_gpu_submission_made_so_far(&gpu);

    drop(native_node);
    for frame in unheld {
        assert!(
            !live_service.is_registered(pool_slot_key_of_surface_id(frame)),
            "an unheld slot of the torn-down node kept its registration"
        );
        assert!(
            is_refused_as_recycled_everywhere(&gpu, check_out_leases, frame),
            "the last frame of a freed slot is not refused as recycled"
        );
    }
    let mut handed_back: Vec<(String, u64)> =
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    handed_back.sort_unstable();
    let mut held: Vec<(String, u64)> = [held_in_this_process, leased]
        .into_iter()
        .map(|frame| {
            (
                pool_slot_key_of_surface_id(frame).to_string(),
                frame_generation_of(frame),
            )
        })
        .collect();
    held.sort_unstable();
    assert_eq!(
        handed_back, held,
        "exactly the held slots came back to the shared ring, generations kept"
    );
    gpu.resolve_pixel_buffer_by_surface_id(leased)
        .expect("a leased frame of the torn-down node still resolves");

    drop(buffer_held_in_this_process);
    check_out_leases
        .release_one_check_out_lease(leased, consumer)
        .expect("the consumer lets the frame go");
    for _ in 0..held.len() {
        gpu.acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH * 2,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("an acquire of another shape");
    }
    assert!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
        "a handed-back slot let go of waited out the idle window, or its ring outlived it"
    );
    for frame in [held_in_this_process, leased] {
        assert!(!live_service.is_registered(pool_slot_key_of_surface_id(frame)));
        assert!(is_refused_as_recycled_everywhere(
            &gpu,
            check_out_leases,
            frame
        ));
    }
    live_service.stop();
}

/// A stopped stream's shared pool entries nothing holds are freed at
/// once — registrations released, ids refused as recycled — while its
/// held ones and the engine's own entries stay; a held one is freed by
/// the next acquire once let go.
/// GPU-gated: skips when no device is present.
#[test]
fn a_stopped_streams_pool_entries_are_freed_and_their_ids_retired() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the stopped stream freeing test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let check_out_leases = live_service.check_out_leases();
    let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
    let stopped_streams_view =
        gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
    let engines_shape = PixelBufferPoolKey {
        width: LEASE_TEST_SURFACE_WIDTH * 2,
        ..LEASE_TEST_SURFACE_SHAPE
    };

    let (held_frame, held_buffer) = stopped_streams_view
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("the stream's held frame");
    let held_frame = held_frame.to_string();
    let let_go_frame = stopped_streams_view
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("the stream's frame it let go of")
        .0
        .to_string();
    let streams_texture_shape = TexturePoolDescriptor::new(
        LEASE_TEST_SURFACE_WIDTH,
        LEASE_TEST_SURFACE_HEIGHT,
        TextureFormat::Rgba8Unorm,
    )
    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
    drop(
        stopped_streams_view
            .acquire_texture(&streams_texture_shape)
            .expect("the stream's texture"),
    );
    gpu.acquire_pixel_buffer(
        engines_shape.width,
        engines_shape.height,
        engines_shape.format,
    )
    .expect("the engine's own frame");
    let streams_slots = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    let engines_slots_before_the_stop = shared_ring_slots_and_generations(&gpu, engines_shape);
    assert_eq!(streams_slots.len(), POOL_PRE_ALLOCATE_COUNT);
    for (pool_slot_key, _) in &streams_slots {
        assert!(live_service.is_registered(pool_slot_key));
    }
    wait_for_every_gpu_submission_made_so_far(&gpu);
    let device_memory_held_before_the_stop =
        device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);

    gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
    assert!(
        device_memory_bytes_the_allocator_holds_on_every_heap(&gpu)
            < device_memory_held_before_the_stop,
        "the stopped stream's freed entries gave no device memory back"
    );
    let held_slot_key = pool_slot_key_of_surface_id(&held_frame).to_string();
    assert_eq!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE),
        vec![(held_slot_key.clone(), frame_generation_of(&held_frame))],
        "only the slot the stream still holds outlived its stop"
    );
    for (pool_slot_key, _) in &streams_slots {
        assert_eq!(
            live_service.is_registered(pool_slot_key),
            *pool_slot_key == held_slot_key,
            "slot {pool_slot_key}'s registration does not follow whether it was freed"
        );
    }
    assert!(
        is_refused_as_recycled_everywhere(&gpu, check_out_leases, &let_go_frame),
        "a freed slot's last frame is not refused as recycled"
    );
    gpu.resolve_pixel_buffer_by_surface_id(&held_frame)
        .expect("the frame the stream still holds resolves");
    assert_eq!(
        gpu.texture_pool()
            .slot_count_of_the_shape_of(&streams_texture_shape),
        0,
        "the stopped stream's texture slot outlived its stop"
    );
    assert_eq!(
        shared_ring_slots_and_generations(&gpu, engines_shape),
        engines_slots_before_the_stop,
        "the engine's own slots went with a stream's stop"
    );

    drop(held_buffer);
    let _engines_next_frame = gpu
        .acquire_pixel_buffer(
            engines_shape.width,
            engines_shape.height,
            engines_shape.format,
        )
        .expect("the engine's next frame");
    assert!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
        "the stopped stream's slot it let go of waited out the idle window"
    );
    assert!(!live_service.is_registered(&held_slot_key));
    assert!(is_refused_as_recycled_everywhere(
        &gpu,
        check_out_leases,
        &held_frame
    ));
    live_service.stop();
}

/// A stopped stream's entry a helper still holds checked out when the stop
/// begins is freed by that stop once the helper's connection closes,
/// with no acquire after it.
/// GPU-gated: skips when no device is present.
#[test]
fn a_stopped_streams_entry_leased_until_its_helpers_connection_closes_is_freed_by_the_stop() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the leased stopped stream freeing test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let check_out_leases = live_service.check_out_leases();
    let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
    let stopped_streams_view =
        gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
    let leased_frame = stopped_streams_view
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("the stream's frame a helper leases")
        .0
        .to_string();
    let helpers_connection = check_out_leases.mint_holder_id();
    check_out_leases
        .record_check_out_lease(&leased_frame, helpers_connection)
        .expect("the helper checks the frame out");
    wait_for_every_gpu_submission_made_so_far(&gpu);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            // Late enough that the stop is already waiting on the lease,
            // well inside the longest it waits.
            std::thread::sleep(std::time::Duration::from_millis(100));
            check_out_leases
                .release_every_check_out_lease_held_by(helpers_connection)
                .expect("the helper's connection closes");
        });
        gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
    });

    assert!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
        "the stop left the slot whose lease cleared during it for a later acquire"
    );
    assert!(!live_service.is_registered(pool_slot_key_of_surface_id(&leased_frame)));
    assert!(is_refused_as_recycled_everywhere(
        &gpu,
        check_out_leases,
        &leased_frame
    ));
    live_service.stop();
}

/// A freed slot whose canvas texture a reader still holds keeps its
/// memory, retired and out of every cache, until the reader lets go;
/// the next acquire then destroys it.
/// GPU-gated: skips when no device is present.
#[test]
fn a_freed_pixel_buffer_slot_whose_canvas_a_reader_holds_is_destroyed_only_once_let_go() {
    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
    let stopped_streams_view =
        gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
    let (frame, buffer) = stopped_streams_view
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("the stream's frame");
    let frame = frame.to_string();
    let canvas_reader = gpu
        .resolve_texture_registration_by_surface_id(
            &frame,
            None,
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
        )
        .expect("the frame's canvas texture");
    drop(buffer);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
    assert!(
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).is_empty(),
        "the stopped stream's slots outlived its stop"
    );
    assert_eq!(
        gpu.pixel_buffer_pool_manager
            .slots_awaiting_destruction
            .count(),
        1,
        "the slot whose canvas a reader holds was destroyed under it, or another slot waits"
    );
    assert!(is_refused_as_recycled_everywhere(
        &gpu,
        &check_out_leases,
        &frame
    ));

    drop(canvas_reader);
    gpu.acquire_pixel_buffer(
        LEASE_TEST_SURFACE_WIDTH * 2,
        LEASE_TEST_SURFACE_HEIGHT,
        PixelFormat::Rgba32,
    )
    .expect("an acquire of another shape");
    assert_eq!(
        gpu.pixel_buffer_pool_manager
            .slots_awaiting_destruction
            .count(),
        0,
        "the slot let go of was not destroyed by the next acquire"
    );
}

/// A stopped stream's freed entries — a pixel-buffer slot and a texture
/// slot — leave their pools and retire their ids at once, but their
/// memory stays while GPU work submitted before their release, naming
/// them, is still pending; once it completes, the next acquire destroys
/// one of each.
/// GPU-gated: skips when no device is present.
#[test]
fn a_freed_entry_is_destroyed_only_once_the_gpu_work_submitted_before_its_release_completes() {
    use crate::core::rhi::VulkanLayout;
    use crate::host_rhi::{VulkanAccess, VulkanStage};
    use crate::vulkan::rhi::HostVulkanTimelineSemaphore;

    let Some((gpu, check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let stopped_stream = crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1);
    let stopped_streams_view =
        gpu.view_for_one_loaded_stream(stopped_stream, None, &std::env::temp_dir());
    let (frame, buffer) = stopped_streams_view
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("the stream's frame");
    let frame = frame.to_string();
    let streams_texture_shape = TexturePoolDescriptor::new(
        LEASE_TEST_SURFACE_WIDTH,
        LEASE_TEST_SURFACE_HEIGHT,
        TextureFormat::Rgba8Unorm,
    )
    .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
    let texture = stopped_streams_view
        .acquire_texture(&streams_texture_shape)
        .expect("the stream's texture");
    let streams_slot_count =
        shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE).len();
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let host_signalled_gate =
        HostVulkanTimelineSemaphore::new(gpu.device().inner.device(), 0).expect("gate timeline");
    let mut recorder = gpu
        .create_command_recorder("gpu-work-naming-freed-entries-test")
        .expect("recorder");
    recorder.begin().expect("begin");
    recorder
        .record_buffer_barrier(
            &buffer,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::MEMORY_READ,
        )
        .expect("a barrier naming the pixel buffer");
    recorder
        .record_image_barrier(
            texture.texture(),
            VulkanLayout::UNDEFINED,
            VulkanLayout::GENERAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::MEMORY_READ,
        )
        .expect("a barrier naming the texture");
    recorder
        .host_inner_mut()
        .submit_waiting_and_signaling_timeline(Some((&host_signalled_gate, 1)), None)
        .expect("the gated submission");
    drop(buffer);
    drop(texture);
    let device_memory_held_before_the_stop =
        device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);

    gpu.free_every_unheld_shared_pool_entry_the_stream_last_acquired(stopped_stream);
    // Read while the gate is shut, asserted once it is open: a failed
    // assertion must not leave the gated submission pending forever.
    let ring_slots_while_gated = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    let texture_slots_while_gated = gpu
        .texture_pool()
        .slot_count_of_the_shape_of(&streams_texture_shape);
    let frame_refused_as_recycled_while_gated =
        is_refused_as_recycled_everywhere(&gpu, &check_out_leases, &frame);
    let pixel_buffer_slots_awaiting_destruction_while_gated = gpu
        .pixel_buffer_pool_manager
        .slots_awaiting_destruction
        .count();
    let texture_slots_awaiting_destruction_while_gated =
        gpu.texture_pool().slot_count_awaiting_destruction();
    let device_memory_held_while_gated =
        device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);
    host_signalled_gate.signal_host(1).expect("open the gate");
    recorder
        .wait_for_completion()
        .expect("the gated submission completes");
    // The pools read completion from the device's per-queue completion
    // timeline, not the recorder's fence, and lavapipe under load can
    // report the fence signalled while that same submission's timeline
    // value has not yet advanced.
    wait_for_every_gpu_submission_made_so_far(&gpu);

    assert!(
        ring_slots_while_gated.is_empty(),
        "the stopped stream's slots stayed in their ring"
    );
    assert_eq!(
        texture_slots_while_gated, 0,
        "the stopped stream's texture slot stayed in its bucket"
    );
    assert!(frame_refused_as_recycled_while_gated);
    assert_eq!(
        pixel_buffer_slots_awaiting_destruction_while_gated, streams_slot_count,
        "a pixel-buffer slot was destroyed under GPU work still pending"
    );
    assert_eq!(
        texture_slots_awaiting_destruction_while_gated, 1,
        "the texture slot was destroyed under GPU work still pending"
    );
    assert!(
        device_memory_held_while_gated >= device_memory_held_before_the_stop,
        "memory was given back while GPU work naming it was pending"
    );

    gpu.acquire_pixel_buffer(
        LEASE_TEST_SURFACE_WIDTH * 2,
        LEASE_TEST_SURFACE_HEIGHT,
        PixelFormat::Rgba32,
    )
    .expect("an acquire of another shape");
    drop(
        gpu.texture_pool()
            .acquire(
                &TexturePoolDescriptor::new(
                    LEASE_TEST_SURFACE_WIDTH * 2,
                    LEASE_TEST_SURFACE_HEIGHT,
                    TextureFormat::Rgba8Unorm,
                ),
                None,
            )
            .expect("a texture of another shape"),
    );
    assert_eq!(
        gpu.pixel_buffer_pool_manager
            .slots_awaiting_destruction
            .count(),
        streams_slot_count - 1,
        "the acquire after the GPU work completed destroyed other than one pixel-buffer slot"
    );
    assert_eq!(
        gpu.texture_pool().slot_count_awaiting_destruction(),
        0,
        "the acquire after the GPU work completed did not destroy the texture slot"
    );
}

/// A one-off pixel buffer flagged through its buffer, as a torn-down
/// helper's are, is freed by the next acquire once let go rather than
/// after the idle window.
/// GPU-gated: skips when no device is present.
#[test]
fn a_pixel_buffer_slot_flagged_through_its_buffer_skips_the_idle_window() {
    let Some((gpu, _check_out_leases)) = gpu_context_reading_check_out_leases_or_skip() else {
        return;
    };
    let (frame, buffer) = gpu
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("a one-off frame");
    let flagged_slot_key = pool_slot_key_of_surface_id(&frame.to_string()).to_string();
    gpu.flag_the_pixel_buffer_pool_slot_of_the_buffer_freed_as_soon_as_unheld(&buffer);
    drop(buffer);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    gpu.acquire_pixel_buffer(
        LEASE_TEST_SURFACE_WIDTH * 2,
        LEASE_TEST_SURFACE_HEIGHT,
        PixelFormat::Rgba32,
    )
    .expect("an acquire of another shape");
    let slots_left = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    assert_eq!(
        slots_left.len(),
        POOL_PRE_ALLOCATE_COUNT - 1,
        "other than exactly the flagged slot was freed"
    );
    assert!(
        !slots_left
            .iter()
            .any(|(pool_slot_key, _)| *pool_slot_key == flagged_slot_key),
        "the flagged slot waited out the idle window"
    );
}

/// A slot left unused is kept inside the idle window and handed out
/// again; past it the next acquire frees it — one per acquire —
/// releasing its registration and its cached frame and retiring its id.
/// GPU-gated: skips when no device is present.
#[test]
fn an_entry_unused_past_the_idle_window_is_freed_and_releases_its_registration() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the idle window freeing test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let check_out_leases = live_service.check_out_leases();
    let acquire_and_hold = || {
        gpu.acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("a frame")
    };

    let unused_frame = acquire_and_hold().0.to_string();
    wait_for_every_gpu_submission_made_so_far(&gpu);
    let held_inside_the_window = acquire_and_hold();
    let slots_inside_the_window = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    assert_eq!(
        slots_inside_the_window.len(),
        POOL_PRE_ALLOCATE_COUNT,
        "an unused slot was freed inside the idle window"
    );

    gpu.pixel_buffer_pool_manager
        .idle_window
        .set_for_a_test(std::time::Duration::ZERO);
    let device_memory_held_inside_the_window =
        device_memory_bytes_the_allocator_holds_on_every_heap(&gpu);
    let held_past_the_window = acquire_and_hold();
    assert!(
        device_memory_bytes_the_allocator_holds_on_every_heap(&gpu)
            < device_memory_held_inside_the_window,
        "the slot freed past the idle window gave no device memory back"
    );
    let slots_past_the_window = shared_ring_slots_and_generations(&gpu, LEASE_TEST_SURFACE_SHAPE);
    assert_eq!(
        slots_past_the_window.len(),
        POOL_PRE_ALLOCATE_COUNT - 1,
        "an acquire freed other than exactly one of the two unused slots"
    );
    let freed_slot_key = pool_slot_key_of_surface_id(&unused_frame);
    assert!(
        !slots_past_the_window
            .iter()
            .any(|(pool_slot_key, _)| pool_slot_key == freed_slot_key),
        "the slot unused longest was not the one freed"
    );
    assert!(
        !live_service.is_registered(freed_slot_key),
        "a freed slot kept its surface-share registration"
    );
    for (pool_slot_key, _) in &slots_past_the_window {
        assert!(live_service.is_registered(pool_slot_key));
    }
    assert!(
        !gpu.published_pixel_buffer_frame_ids_cached_in_this_process()
            .iter()
            .any(|cached| pool_slot_key_of_surface_id(cached) == freed_slot_key),
        "a freed slot's frame stayed in the buffer cache"
    );
    assert!(is_refused_as_recycled_everywhere(
        &gpu,
        check_out_leases,
        &unused_frame
    ));
    drop((held_inside_the_window, held_past_the_window));
    live_service.stop();
}
