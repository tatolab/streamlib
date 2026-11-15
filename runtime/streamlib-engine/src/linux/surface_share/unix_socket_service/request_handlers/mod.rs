// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface-share service's request handlers, split by request kind, and the parsing and check-out helpers they share.

use super::*;

mod check_out_release_and_lookup_handlers;
mod gpu_pool_claim_table_pool_slot_and_viewed_frame_handlers;
mod surface_registration_handlers;

pub(super) use check_out_release_and_lookup_handlers::{
    handle_check_out, handle_lookup, handle_release_check_out,
};
pub(super) use gpu_pool_claim_table_pool_slot_and_viewed_frame_handlers::{
    handle_claim_table_of_gpu_pool, handle_import_pool_slot, handle_import_viewed_frame,
};
pub(super) use surface_registration_handlers::{
    handle_check_in, handle_register, handle_unregister, handle_update_layout,
};

/// The raw-handle export contract an OPAQUE_FD producer states beside its
/// fd, as a registration or check-in request carries it.
struct RawHandleExportContractFields {
    vk_memory_type_index: Option<u32>,
    exporting_device_uuid: Option<String>,
}

/// Read the raw-handle export contract fields from `request`. No default
/// exists: `0` is a valid memory type index, so absence stays absent through
/// the table and the checkout echo.
fn parse_raw_handle_export_contract_fields(
    request: &serde_json::Value,
) -> RawHandleExportContractFields {
    RawHandleExportContractFields {
        vk_memory_type_index: stated_vk_memory_type_index(request),
        exporting_device_uuid: request
            .get(SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID)
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// Extract `plane_sizes`, `plane_offsets`, and `plane_strides` from a JSON
/// request body. Returns vecs whose length matches `expected_plane_count`,
/// falling back to `[0]` for single-plane registrations that omit the arrays.
///
/// Returning `None` is an explicit wire-protocol violation (mismatched
/// arrays, negative values); the handler should error out instead of
/// guessing.
fn parse_plane_arrays(
    request: &serde_json::Value,
    expected_plane_count: usize,
) -> Option<(Vec<u64>, Vec<u64>, Vec<u64>)> {
    let parse_arr = |key: &str| -> Option<Option<Vec<u64>>> {
        match request.get(key) {
            None => Some(None),
            Some(v) => v
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|el| el.as_u64())
                        .collect::<Option<Vec<u64>>>()
                })
                .map(Some)
                .unwrap_or(Some(None)),
        }
    };

    let resolve = |opt: Option<Vec<u64>>| -> Option<Vec<u64>> {
        match opt {
            Some(v) if v.len() == expected_plane_count => Some(v),
            Some(_) => None,
            None if expected_plane_count <= 1 => Some(vec![0u64; expected_plane_count.max(1)]),
            None => None,
        }
    };

    let sizes = resolve(parse_arr("plane_sizes")?)?;
    let offsets = resolve(parse_arr("plane_offsets")?)?;
    let strides = resolve(parse_arr("plane_strides")?)?;
    Some((sizes, offsets, strides))
}

/// The pool slot keys one connection was handed memory for, so the
/// connection can tell its peer when the engine unregisters one.
#[derive(Default)]
pub(super) struct PoolSlotKeysHandedMemoryOnThisConnection {
    pool_slot_keys: HashSet<String>,
}

impl PoolSlotKeysHandedMemoryOnThisConnection {
    fn remember(&mut self, pool_slot_key: &str) {
        if !self.pool_slot_keys.contains(pool_slot_key) {
            self.pool_slot_keys.insert(pool_slot_key.to_string());
        }
    }

    /// Forget every remembered key `state` no longer registers and list them
    /// on `reply` under `retired_pool_slots`. Keys are never reused, so a key
    /// absent from the table is retired for good.
    pub(super) fn list_the_retired_ones_on_the_reply(
        &mut self,
        state: &SurfaceShareState,
        reply: &mut serde_json::Value,
    ) {
        let mut retired_pool_slots: Vec<String> = Vec::new();
        self.pool_slot_keys.retain(|pool_slot_key| {
            let still_registered = state.is_registered(pool_slot_key);
            if !still_registered {
                retired_pool_slots.push(pool_slot_key.clone());
            }
            still_registered
        });
        if retired_pool_slots.is_empty() {
            return;
        }
        if let Some(reply_fields) = reply.as_object_mut() {
            reply_fields.insert(
                SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD.to_string(),
                retired_pool_slots.into(),
            );
        }
    }
}

/// The pool slot a published frame id (`<slot>#<generation>`) names; `None`
/// for every other surface id.
fn pool_slot_of_a_published_frame_id(surface_id: &str) -> Option<&str> {
    split_pool_slot_and_frame_generation(surface_id).map(|(pool_slot, _)| pool_slot)
}

/// Dup every plane fd, then `produce_done` and `consume_done` when the
/// registration carries them, in that order — the order a peer peels the
/// trailing timeline fds off in. The dups are independent of the table's own
/// copies; on a partial failure every dup already taken is closed.
fn duplicate_the_handles_of_a_check_out(
    checkout: &SurfacePlaneCheckout,
) -> Result<Vec<RawFd>, serde_json::Value> {
    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(checkout.dma_buf_fds.len() + 2);
    let handles = checkout
        .dma_buf_fds
        .iter()
        .map(|fd| (*fd, "DMA-BUF"))
        .chain(checkout.produce_done_fd.map(|fd| (fd, "produce_done")))
        .chain(checkout.consume_done_fd.map(|fd| (fd, "consume_done")));
    for (source_fd, handle_name) in handles {
        let dup = unsafe { libc::fcntl(source_fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return Err(serde_json::json!({"error": format!("failed to dup {handle_name} FD")}));
        }
        dup_fds.push(dup);
    }
    Ok(dup_fds)
}

/// The registration metadata a `lookup` or `check_out` answers with,
/// `current_image_layout` included. `has_produce_done_fd` and
/// `has_consume_done_fd` state what the registration carries; the fds trail
/// the planes only on a reply that sends handles.
fn surface_share_check_out_metadata(
    state: &SurfaceShareState,
    surface_id: &str,
    checkout: SurfacePlaneCheckout,
) -> serde_json::Value {
    let surfaces = state.get_surfaces();
    let pool_slot_key = crate::core::rhi::pool_slot_key_of_surface_id(surface_id);
    let metadata = surfaces.iter().find(|s| s.surface_id == pool_slot_key);
    let (width, height, format, resource_type) = match metadata {
        Some(m) => (
            m.width,
            m.height,
            m.format.as_str(),
            m.resource_type.as_str(),
        ),
        None => (0, 0, "unknown", "pixel_buffer"),
    };
    let mut response = serde_json::json!({
        "surface_id": surface_id,
        "width": width,
        "height": height,
        "format": format,
        "resource_type": resource_type,
        "handle_type": checkout.handle_type,
        "plane_sizes": checkout.plane_sizes,
        "plane_offsets": checkout.plane_offsets,
        "plane_strides": checkout.plane_strides,
        "drm_format_modifier": checkout.drm_format_modifier,
        "has_produce_done_fd": checkout.produce_done_fd.is_some(),
        "has_consume_done_fd": checkout.consume_done_fd.is_some(),
        "current_image_layout": checkout.current_image_layout,
        "vk_image_type": checkout.vk_image_type,
        "vk_image_mip_levels": checkout.vk_image_mip_levels,
        "vk_image_array_layers": checkout.vk_image_array_layers,
        "vk_image_samples": checkout.vk_image_samples,
        "vk_image_tiling": checkout.vk_image_tiling,
        "vk_image_usage": checkout.vk_image_usage,
        "vk_image_allocation_size": checkout.vk_image_allocation_size,
    });
    // Echoed only when registered: absence is meaningful (see
    // `SurfaceMetadata::vk_memory_type_index`), so no default may appear
    // on the wire.
    if let Some(vk_memory_type_index) = checkout.vk_memory_type_index {
        response[SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX] = vk_memory_type_index.into();
    }
    if let Some(exporting_device_uuid) = checkout.exporting_device_uuid {
        response[SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID] = exporting_device_uuid.into();
    }
    echo_the_tensor_layout_onto_a_lookup_reply(&mut response, checkout.tensor_layout.as_ref());
    if let Some(pixel_planes_within_one_allocation) = &checkout.pixel_planes_within_one_allocation {
        pixel_planes_within_one_allocation.write_surface_share_fields(&mut response);
    }
    response
}
