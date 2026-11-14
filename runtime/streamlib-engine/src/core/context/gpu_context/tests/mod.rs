// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use super::kernels::compute_kernel_cache_key;
use super::pixel_buffer_pool_manager::POOL_PRE_ALLOCATE_COUNT;
use super::*;

#[cfg(target_os = "linux")]
use crate::core::test_support::wait_for_every_gpu_submission_made_so_far;

mod access_tiers_and_escalation;
mod cross_process_leases;
mod freed_pool_entries;
mod native_node_output_pools;
mod texture_cache_and_pixel_buffer_registration;

#[cfg(target_os = "linux")]
const LEASE_TEST_SURFACE_WIDTH: u32 = 32;

#[cfg(target_os = "linux")]
const LEASE_TEST_SURFACE_HEIGHT: u32 = 32;

/// A context whose pool can see cross-process holders.
///
/// The store is deliberately never connected: the pool reads the lease
/// table in this address space, and the socket exists only to carry the
/// checkouts that write it. Registration warns and moves on, which is the
/// same path a store whose service died already takes.
#[cfg(target_os = "linux")]
fn gpu_context_reading_check_out_leases_or_skip() -> Option<(
    GpuContext,
    Arc<crate::core::context::SurfaceCheckOutLeaseRegistry>,
)> {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        println!("Skipping - no GPU device available");
        return None;
    };
    let check_out_leases = Arc::new(crate::core::context::SurfaceCheckOutLeaseRegistry::new());
    gpu.install_the_engines_surface_store(SurfaceStore::new_reading_check_out_leases(
        "the-pool-reads-this-lease-table-in-process".to_string(),
        "surface-check-out-lease-test-runtime".to_string(),
        Arc::clone(&check_out_leases),
    ))
    .expect("the engine's surface store installs once");
    Some((gpu, check_out_leases))
}

#[cfg(target_os = "linux")]
fn acquire_one_pool_slot_id(gpu: &GpuContext) -> Result<String> {
    // The handle is dropped here on purpose: from this point the slot is
    // free as far as the in-process refcount is concerned, so anything
    // that keeps the pool off it is a lease and nothing else.
    gpu.acquire_pixel_buffer(
        LEASE_TEST_SURFACE_WIDTH,
        LEASE_TEST_SURFACE_HEIGHT,
        PixelFormat::Rgba32,
    )
    .map(|(published_frame_id, _returned_to_the_pool_immediately)| published_frame_id.to_string())
}

/// Slot-key equality: ids of *different frames* of one slot must count
/// as the same slot, or a rehand would hide behind a fresh generation.
#[cfg(target_os = "linux")]
fn same_pool_slot(one_published_id: &str, another_published_id: &str) -> bool {
    pool_slot_key_of_surface_id(one_published_id)
        == pool_slot_key_of_surface_id(another_published_id)
}

/// The ids of the pool's pre-allocated slots, learned by cycling the ring
/// once.
#[cfg(target_os = "linux")]
fn pool_slot_ids_in_ring_order(gpu: &GpuContext) -> Vec<String> {
    (0..POOL_PRE_ALLOCATE_COUNT)
        .map(|_| {
            acquire_one_pool_slot_id(gpu).expect("the pool hands out a slot when none is held")
        })
        .collect()
}

#[cfg(target_os = "linux")]
const LEASE_TEST_SURFACE_SHAPE: PixelBufferPoolKey = PixelBufferPoolKey {
    width: LEASE_TEST_SURFACE_WIDTH,
    height: LEASE_TEST_SURFACE_HEIGHT,
    format: PixelFormat::Rgba32,
};

/// The slots of a shape one native node borrows from the shared ring while
/// nothing holds its frames: one per frame of its rotation.
#[cfg(target_os = "linux")]
const SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD: usize =
    crate::core::context::processor_output_pixel_buffer_pool_of_one_native_node::NATIVE_NODE_OUTPUT_PIXEL_BUFFER_POOL_ROTATION_DEPTH;

/// The pre-allocated slots a shape's shared ring keeps once one native
/// node has borrowed a slot per frame of its rotation.
#[cfg(target_os = "linux")]
const PRE_ALLOCATED_SLOTS_THE_SHARED_RING_KEEPS_PAST_ONE_NATIVE_NODES_ROTATION: usize =
    POOL_PRE_ALLOCATE_COUNT.saturating_sub(SLOTS_ONE_NATIVE_NODE_BORROWS_WITH_NOTHING_HELD);

/// The next frame `native_node_output_pool` publishes, its buffer handed
/// back at once so only a lease can keep the slot.
#[cfg(target_os = "linux")]
fn acquire_one_native_node_frame_id(
    native_node_output_pool: &mut crate::core::context::ProcessorOutputPixelBufferPoolOfOneNativeNode,
) -> Result<String> {
    native_node_output_pool
        .acquire_the_next_output_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .map(|frame| frame.published_frame_id.to_string())
}

/// Each slot of `shape`'s shared ring, in ring order, with the generation
/// it last published.
#[cfg(target_os = "linux")]
fn shared_ring_slots_and_generations(
    gpu: &GpuContext,
    shape: PixelBufferPoolKey,
) -> Vec<(String, u64)> {
    let pools = gpu.pixel_buffer_pool_manager.lock_the_shared_rings();
    pools.get(&shape).map_or_else(Vec::new, |ring_pool| {
        (0..ring_pool.ring.slot_count())
            .map(|slot_index| {
                let slot = ring_pool.ring.slot(slot_index);
                (
                    slot.pool_slot_key().to_string(),
                    slot.published_frame_generation(),
                )
            })
            .collect()
    })
}

/// The generation `published_frame_id` names.
#[cfg(target_os = "linux")]
fn frame_generation_of(published_frame_id: &str) -> u64 {
    crate::core::rhi::split_pool_slot_and_frame_generation(published_frame_id)
        .expect("a published frame id carries its generation")
        .1
}

/// Lease every frame `native_node_output_pool` may hold, to `holder`.
#[cfg(target_os = "linux")]
fn lease_every_frame_a_native_node_may_hold(
    native_node_output_pool: &mut crate::core::context::ProcessorOutputPixelBufferPoolOfOneNativeNode,
    check_out_leases: &crate::core::context::SurfaceCheckOutLeaseRegistry,
    holder: crate::core::context::SurfaceCheckOutLeaseHolderId,
) -> Vec<String> {
    (0..crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY)
        .map(|_| {
            let frame = acquire_one_native_node_frame_id(native_node_output_pool)
                .expect("a node under its cap is served");
            check_out_leases
                .record_check_out_lease(&frame, holder)
                .expect("a consumer holds the node's frame");
            frame
        })
        .collect()
}

/// The device memory this context's allocator holds across every heap,
/// read from the driver now.
#[cfg(target_os = "linux")]
fn device_memory_bytes_the_allocator_holds_on_every_heap(gpu: &GpuContext) -> u64 {
    gpu.device()
        .inner
        .read_every_heaps_device_memory_budget_fetched_from_the_driver_now()
        .expect("every heap's budget reads")
        .iter()
        .map(|heap| heap.bytes_this_runtimes_allocator_pools_hold_on_the_heap)
        .sum()
}

/// Whether `published_frame_id` is refused as a recycled frame, in this
/// process and by the lease table alike.
#[cfg(target_os = "linux")]
fn is_refused_as_recycled_everywhere(
    gpu: &GpuContext,
    check_out_leases: &crate::core::context::SurfaceCheckOutLeaseRegistry,
    published_frame_id: &str,
) -> bool {
    let refused_in_this_process = matches!(
        gpu.refuse_a_retired_frame_id(published_frame_id),
        Err(Error::SurfaceFrameRecycled { .. })
    );
    let refused_by_the_lease_table = matches!(
        check_out_leases
            .record_check_out_lease(published_frame_id, check_out_leases.mint_holder_id()),
        Err(Error::SurfaceFrameRecycled { .. })
    );
    refused_in_this_process
        && refused_by_the_lease_table
        && gpu
            .resolve_pixel_buffer_by_surface_id(published_frame_id)
            .is_err()
}
