// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-share verbs' platform-neutral half, shared by the Unix-socket
//! arm and the raw-Mach arm: what a request names, the checkout lease a claim
//! takes, and what a closed connection gives back. Only how a surface's
//! handle crosses differs between the two.

use serde_json::Value;

use super::{SurfaceCheckOutLeaseHolderId, SurfaceCheckOutLeaseRegistry};

/// The `runtime_id` a request that names none is charged to. Never a real
/// runtime, so a connection that only ever sends it latches none.
pub(crate) const SURFACE_SHARE_UNNAMED_RUNTIME_ID: &str = "unknown";

/// Wire value of `resource_type` for a texture registration — the only
/// kind a texture lookup imports.
pub(crate) const SURFACE_RESOURCE_TYPE_TEXTURE: &str = "texture";

/// Wire value of `resource_type` for a pixel-buffer registration.
pub(crate) const SURFACE_RESOURCE_TYPE_PIXEL_BUFFER: &str = "pixel_buffer";

/// A surface-share table's registrations, by the runtime that made them.
pub(crate) trait SurfaceShareRegistrationsByRuntime {
    /// Surface ids `runtime_id` registered.
    fn surface_ids_by_runtime(&self, runtime_id: &str) -> Vec<String>;
    /// Drop `surface_id`'s registration when `runtime_id` made it.
    fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool;
}

/// The surface a request names, or `None` when it names none.
pub(crate) fn requested_surface_id(request: &Value) -> Option<&str> {
    request.get("surface_id").and_then(Value::as_str)
}

/// The `VkImageLayout` a request or an answer names under
/// `current_image_layout`, or `None` when it names none that fits.
#[cfg_attr(
    target_os = "linux",
    expect(
        dead_code,
        reason = "the Unix-socket arm parses the layout in its own handlers"
    )
)]
pub(crate) fn stated_current_image_layout(wire_fields: &Value) -> Option<i32> {
    wire_fields
        .get("current_image_layout")
        .and_then(Value::as_i64)
        .and_then(|layout| i32::try_from(layout).ok())
}

/// The runtime a request charges its registration to.
pub(crate) fn requested_runtime_id(request: &Value) -> &str {
    request
        .get("runtime_id")
        .and_then(Value::as_str)
        .unwrap_or(SURFACE_SHARE_UNNAMED_RUNTIME_ID)
}

/// Latch the first real `runtime_id` a connection names — whose
/// registrations go when an out-of-process client's connection closes.
///
/// One runtime per connection for the connection's life: a helper inherits
/// its runtime id once at spawn and never multiplexes sibling runtimes over
/// one connection.
pub(crate) fn latch_the_first_named_runtime_id(
    observed_runtime_id: &mut Option<String>,
    request: &Value,
) {
    if observed_runtime_id.is_none() {
        *observed_runtime_id = request
            .get("runtime_id")
            .and_then(Value::as_str)
            .filter(|runtime_id| {
                !runtime_id.is_empty() && *runtime_id != SURFACE_SHARE_UNNAMED_RUNTIME_ID
            })
            .map(str::to_string);
    }
}

/// The answer a lookup of a retired published frame id gets, before any
/// handle crosses: the slot's registration still exists, the frame it named
/// does not.
pub(crate) fn refusal_of_a_retired_frame_id(
    leases: &SurfaceCheckOutLeaseRegistry,
    surface_id: &str,
) -> Option<Value> {
    let retired = leases.refuse_a_retired_frame_id(surface_id).err()?;
    tracing::warn!(
        "[Surface share] refusing lookup of '{}': {}",
        surface_id,
        retired
    );
    Some(serde_json::json!({"error": retired.to_string()}))
}

/// Record `holder`'s claim on `surface_id`, or the answer that refuses the
/// checkout.
///
/// Handing out a handle the pool believes is free is exactly the silent
/// wrongness the lease exists to remove, so a lease that cannot be recorded
/// refuses the checkout outright.
pub(crate) fn record_check_out_lease_or_refusal(
    leases: &SurfaceCheckOutLeaseRegistry,
    surface_id: &str,
    holder: SurfaceCheckOutLeaseHolderId,
) -> Result<(), Value> {
    let Err(unrecordable) = leases.record_check_out_lease(surface_id, holder) else {
        return Ok(());
    };
    tracing::error!(
        "[Surface share] refusing check_out of '{}' for {}: {}",
        surface_id,
        holder,
        unrecordable
    );
    // The recycled-frame refusal is its own story and travels verbatim;
    // wrapping fits only the lease-bookkeeping failures.
    let error = match &unrecordable {
        crate::core::Error::SurfaceFrameRecycled { .. } => unrecordable.to_string(),
        _ => format!(
            "no checkout lease could be recorded for surface '{surface_id}', so its \
             producer could recycle the slot while you read it: {unrecordable}"
        ),
    };
    Err(serde_json::json!({ "error": error }))
}

/// Answer `release_check_out`: drop one of `holder`'s claims.
///
/// `released: false` means this connection held no lease on that id —
/// reported rather than raised, because the caller is usually a `Drop` with
/// nowhere to raise to, and one connection must never be able to unpin
/// another's frame.
pub(crate) fn answer_release_check_out(
    leases: &SurfaceCheckOutLeaseRegistry,
    request: &Value,
    holder: SurfaceCheckOutLeaseHolderId,
) -> Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    match leases.release_one_check_out_lease(surface_id, holder) {
        Ok(released) => serde_json::json!({"success": true, "released": released}),
        Err(unreadable) => serde_json::json!({"error": unreadable.to_string()}),
    }
}

/// Answer `unregister` / `release`: drop the registration the requesting
/// runtime made.
pub(crate) fn answer_unregister(
    registrations: &dyn SurfaceShareRegistrationsByRuntime,
    request: &Value,
) -> Value {
    let Some(surface_id) = requested_surface_id(request) else {
        return serde_json::json!({"error": "missing surface_id"});
    };
    serde_json::json!({
        "success": registrations.release_surface(surface_id, requested_runtime_id(request))
    })
}

/// Release every surface `runtime_id` registered. Idempotent: a surface
/// already released is simply absent.
pub(crate) fn release_every_surface_registered_by(
    registrations: &dyn SurfaceShareRegistrationsByRuntime,
    runtime_id: &str,
) {
    let surface_ids = registrations.surface_ids_by_runtime(runtime_id);
    if surface_ids.is_empty() {
        return;
    }
    tracing::info!(
        "[Surface share] releasing {} surface(s) registered by '{}' after its connection closed",
        surface_ids.len(),
        runtime_id,
    );
    for surface_id in surface_ids {
        let _ = registrations.release_surface(&surface_id, runtime_id);
    }
}

/// What a closed connection gives back: every checkout lease it held, and —
/// when `registrations_of_its_runtime` names the table and the runtime of a
/// client in another process — every surface that runtime registered.
///
/// A lease may never outlive the reader holding it, so leases go for every
/// connection. A registration from this process outlives its connection by
/// design, so only another process's go.
pub(crate) fn release_what_a_closed_connection_held(
    leases: &SurfaceCheckOutLeaseRegistry,
    holder: SurfaceCheckOutLeaseHolderId,
    closed_connection: &dyn std::fmt::Display,
    registrations_of_its_runtime: Option<(&dyn SurfaceShareRegistrationsByRuntime, &str)>,
) {
    match leases.release_every_check_out_lease_held_by(holder) {
        Ok(0) => {}
        Ok(freed) => tracing::debug!(
            "[Surface share] {} closed, freeing {} slot(s) for their producers",
            closed_connection,
            freed
        ),
        Err(unreadable) => tracing::error!(
            "[Surface share] could not reclaim {}'s checkout leases: {}. Their pool slots stay \
             pinned until the runtime stops.",
            closed_connection,
            unreadable
        ),
    }
    if let Some((registrations, runtime_id)) = registrations_of_its_runtime {
        release_every_surface_registered_by(registrations, runtime_id);
    }
}

/// Defaults for the `vk_image_*` fields a registration omits — the shape
/// `HostVulkanTexture::new_opaque_fd_export` allocates and the consumer's
/// `from_opaque_fd` rebuilds.
pub const VK_IMAGE_TYPE_DEFAULT: i32 = 1; // VK_IMAGE_TYPE_2D
pub const VK_IMAGE_MIP_LEVELS_DEFAULT: u32 = 1;
pub const VK_IMAGE_ARRAY_LAYERS_DEFAULT: u32 = 1;
pub const VK_IMAGE_SAMPLES_DEFAULT: i32 = 1; // VK_SAMPLE_COUNT_1_BIT
pub const VK_IMAGE_TILING_DEFAULT: i32 = 0; // VK_IMAGE_TILING_OPTIMAL
/// `TRANSFER_SRC (0x01) | TRANSFER_DST (0x02) | SAMPLED (0x04) | STORAGE (0x08)`.
pub const VK_IMAGE_USAGE_DEFAULT: u32 = 0x0F;
pub const VK_IMAGE_ALLOCATION_SIZE_DEFAULT: u64 = 0;

/// The seven `vk_image_*` fields a cross-process `VkImage`'s registration
/// carries so a consumer can rebuild a matching `VkImageCreateInfo`; a field
/// the registration omits takes its `VK_IMAGE_*_DEFAULT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VkImageCreateInfoFields {
    pub(crate) vk_image_type: i32,
    pub(crate) vk_image_mip_levels: u32,
    pub(crate) vk_image_array_layers: u32,
    pub(crate) vk_image_samples: i32,
    pub(crate) vk_image_tiling: i32,
    pub(crate) vk_image_usage: u32,
    pub(crate) vk_image_allocation_size: u64,
}

impl VkImageCreateInfoFields {
    /// The fields under their wire keys, into a registration request or a
    /// lookup's reply.
    #[cfg_attr(
        target_os = "linux",
        expect(
            dead_code,
            reason = "the Unix-socket arm writes these from its own table"
        )
    )]
    pub(crate) fn insert_into_wire_fields(&self, reply: &mut serde_json::Map<String, Value>) {
        reply.insert("vk_image_type".into(), self.vk_image_type.into());
        reply.insert(
            "vk_image_mip_levels".into(),
            self.vk_image_mip_levels.into(),
        );
        reply.insert(
            "vk_image_array_layers".into(),
            self.vk_image_array_layers.into(),
        );
        reply.insert("vk_image_samples".into(), self.vk_image_samples.into());
        reply.insert("vk_image_tiling".into(), self.vk_image_tiling.into());
        reply.insert("vk_image_usage".into(), self.vk_image_usage.into());
        reply.insert(
            "vk_image_allocation_size".into(),
            self.vk_image_allocation_size.into(),
        );
    }
}

/// The `vk_image_*` fields a registration request carries.
pub(crate) fn parse_vk_image_create_info_fields(request: &Value) -> VkImageCreateInfoFields {
    let as_i32 = |key: &str, default: i32| -> i32 {
        request
            .get(key)
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
            .unwrap_or(default)
    };
    let as_u32 = |key: &str, default: u32| -> u32 {
        request
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(default)
    };
    let as_u64 = |key: &str, default: u64| -> u64 {
        request.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
    };
    VkImageCreateInfoFields {
        vk_image_type: as_i32("vk_image_type", VK_IMAGE_TYPE_DEFAULT),
        vk_image_mip_levels: as_u32("vk_image_mip_levels", VK_IMAGE_MIP_LEVELS_DEFAULT),
        vk_image_array_layers: as_u32("vk_image_array_layers", VK_IMAGE_ARRAY_LAYERS_DEFAULT),
        vk_image_samples: as_i32("vk_image_samples", VK_IMAGE_SAMPLES_DEFAULT),
        vk_image_tiling: as_i32("vk_image_tiling", VK_IMAGE_TILING_DEFAULT),
        vk_image_usage: as_u32("vk_image_usage", VK_IMAGE_USAGE_DEFAULT),
        vk_image_allocation_size: as_u64(
            "vk_image_allocation_size",
            VK_IMAGE_ALLOCATION_SIZE_DEFAULT,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_latches_the_first_runtime_it_names_and_skips_the_unnamed_defaults() {
        let mut observed = None;
        latch_the_first_named_runtime_id(&mut observed, &serde_json::json!({"op": "check_out"}));
        latch_the_first_named_runtime_id(&mut observed, &serde_json::json!({"runtime_id": ""}));
        latch_the_first_named_runtime_id(
            &mut observed,
            &serde_json::json!({"runtime_id": SURFACE_SHARE_UNNAMED_RUNTIME_ID}),
        );
        assert_eq!(observed, None);
        latch_the_first_named_runtime_id(&mut observed, &serde_json::json!({"runtime_id": "R-a"}));
        latch_the_first_named_runtime_id(&mut observed, &serde_json::json!({"runtime_id": "R-b"}));
        assert_eq!(observed.as_deref(), Some("R-a"));
    }

    #[test]
    fn a_request_naming_no_runtime_is_charged_to_the_unnamed_runtime() {
        assert_eq!(
            requested_runtime_id(&serde_json::json!({"op": "release"})),
            SURFACE_SHARE_UNNAMED_RUNTIME_ID
        );
        assert_eq!(
            requested_runtime_id(&serde_json::json!({"runtime_id": "R-a"})),
            "R-a"
        );
    }
}
