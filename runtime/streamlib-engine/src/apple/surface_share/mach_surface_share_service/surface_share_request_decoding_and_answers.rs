// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Decoding a registration out of a request, and answering register, check-in, lookup, layout update, timeline and check-out requests.

use super::*;

/// The registration a `register` or `check_in` describes, holding the one
/// IOSurface its first port names and, when the flags announce them, the
/// timeline pair's shared-event ports after it.
fn registration_of_request(
    request: &serde_json::Value,
    surface_id: String,
    received_ports: Vec<OwnedMachSendRight>,
) -> Result<IOSurfaceShareRegistration, String> {
    let announced = |flag: &str| {
        request
            .get(flag)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let announces_timeline_pair = match (
        announced(SURFACE_SHARE_HAS_PRODUCE_DONE_PORT),
        announced(SURFACE_SHARE_HAS_CONSUME_DONE_PORT),
    ) {
        (false, false) => false,
        (true, true) => true,
        _ => {
            return Err(
                "a registration carries both timeline ports or neither, never one".to_string(),
            );
        }
    };
    let mut received_ports = received_ports.into_iter();
    let Some(iosurface_port) = received_ports.next() else {
        return Err("a registration carries exactly one IOSurface port".to_string());
    };
    let timeline_send_rights = if announces_timeline_pair {
        let (Some(produce_done), Some(consume_done)) =
            (received_ports.next(), received_ports.next())
        else {
            return Err("the announced timeline ports did not arrive".to_string());
        };
        Some(Arc::new(SharedTimelineSendRights {
            produce_done,
            consume_done,
        }))
    } else {
        None
    };
    if received_ports.next().is_some() {
        return Err(
            "a registration carries exactly one IOSurface port, then the ports its flags announce"
                .to_string(),
        );
    }
    let iosurface = IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
        .ok_or_else(|| "the registered port names no IOSurface".to_string())?;
    let requested_u32 = |key: &str| {
        request
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    let requested_str = |key: &str, default: &str| {
        request
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    let resource_type = requested_str("resource_type", SURFACE_RESOURCE_TYPE_PIXEL_BUFFER);
    let tensor_layout = tensor_layout_of_a_storage_buffer_registration(request)?;
    if let Some(tensor_layout) = &tensor_layout
        && u64::try_from(iosurface.alloc_size())
            .is_ok_and(|alloc_byte_size| alloc_byte_size < tensor_layout.byte_size())
    {
        return Err(format!(
            "the registered IOSurface's {} bytes cannot hold a {tensor_layout} tensor",
            iosurface.alloc_size()
        ));
    }
    let texture_image = (resource_type == SURFACE_RESOURCE_TYPE_TEXTURE).then(|| {
        Arc::new(RegisteredTextureImage::new(
            parse_vk_image_create_info_fields(request),
            stated_current_image_layout(request).unwrap_or(0),
        ))
    });
    Ok(IOSurfaceShareRegistration {
        surface_id,
        runtime_id: requested_runtime_id(request).to_string(),
        width: requested_u32("width").unwrap_or(iosurface.width() as u32),
        height: requested_u32("height").unwrap_or(iosurface.height() as u32),
        format: requested_str("format", "unknown"),
        resource_type,
        iosurface: RetainedIOSurfaceSharedAcrossThreads::new(iosurface),
        timeline_send_rights,
        texture_image,
        tensor_layout,
    })
}

pub(super) fn handle_register(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    // Registrations are per-slot; the `#<generation>` suffix belongs to the
    // published frame ids minted over a slot.
    if crate::core::rhi::PublishedPixelBufferFrameId::parse(surface_id).is_some() {
        return serde_json::json!({"error": format!(
            "surface id '{surface_id}' ends in the reserved #<generation> suffix; \
             register the pool slot and let acquisition publish the frame ids"
        )});
    }
    let registration =
        match registration_of_request(request, surface_id.to_string(), received_ports) {
            Ok(registration) => registration,
            Err(refusal) => return serde_json::json!({ "error": refusal }),
        };
    let runtime_id = registration.runtime_id.clone();
    match state.register_surface(registration) {
        Ok(()) => {
            tracing::debug!(
                "[Surface share] register: surface '{}' for runtime '{}'",
                surface_id,
                runtime_id
            );
            serde_json::json!({"success": true})
        }
        Err(_) => {
            tracing::warn!(
                "[Surface share] register: surface '{}' already exists",
                surface_id
            );
            serde_json::json!({"success": false})
        }
    }
}

pub(super) fn handle_check_in(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    received_ports: Vec<OwnedMachSendRight>,
) -> serde_json::Value {
    let surface_id = uuid::Uuid::new_v4().to_string();
    match registration_of_request(request, surface_id.clone(), received_ports) {
        Ok(registration) => match state.register_surface(registration) {
            Ok(()) => serde_json::json!({ "surface_id": surface_id }),
            Err(_) => serde_json::json!({"error": "a freshly minted surface id collided"}),
        },
        Err(refusal) => serde_json::json!({ "error": refusal }),
    }
}

pub(super) fn handle_lookup(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    answer_a_lookup(state, request, LookupReplyPorts::Minted)
}

/// Whether a lookup's reply carries fresh rights to the surface and its
/// timeline pair, or only the registration: a peer that holds the slot's
/// import is minted nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LookupReplyPorts {
    Minted,
    WithheldFromAPeerHoldingTheSlotImport,
}

fn answer_a_lookup(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    reply_ports_wanted: LookupReplyPorts,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    if let Some(refusal) = refusal_of_a_retired_frame_id(state.check_out_leases(), surface_id) {
        return (refusal, Vec::new());
    }
    let Some(registration) = state.registration_of(surface_id) else {
        return (
            serde_json::json!({"error": "surface not found"}),
            Vec::new(),
        );
    };
    let carries_timeline_pair = registration.timeline_send_rights.is_some();
    let reply_ports = match reply_ports_wanted {
        LookupReplyPorts::Minted => match ports_a_lookup_reply_carries(&registration) {
            Ok(reply_ports) => reply_ports,
            Err(unminted) => return (serde_json::json!({"error": unminted}), Vec::new()),
        },
        LookupReplyPorts::WithheldFromAPeerHoldingTheSlotImport => Vec::new(),
    };
    let mut reply = serde_json::json!({
        "surface_id": surface_id,
        "width": registration.width,
        "height": registration.height,
        "format": registration.format,
        "resource_type": registration.resource_type,
        "handle_type": SURFACE_HANDLE_TYPE_IOSURFACE,
        "plane_sizes": [registration.iosurface.alloc_size()],
        "plane_offsets": [0],
        "plane_strides": [registration.iosurface.bytes_per_row()],
        SURFACE_SHARE_HAS_PRODUCE_DONE_PORT: carries_timeline_pair,
        SURFACE_SHARE_HAS_CONSUME_DONE_PORT: carries_timeline_pair,
    });
    if let (Some(texture_image), Some(reply_fields)) =
        (&registration.texture_image, reply.as_object_mut())
    {
        texture_image.recipe.insert_into_wire_fields(reply_fields);
        reply_fields.insert(
            "current_image_layout".into(),
            texture_image.current_image_layout().into(),
        );
    }
    echo_the_tensor_layout_onto_a_lookup_reply(&mut reply, registration.tensor_layout.as_ref());
    (reply, reply_ports)
}

/// A fresh send right to the registration's IOSurface, then one to each edge
/// of its timeline pair when it has one.
fn ports_a_lookup_reply_carries(
    registration: &IOSurfaceShareRegistration,
) -> Result<Vec<OwnedMachSendRight>, String> {
    let mut reply_ports = vec![
        create_iosurface_mach_send_right(&registration.iosurface)
            .map_err(|unminted| unminted.to_string())?,
    ];
    if let Some(timeline_send_rights) = &registration.timeline_send_rights {
        reply_ports.push(
            timeline_send_rights
                .produce_done
                .try_clone()
                .map_err(|unminted| unminted.to_string())?,
        );
        reply_ports.push(
            timeline_send_rights
                .consume_done
                .try_clone()
                .map_err(|unminted| unminted.to_string())?,
        );
    }
    Ok(reply_ports)
}

/// Publish the layout a helper left a texture in, for the next holder to
/// transition out of. A helper that holds the slot checked out has released
/// it to the external queue family in that layout, and that release is
/// recorded for the slot's owner to acquire.
pub(super) fn handle_update_layout(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    let Some(layout) = stated_current_image_layout(request) else {
        return serde_json::json!({"error": "missing current_image_layout"});
    };
    let updated = state.update_image_layout(surface_id, layout);
    if updated
        && let Err(unrecorded_release) = state
            .check_out_leases()
            .record_image_layout_a_check_out_holder_released_in(surface_id, lease_holder, layout)
    {
        return serde_json::json!({"error": unrecorded_release.to_string()});
    }
    serde_json::json!({"success": updated})
}

/// A helper's host-side report that it released the frame at `value`,
/// signalled on the engine's own `consume_done`.
pub(super) fn answer_signal_consume_done(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    let Some(value) = request.get("value").and_then(serde_json::Value::as_u64) else {
        return serde_json::json!({"error": "missing value"});
    };
    match state
        .cross_process_timeline_pairs()
        .pair_or_refusal(surface_id)
        .and_then(|pair| pair.record_consumer_release_reported_over_the_channel(value))
    {
        Ok(()) => serde_json::json!({"success": true}),
        Err(refusal) => serde_json::json!({"error": refusal.to_string()}),
    }
}

/// A helper could not import `surface_id`'s timeline pair; the engine orders
/// that surface host-side from now on.
pub(super) fn answer_timeline_import_refused(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
) -> serde_json::Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    let reason = request
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("the helper gave no reason");
    match state
        .cross_process_timeline_pairs()
        .pair_or_refusal(surface_id)
    {
        Ok(pair) => {
            pair.fall_back_to_host_side_ordering(reason);
            serde_json::json!({"success": true})
        }
        Err(refusal) => serde_json::json!({"error": refusal.to_string()}),
    }
}

/// `lookup` plus a claim: the surface is pinned against producer reuse until
/// this connection releases it or closes. A peer that states it holds its own
/// import of a published frame's pool slot is sent no port. A refused lease
/// releases the ports already minted, unsent.
pub(super) fn handle_check_out(
    state: &IOSurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<OwnedMachSendRight>) {
    let Some(surface_id) = requested_surface_id(request) else {
        return (
            serde_json::json!({"error": "missing surface_id"}),
            Vec::new(),
        );
    };
    let peer_holds_the_pool_slot_import = split_pool_slot_and_frame_generation(surface_id)
        .is_some()
        && request
            .get(SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    let (response, reply_ports) = answer_a_lookup(
        state,
        request,
        if peer_holds_the_pool_slot_import {
            LookupReplyPorts::WithheldFromAPeerHoldingTheSlotImport
        } else {
            LookupReplyPorts::Minted
        },
    );
    if response.get("error").is_some() {
        return (response, reply_ports);
    }
    match record_check_out_lease_or_refusal(state.check_out_leases(), surface_id, lease_holder) {
        Ok(()) => (response, reply_ports),
        Err(refusal) => (refusal, Vec::new()),
    }
}
