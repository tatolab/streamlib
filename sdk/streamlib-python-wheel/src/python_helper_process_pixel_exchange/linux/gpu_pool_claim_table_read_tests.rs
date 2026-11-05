// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use streamlib::sdk::context::{
    GpuPoolClaimTableRowLease, GpuPoolClaimTablesOfOnePool, GpuPoolSlotRehandTest, SurfaceStore,
};
use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

const PIXEL_BUFFER_WIDTH: u32 = 64;
const PIXEL_BUFFER_HEIGHT: u32 = 16;
const PIXEL_BUFFER_BYTE_SIZE: u64 = 64 * 4 * 16;
const POOL_SLOT_KEY: &str = "claim-table-read-slot";

/// One engine-exported OPAQUE_FD pixel buffer filled with `stamp`; `None`
/// where the device exports none.
fn a_stamped_pool_slot(host_device: &Arc<HostVulkanDevice>, stamp: u8) -> Option<PixelBuffer> {
    let exported = match HostVulkanBuffer::new_opaque_fd_export(host_device, PIXEL_BUFFER_BYTE_SIZE)
    {
        Ok(exported) => exported,
        Err(unavailable) => {
            tracing::warn!("skipping: no OPAQUE_FD buffer export on this device ({unavailable})");
            return None;
        }
    };
    // SAFETY: the export is a live mapping of PIXEL_BUFFER_BYTE_SIZE bytes.
    unsafe {
        std::ptr::write_bytes(
            exported.mapped_ptr(),
            stamp,
            PIXEL_BUFFER_BYTE_SIZE as usize,
        )
    };
    Some(PixelBuffer::from_host_vulkan_buffer(
        Arc::new(exported),
        PIXEL_BUFFER_WIDTH,
        PIXEL_BUFFER_HEIGHT,
        4,
        PixelFormat::Bgra32,
    ))
}

fn the_frame_of(
    row_lease: &GpuPoolClaimTableRowLease,
    generation: u64,
) -> GpuPoolSlotFrameDescriptorBody {
    GpuPoolSlotFrameDescriptorBody {
        pool: row_lease.table().pool_uuid(),
        slot: row_lease.row_index(),
        generation,
        timeline_value: 0,
        image_layout: 0,
    }
}

/// Publish `generation` the way a pool's hand-off does: to the slot's row and
/// to the lease table.
fn publish(share: &SurfaceShareUnderTest, row_lease: &GpuPoolClaimTableRowLease, generation: u64) {
    row_lease.row().publish_generation(generation);
    share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, generation);
}

/// After one hand-over and one slot import, a pooled frame is claimed and read
/// with no surface-share request at all: the claim lives in the shared table,
/// holds the slot against a rehand while it lives, takes no checkout lease,
/// and a frame the slot has moved past is refused as recycled. Needs a Vulkan
/// device; skips where none comes up.
#[test]
fn a_pooled_frame_read_after_its_slot_set_crossed_makes_no_surface_share_request() {
    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let Some(pool_slot) = a_stamped_pool_slot(&host_device, 0x5A) else {
        return;
    };
    let share = SurfaceShareUnderTest::start("claim-table-read");
    let engine_surface_store = SurfaceStore::new(
        share.socket_path.to_string_lossy().into_owned(),
        "engine:claim-table-read-under-test".to_string(),
    );
    engine_surface_store
        .connect()
        .expect("the engine's store connects");
    engine_surface_store
        .register_buffer(POOL_SLOT_KEY, &pool_slot)
        .expect("the engine registers the pool slot");
    let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("claim-table-read");
    let row_lease = claim_tables_of_the_pool
        .lease_a_row_for_a_slot(
            share.check_out_leases().gpu_pool_claim_tables(),
            POOL_SLOT_KEY,
        )
        .expect("the pool leases its slot a row");
    let exchange_client = share.exchange_client_labelled_importing_on(
        "claim-table-read-under-test",
        Ok(host_device.physical_device_uuid()),
    );

    publish(&share, &row_lease, 1);
    exchange_client
        .note_the_pooled_gpu_frame_a_read_carries(the_frame_of(&row_lease, 1))
        .expect("the first frame fetches the pool's claim table");
    let first_frame = exchange_client
        .check_out_and_import(&format!("{POOL_SLOT_KEY}#1"))
        .expect("the first frame imports its slot once");
    let HelperCheckedOutSurface::PixelBuffer(first_pixels) = &first_frame else {
        panic!("a pixel_buffer registration reads as a pixel buffer");
    };
    let slot_import = Arc::clone(&first_pixels.pixel_buffer_pool_slot_import);
    drop(first_frame);
    assert!(!row_lease.row().is_claimed());

    // Every later frame must make no request: a closed connection and a
    // removed socket fail any request this helper tries.
    drop(exchange_client.surface_share_connection.lock().take());
    std::fs::remove_file(&share.socket_path).expect("the socket is removed");

    for generation in 2..=4 {
        publish(&share, &row_lease, generation);
        exchange_client
            .note_the_pooled_gpu_frame_a_read_carries(the_frame_of(&row_lease, generation))
            .expect("a known slot's frame needs no hand-over");
        let frame_id = format!("{POOL_SLOT_KEY}#{generation}");
        let claimed = exchange_client
            .claim_surface_against_producer_reuse(&frame_id)
            .unwrap_or_else(|refusal| panic!("frame {frame_id} is claimed: {refusal}"));
        let read = exchange_client
            .check_out_and_import(&frame_id)
            .unwrap_or_else(|refusal| panic!("frame {frame_id} is read: {refusal}"));
        let HelperCheckedOutSurface::PixelBuffer(pixels) = &read else {
            panic!("a pixel_buffer registration reads as a pixel buffer");
        };
        assert!(Arc::ptr_eq(
            &slot_import,
            &pixels.pixel_buffer_pool_slot_import
        ));
        let base_address = pixels.host_mapped_base_address();
        // SAFETY: the mapping spans the slot's bytes while `read` lives.
        let bytes =
            unsafe { std::slice::from_raw_parts(base_address, PIXEL_BUFFER_BYTE_SIZE as usize) };
        assert!(bytes.iter().all(|byte| *byte == 0x5A));
        assert_eq!(
            share
                .check_out_leases()
                .outstanding_check_out_count(&frame_id)
                .expect("the lease table stays readable"),
            0,
            "a frame claimed in its table takes no checkout lease"
        );
        assert_eq!(
            row_lease.row().retire_for_a_rehand(generation),
            GpuPoolSlotRehandTest::HeldByAClaim,
            "a claimed frame's slot is never rehanded under its reader"
        );
        drop(claimed);
        assert!(
            row_lease.row().is_claimed(),
            "the read still holds the frame"
        );
        drop(read);
        assert!(!row_lease.row().is_claimed());
    }

    publish(&share, &row_lease, 5);
    let recycled = exchange_client
        .claim_surface_against_producer_reuse(&format!("{POOL_SLOT_KEY}#4"))
        .err()
        .expect("a frame its slot moved past is refused");
    assert!(recycled.to_string().contains("recycled"), "{recycled}");
    assert!(!row_lease.row().is_claimed());
}
