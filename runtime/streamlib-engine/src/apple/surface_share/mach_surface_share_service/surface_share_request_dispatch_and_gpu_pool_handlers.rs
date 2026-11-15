// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Routing a connected peer's request to its answer, and handing out GPU pool claim tables and pool slots.

use super::surface_share_request_decoding_and_answers::{
    answer_signal_consume_done, answer_timeline_import_refused, handle_check_in, handle_check_out,
    handle_lookup, handle_register, handle_update_layout,
};
use super::*;

/// Answer one request on a connected peer's connection: the verbs that hand
/// over a pool's claim table or slots, and the pool slots retired since the
/// connection was last answered, besides every connection-free verb.
pub(super) fn answer_a_connected_peers_request(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
    peer: &mut ConnectedSurfaceSharePeer,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let op = request
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let (mut response, reply_ports) = match op {
        SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP => {
            match peer.claimant_handed_columns_on_this_connection() {
                Ok(claimant) => handle_claim_table_of_gpu_pool(state, request, claimant),
                Err(refusal) => (serde_json::json!({ "error": refusal }), Vec::new()),
            }
        }
        SURFACE_SHARE_IMPORT_POOL_SLOT_OP => {
            handle_import_pool_slot(state, request, &mut peer.pool_slot_keys_handed_an_iosurface)
        }
        _ => {
            let (response, reply_ports) =
                answer_surface_share_request(state, request, received_ports, peer.lease_holder);
            if op == "check_out"
                && !reply_ports.is_empty()
                && let Some((pool_slot_key, _)) =
                    requested_surface_id(request).and_then(split_pool_slot_and_frame_generation)
            {
                peer.pool_slot_keys_handed_an_iosurface
                    .remember(pool_slot_key);
            }
            (response, reply_ports)
        }
    };
    peer.pool_slot_keys_handed_an_iosurface
        .list_the_retired_ones_on_the_reply(state, &mut response);
    (response, reply_ports)
}

/// Hand a peer the claim table a `GpuPoolSlotFrame` descriptor names: a send
/// right to the table's Mach memory entry, the column the peer's process
/// claims in, and the pool slot key leased to each row. A peer past the
/// table's last column is refused.
fn handle_claim_table_of_gpu_pool(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    claimant: GpuPoolClaimantProcessId,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let Some(pool_uuid) = request
        .get(SURFACE_SHARE_GPU_POOL_FIELD)
        .and_then(serde_json::Value::as_str)
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
    let memory_entry = match table.shared_rows().memory_entry_send_right() {
        Ok(memory_entry) => memory_entry,
        Err(unminted) => {
            return (
                serde_json::json!({
                    "error": format!("the claim table's memory entry minted no send right: {unminted}")
                }),
                Vec::new(),
            );
        }
    };
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
        vec![memory_entry],
    )
}

/// Hand a peer one pool slot's IOSurface port, and its timeline pair when it
/// has one, under its pool slot key and with no lease: what keeps the slot
/// from reuse is the peer's claim in the pool's claim table, per frame.
fn handle_import_pool_slot(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    pool_slot_keys_handed_an_iosurface: &mut PoolSlotKeysHandedAnIOSurfaceOnThisConnection,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let Some(pool_slot_key) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if split_pool_slot_and_frame_generation(pool_slot_key).is_some() {
        return (
            serde_json::json!({"error": "import_pool_slot names a pool slot key, never a \
                                published frame id"}),
            Vec::new(),
        );
    }
    let (response, reply_ports) = handle_lookup(state, request);
    if response.get("error").is_none() {
        pool_slot_keys_handed_an_iosurface.remember(pool_slot_key);
    }
    (response, reply_ports)
}

/// Answer one request, returning the reply and the ports it carries.
/// `received_ports` a verb does not consume are released on return.
pub(super) fn answer_surface_share_request(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let op = request
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match op {
        "register" => (handle_register(state, request, received_ports), Vec::new()),
        "check_in" => (handle_check_in(state, request, received_ports), Vec::new()),
        // `lookup` and `check_out` answer with the same surface; only
        // `check_out` claims the frame.
        "lookup" => handle_lookup(state, request),
        "check_out" => handle_check_out(state, request, lease_holder),
        "release_check_out" => (
            answer_release_check_out(state.check_out_leases(), request, lease_holder),
            Vec::new(),
        ),
        "unregister" | "release" => (answer_unregister(state, request), Vec::new()),
        "update_layout" => (
            handle_update_layout(state, request, lease_holder),
            Vec::new(),
        ),
        SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE => {
            (answer_signal_consume_done(state, request), Vec::new())
        }
        SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED => {
            (answer_timeline_import_refused(state, request), Vec::new())
        }
        _ => (
            serde_json::json!({"error": format!("unknown operation: {op}")}),
            Vec::new(),
        ),
    }
}
