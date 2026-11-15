// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Checking a surface out, releasing a check-out, and looking a surface up.

use super::*;

/// `check_out` is `lookup` plus a claim: the surface is pinned against
/// producer reuse until this connection releases it or drops.
///
/// A peer that states it holds its own import of a published frame's pool
/// slot gets the frame's metadata and no handle. Either way the retired-frame
/// refusal answers first, and a lease that cannot be recorded refuses the
/// checkout outright: handing a consumer memory the pool believes is free is
/// exactly the silent wrongness the lease exists to remove.
pub(in crate::linux::surface_share::unix_socket_service) fn handle_check_out(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
    pool_slot_keys_handed_memory: &mut PoolSlotKeysHandedMemoryOnThisConnection,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), surface_id) {
        return (refusal, Vec::new());
    }
    let Some(checkout) = state.get_surface_planes(surface_id) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    let published_pool_slot = pool_slot_of_a_published_frame_id(surface_id);
    let peer_holds_the_pool_slot_import = published_pool_slot.is_some()
        && request
            .get(SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

    let reply_fds = if peer_holds_the_pool_slot_import {
        Vec::new()
    } else {
        match duplicate_the_handles_of_a_check_out(&checkout) {
            Ok(duplicated_fds) => duplicated_fds,
            Err(refusal) => return (refusal, Vec::new()),
        }
    };

    if let Err(refusal) =
        record_check_out_lease_or_refusal(state.check_out_leases(), surface_id, lease_holder)
    {
        for fd in &reply_fds {
            unsafe { libc::close(*fd) };
        }
        return (refusal, Vec::new());
    }

    if !peer_holds_the_pool_slot_import && let Some(pool_slot) = published_pool_slot {
        pool_slot_keys_handed_memory.remember(pool_slot);
    }
    (
        surface_share_check_out_metadata(state, surface_id, checkout),
        reply_fds,
    )
}

pub(in crate::linux::surface_share::unix_socket_service) fn handle_release_check_out(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<RawFd>) {
    (
        answer_release_check_out(state.check_out_leases(), request, lease_holder),
        Vec::new(),
    )
}

pub(in crate::linux::surface_share::unix_socket_service) fn handle_lookup(
    state: &SurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<RawFd>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), surface_id) {
        return (refusal, Vec::new());
    }
    let Some(checkout) = state.get_surface_planes(surface_id) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    match duplicate_the_handles_of_a_check_out(&checkout) {
        Ok(duplicated_fds) => (
            surface_share_check_out_metadata(state, surface_id, checkout),
            duplicated_fds,
        ),
        Err(refusal) => (refusal, Vec::new()),
    }
}
