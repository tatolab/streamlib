// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Duration;

use super::*;
use crate::core::context::{GpuContext, SurfaceCheckOutLeaseRegistry, SurfaceStore};
use crate::core::test_support::wait_for_every_gpu_submission_made_so_far;

fn gpu_context_or_skip() -> Option<GpuContext> {
    match GpuContext::init_for_platform_sync() {
        Ok(gpu) => Some(gpu),
        Err(no_device) => {
            tracing::warn!("no GPU device, so the texture pool freeing test skips: {no_device}");
            None
        }
    }
}

fn a_16_high_rgba8_texture_of_width(width: u32) -> TexturePoolDescriptor {
    TexturePoolDescriptor::new(width, 16, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST)
}

/// What a stream's stop runs on the texture pool, with no lease to wait
/// out: flag the stream's slots, then free every unheld one due.
fn free_what_a_stopped_stream_last_acquired(pool: &TexturePool, stream: LoadedStreamTag) -> usize {
    pool.flag_every_slot_the_stream_last_acquired(stream);
    pool.free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work()
}

#[test]
fn a_texture_pool_slot_unheld_past_the_idle_window_is_freed_on_a_later_acquire() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let pool = gpu.texture_pool();
    pool.set_idle_window_for_a_test(Duration::ZERO);
    let idle_shape = a_16_high_rgba8_texture_of_width(16);
    drop(pool.acquire(&idle_shape, None).expect("a texture"));
    assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 1);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let _another_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture of another shape");
    assert_eq!(
        pool.slot_count_of_the_shape_of(&idle_shape),
        0,
        "an unheld slot past the idle window survived an acquire"
    );
}

#[test]
fn a_texture_pool_slot_unheld_inside_the_idle_window_is_kept_and_handed_out_again() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let pool = gpu.texture_pool();
    let idle_shape = a_16_high_rgba8_texture_of_width(16);
    let first = pool.acquire(&idle_shape, None).expect("a texture");
    let first_slot_id = first.slot_id();
    drop(first);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let _another_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture of another shape");
    assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 1);
    let again = pool.acquire(&idle_shape, None).expect("a texture");
    assert_eq!(again.slot_id(), first_slot_id);
}

#[test]
fn at_most_one_idle_texture_pool_slot_is_freed_per_acquire() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let pool = gpu.texture_pool();
    pool.set_idle_window_for_a_test(Duration::ZERO);
    let idle_shape = a_16_high_rgba8_texture_of_width(16);
    let held: Vec<PooledTextureHandle> = (0..3)
        .map(|_| pool.acquire(&idle_shape, None).expect("a texture"))
        .collect();
    // Each handle's own release sees a reader still on its texture, so no
    // slot is freed while the three are let go.
    let readers: Vec<Texture> = held
        .iter()
        .map(PooledTextureHandle::texture_clone)
        .collect();
    drop(held);
    drop(readers);
    assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 3);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let _first_other_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture");
    assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 2);
    let _second_other_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(48), None)
        .expect("a texture");
    assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 1);
}

#[test]
fn a_texture_pool_slot_a_reader_still_holds_is_never_freed() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let pool = gpu.texture_pool();
    pool.set_idle_window_for_a_test(Duration::ZERO);
    let idle_shape = a_16_high_rgba8_texture_of_width(16);
    let handle = pool.acquire(&idle_shape, None).expect("a texture");
    let reader = handle.texture_clone();
    drop(handle);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let _another_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture");
    assert_eq!(
        pool.slot_count_of_the_shape_of(&idle_shape),
        1,
        "a slot was freed under a reader of its texture"
    );
    drop(reader);
    let _a_third_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(48), None)
        .expect("a texture");
    assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 0);
}

#[test]
fn a_texture_pool_slot_a_helper_holds_checked_out_is_never_freed() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let check_out_leases = Arc::new(SurfaceCheckOutLeaseRegistry::new());
    gpu.install_the_engines_surface_store(SurfaceStore::new_reading_check_out_leases(
        "the-texture-pool-reads-this-lease-table-in-process".to_string(),
        "texture-pool-freeing-test-runtime".to_string(),
        Arc::clone(&check_out_leases),
    ))
    .expect("the engine's surface store installs once");
    let pool = gpu.texture_pool();
    pool.set_idle_window_for_a_test(Duration::ZERO);
    let checked_out_shape = a_16_high_rgba8_texture_of_width(16);
    let handle = pool.acquire(&checked_out_shape, None).expect("a texture");
    handle.note_a_surface_share_id_naming_its_pool_slot("texture-checked-out-by-a-helper");
    let helper = check_out_leases.mint_holder_id();
    check_out_leases
        .record_check_out_lease("texture-checked-out-by-a-helper", helper)
        .expect("the lease records");
    drop(handle);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let _another_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture");
    assert_eq!(
        pool.slot_count_of_the_shape_of(&checked_out_shape),
        1,
        "a slot a helper holds checked out was freed"
    );
    check_out_leases
        .release_one_check_out_lease("texture-checked-out-by-a-helper", helper)
        .expect("the lease releases");
    let _a_third_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(48), None)
        .expect("a texture");
    assert_eq!(pool.slot_count_of_the_shape_of(&checked_out_shape), 0);
}

#[test]
fn a_texture_pool_slot_flagged_freed_as_soon_as_unheld_skips_the_idle_window() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let pool = gpu.texture_pool();
    let owner_gone_shape = a_16_high_rgba8_texture_of_width(16);
    let handle = pool.acquire(&owner_gone_shape, None).expect("a texture");
    handle.flag_its_pool_slot_freed_as_soon_as_unheld();
    drop(handle);
    wait_for_every_gpu_submission_made_so_far(&gpu);

    let _another_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture");
    assert_eq!(
        pool.slot_count_of_the_shape_of(&owner_gone_shape),
        0,
        "a flagged slot waited out the idle window"
    );
}

#[test]
fn a_stopped_streams_unheld_texture_pool_slots_are_freed_and_its_held_ones_once_let_go() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let stopping_stream = LoadedStreamTag::numbered_for_a_test(1);
    let running_stream = LoadedStreamTag::numbered_for_a_test(2);
    let pool = gpu.texture_pool();
    let shape = a_16_high_rgba8_texture_of_width(16);
    let unheld_of_the_stopping_stream = pool
        .acquire(&shape, Some(stopping_stream))
        .expect("a texture");
    let still_held_by_the_stopping_stream = pool
        .acquire(&shape, Some(stopping_stream))
        .expect("a texture");
    let unheld_of_the_running_stream = pool
        .acquire(&shape, Some(running_stream))
        .expect("a texture");
    drop(unheld_of_the_stopping_stream);
    drop(unheld_of_the_running_stream);
    assert_eq!(pool.slot_count_of_the_shape_of(&shape), 3);

    assert_eq!(
        free_what_a_stopped_stream_last_acquired(pool, stopping_stream),
        1
    );
    assert_eq!(pool.slot_count_of_the_shape_of(&shape), 2);

    drop(still_held_by_the_stopping_stream);
    wait_for_every_gpu_submission_made_so_far(&gpu);
    let _another_shape = pool
        .acquire(&a_16_high_rgba8_texture_of_width(32), None)
        .expect("a texture");
    assert_eq!(
        pool.slot_count_of_the_shape_of(&shape),
        1,
        "the running stream's idle slot was freed, or the stopped stream's held one was not \
             once let go"
    );
}

#[test]
fn a_texture_pool_slot_handed_out_again_belongs_to_the_stream_that_acquired_it_last() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let first_stream = LoadedStreamTag::numbered_for_a_test(1);
    let second_stream = LoadedStreamTag::numbered_for_a_test(2);
    let pool = gpu.texture_pool();
    let shape = a_16_high_rgba8_texture_of_width(16);
    let first = pool.acquire(&shape, Some(first_stream)).expect("a texture");
    let slot_id = first.slot_id();
    drop(first);
    let second = pool
        .acquire(&shape, Some(second_stream))
        .expect("a texture");
    assert_eq!(second.slot_id(), slot_id);
    drop(second);

    assert_eq!(
        free_what_a_stopped_stream_last_acquired(pool, first_stream),
        0
    );
    assert_eq!(
        free_what_a_stopped_stream_last_acquired(pool, second_stream),
        1
    );
    assert_eq!(pool.slot_count_of_the_shape_of(&shape), 0);
}
