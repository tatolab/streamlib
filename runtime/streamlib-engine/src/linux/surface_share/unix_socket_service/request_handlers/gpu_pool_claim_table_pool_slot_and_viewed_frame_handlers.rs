// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Handing a claimant its GPU pool claim table, importing a pool slot, and importing the frame a viewer reads.

use super::*;

/// Hand a peer the claim table a `GpuPoolSlotFrame` descriptor names: the
/// table's memfd, the column the peer's process claims in, and the pool slot
/// key leased to each row. A peer past the table's last column is refused.
pub(in crate::linux::surface_share::unix_socket_service) fn handle_claim_table_of_gpu_pool(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    claimant: GpuPoolClaimantProcessId,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(pool_uuid) = request
        .get(SURFACE_SHARE_GPU_POOL_FIELD)
        .and_then(|value| value.as_str())
        .and_then(gpu_pool_uuid_of_its_wire_name)
    else {
        return (
            serde_json::json!({"error": "missing or malformed pool"}),
            Vec::new(),
        );
    };
    let Some(table) = state
        .check_out_leases()
        .gpu_pool_claim_tables()
        .table_named(&pool_uuid)
    else {
        return (
            serde_json::json!({"error": "no GPU pool holds that claim table"}),
            Vec::new(),
        );
    };
    let claimant_column = match table.claimant_column_of(claimant) {
        Ok(column) => column,
        Err(refusal) => {
            return (
                serde_json::json!({"error": refusal.to_string()}),
                Vec::new(),
            );
        }
    };
    // SAFETY: a valid memfd the table keeps open for its life.
    let duplicated_memfd = unsafe {
        libc::fcntl(
            table.shared_rows().memfd().as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            0,
        )
    };
    if duplicated_memfd < 0 {
        return (
            serde_json::json!({"error": "failed to dup the claim table memfd"}),
            Vec::new(),
        );
    }
    let pool_slots: Vec<serde_json::Value> = table
        .rows_leased_to_slots()
        .iter()
        .map(GpuPoolClaimTableRowLeasedToASlot::pool_slots_entry)
        .collect();
    (
        serde_json::json!({
            SURFACE_SHARE_GPU_POOL_FIELD: gpu_pool_uuid_wire_name(&pool_uuid),
            SURFACE_SHARE_CLAIMANT_COLUMN_FIELD: claimant_column.index(),
            SURFACE_SHARE_POOL_SLOTS_FIELD: pool_slots,
        }),
        vec![duplicated_memfd],
    )
}

/// Hand a peer one pool slot's memory under its pool slot key, with no lease:
/// what keeps the slot from reuse is the peer's claim in the pool's claim
/// table, per frame.
pub(in crate::linux::surface_share::unix_socket_service) fn handle_import_pool_slot(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    pool_slot_keys_handed_memory: &mut PoolSlotKeysHandedMemoryOnThisConnection,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(pool_slot_key) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if pool_slot_of_a_published_frame_id(pool_slot_key).is_some() {
        return (
            serde_json::json!({"error": "import_pool_slot names a pool slot key, never a \
                                published frame id"}),
            Vec::new(),
        );
    }
    let Some(checkout) = state.get_surface_planes(pool_slot_key) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    let reply_fds = match duplicate_the_handles_of_a_check_out(&checkout) {
        Ok(duplicated_fds) => duplicated_fds,
        Err(refusal) => return (refusal, Vec::new()),
    };
    pool_slot_keys_handed_memory.remember(pool_slot_key);
    (
        surface_share_check_out_metadata(state, pool_slot_key, checkout),
        reply_fds,
    )
}

/// Hand a viewer memory holding the published frame the request names, the
/// way the request says the viewer reads it, with no lease: the viewer's claim in the pool's claim table
/// keeps the frame's slot from reuse while the engine copies it. The engine
/// names the registration — the slot itself, or its GPU copy — and its
/// handles ride the reply unless the viewer lists that id as imported.
pub(in crate::linux::surface_share::unix_socket_service) fn handle_import_viewed_frame(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    pool_slot_keys_handed_memory: &mut PoolSlotKeysHandedMemoryOnThisConnection,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(frame_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if pool_slot_of_a_published_frame_id(frame_id).is_none() {
        return (
            serde_json::json!({"error": format!(
                "{SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP} names a published frame id \
                 (<pool slot>#<generation>), never {frame_id:?}"
            )}),
            Vec::new(),
        );
    }
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), frame_id) {
        return (refusal, Vec::new());
    }
    let requested_read_by = request
        .get(SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD)
        .and_then(serde_json::Value::as_str);
    let Some(read_by) = requested_read_by.and_then(ViewedFrameReadBy::from_wire_name) else {
        return (
            serde_json::json!({"error": format!(
                "{SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP} states {SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD} \
                 as {:?} or {:?}, not {requested_read_by:?}",
                ViewedFrameReadBy::DisplayImport.wire_name(),
                ViewedFrameReadBy::CpuRead.wire_name(),
            )}),
            Vec::new(),
        );
    };
    let Some(viewed_frame_source) = state.viewed_frame_source() else {
        return (
            serde_json::json!({"error": "the engine has no GPU context yet, so no frame can be \
                                copied for a viewer; one exists once a stream has started"}),
            Vec::new(),
        );
    };
    let layout_the_producer_left_the_frame_in = request
        .get(SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD)
        .and_then(serde_json::Value::as_i64)
        .and_then(|raw_layout| i32::try_from(raw_layout).ok())
        .filter(|raw_layout| *raw_layout != 0)
        .map(streamlib_consumer_rhi::VulkanLayout);
    let import_id = match viewed_frame_source.surface_share_id_a_viewer_reads_the_frame_through(
        frame_id,
        read_by,
        layout_the_producer_left_the_frame_in,
    ) {
        Ok(import_id) => import_id,
        Err(refusal) => {
            return (
                serde_json::json!({"error": refusal.to_string()}),
                Vec::new(),
            );
        }
    };
    let Some(checkout) = state.get_surface_planes(&import_id) else {
        return (
            serde_json::json!({"error": format!(
                "the engine named {import_id:?} for frame {frame_id:?}, which surface-share does \
                 not hold"
            )}),
            Vec::new(),
        );
    };
    let the_viewer_holds_the_import = request
        .get(SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD)
        .and_then(serde_json::Value::as_array)
        .is_some_and(|held| {
            held.iter()
                .any(|held_id| held_id.as_str() == Some(&import_id))
        });
    let reply_fds = if the_viewer_holds_the_import {
        Vec::new()
    } else {
        match duplicate_the_handles_of_a_check_out(&checkout) {
            Ok(duplicated_fds) => duplicated_fds,
            Err(refusal) => return (refusal, Vec::new()),
        }
    };
    pool_slot_keys_handed_memory.remember(&import_id);
    (
        surface_share_check_out_metadata(state, &import_id, checkout),
        reply_fds,
    )
}
