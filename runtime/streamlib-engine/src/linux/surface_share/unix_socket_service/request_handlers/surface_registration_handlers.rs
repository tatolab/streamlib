// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Registering, checking in, updating the layout of, and unregistering a surface.

use super::*;

pub(in crate::linux::surface_share::unix_socket_service) fn handle_register(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    received_fds: &[RawFd],
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            return (
                serde_json::json!({"error": "missing surface_id"}),
                Vec::new(),
            );
        }
    };

    // Registrations are per-slot; the `#<generation>` suffix belongs to the
    // published frame ids minted *over* a slot. Accepting one here would
    // fork the table's key grammar and let a registration collide with a
    // frame id's slot key.
    if crate::core::rhi::PublishedPixelBufferFrameId::parse(surface_id).is_some() {
        return (
            serde_json::json!({"error": format!(
                "surface id '{surface_id}' ends in the reserved #<generation> suffix; \
                 register the pool slot and let acquisition publish the frame ids"
            )}),
            Vec::new(),
        );
    }

    let runtime_id = requested_runtime_id(request);

    // Peel off the optional trailing timeline FDs in the order the host
    // attached them: plane FDs first, then `produce_done` (if present),
    // then `consume_done` (if present). The wire signals each timeline's
    // presence via a JSON boolean (`has_produce_done_fd`,
    // `has_consume_done_fd`); the SCM_RIGHTS slot count matches the sum.
    // The single-writer-per-edge model is documented in
    // the single-writer-per-edge timeline rule.
    let has_produce_done_fd = request
        .get("has_produce_done_fd")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_consume_done_fd = request
        .get("has_consume_done_fd")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let trailing_fd_count = (has_produce_done_fd as usize) + (has_consume_done_fd as usize);
    let plane_count = received_fds.len().saturating_sub(trailing_fd_count);
    let plane_fds: &[RawFd] = &received_fds[..plane_count];
    let mut trailing_idx = plane_count;
    let produce_done_src_fd: Option<RawFd> = if has_produce_done_fd {
        let fd = received_fds.get(trailing_idx).copied();
        trailing_idx += 1;
        fd
    } else {
        None
    };
    let consume_done_src_fd: Option<RawFd> = if has_consume_done_fd {
        received_fds.get(trailing_idx).copied()
    } else {
        None
    };

    if plane_fds.is_empty() {
        return (
            serde_json::json!({"error": "missing DMA-BUF fd(s)"}),
            Vec::new(),
        );
    }
    if plane_fds.len() > MAX_DMA_BUF_PLANES {
        return (
            serde_json::json!({
                "error": format!(
                    "too many plane fds: {} > MAX_DMA_BUF_PLANES ({})",
                    plane_fds.len(), MAX_DMA_BUF_PLANES
                )
            }),
            Vec::new(),
        );
    }
    if has_produce_done_fd && produce_done_src_fd.is_none() {
        return (
            serde_json::json!({
                "error": "has_produce_done_fd=true but no trailing produce_done FD attached"
            }),
            Vec::new(),
        );
    }
    if has_consume_done_fd && consume_done_src_fd.is_none() {
        return (
            serde_json::json!({
                "error": "has_consume_done_fd=true but no trailing consume_done FD attached"
            }),
            Vec::new(),
        );
    }

    let (plane_sizes, plane_offsets, plane_strides) =
        match parse_plane_arrays(request, plane_fds.len()) {
            Some(arrays) => arrays,
            None => {
                return (
                    serde_json::json!({
                        "error": "plane_sizes/plane_offsets/plane_strides length mismatch"
                    }),
                    Vec::new(),
                );
            }
        };
    let drm_format_modifier = request
        .get("drm_format_modifier")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");
    let handle_type = request
        .get("handle_type")
        .and_then(|v| v.as_str())
        .unwrap_or("dma_buf");
    // Producer's declared layout (i32 per `VkImageLayout`). Absent or
    // unparseable defaults to `VK_IMAGE_LAYOUT_UNDEFINED` (0) — the
    // back-compat behaviour for clients that haven't been updated to
    // publish layout state through the surface-share wire format.
    let current_image_layout = request
        .get("current_image_layout")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .unwrap_or(0);
    // OPAQUE_FD VkImage carries the full VkImageCreateInfo shape so a
    // CUDA-importable consumer can rebuild a matching VkImage; every
    // field absent-defaults to the OPAQUE_FD constructor's hardcoded
    // shape so legacy callers see no behavior change.
    let vk_image = parse_vk_image_create_info_fields(request);
    let RawHandleExportContractFields {
        vk_memory_type_index,
        exporting_device_uuid,
    } = parse_raw_handle_export_contract_fields(request);
    let tensor_layout = match tensor_layout_of_a_storage_buffer_registration(request) {
        Ok(tensor_layout) => tensor_layout,
        Err(refusal) => return (serde_json::json!({"error": refusal}), Vec::new()),
    };
    let pixel_planes_within_one_allocation =
        match pixel_planes_of_a_pixel_buffer_registration(request, &plane_sizes) {
            Ok(pixel_planes_within_one_allocation) => pixel_planes_within_one_allocation,
            Err(refusal) => return (serde_json::json!({"error": refusal}), Vec::new()),
        };

    let mut dup_plane_fds: Vec<RawFd> = Vec::with_capacity(plane_fds.len());
    for fd in plane_fds {
        let dup_fd = unsafe { libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup_fd < 0 {
            for d in &dup_plane_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup memory fd"}),
                Vec::new(),
            );
        }
        dup_plane_fds.push(dup_fd);
    }

    let dup_produce_done_fd: Option<RawFd> = match produce_done_src_fd {
        Some(src) => {
            let dup = unsafe { libc::fcntl(src, libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                for d in &dup_plane_fds {
                    unsafe { libc::close(*d) };
                }
                return (
                    serde_json::json!({"error": "failed to dup produce_done FD"}),
                    Vec::new(),
                );
            }
            Some(dup)
        }
        None => None,
    };
    let dup_consume_done_fd: Option<RawFd> = match consume_done_src_fd {
        Some(src) => {
            let dup = unsafe { libc::fcntl(src, libc::F_DUPFD_CLOEXEC, 0) };
            if dup < 0 {
                for d in &dup_plane_fds {
                    unsafe { libc::close(*d) };
                }
                if let Some(fd) = dup_produce_done_fd {
                    unsafe { libc::close(fd) };
                }
                return (
                    serde_json::json!({"error": "failed to dup consume_done FD"}),
                    Vec::new(),
                );
            }
            Some(dup)
        }
        None => None,
    };

    match state.register_surface(SurfaceRegistration {
        surface_id,
        runtime_id,
        dma_buf_fds: dup_plane_fds,
        plane_sizes,
        plane_offsets,
        plane_strides,
        width,
        height,
        format,
        resource_type,
        handle_type,
        drm_format_modifier,
        produce_done_fd: dup_produce_done_fd,
        consume_done_fd: dup_consume_done_fd,
        current_image_layout,
        vk_image_type: vk_image.vk_image_type,
        vk_image_mip_levels: vk_image.vk_image_mip_levels,
        vk_image_array_layers: vk_image.vk_image_array_layers,
        vk_image_samples: vk_image.vk_image_samples,
        vk_image_tiling: vk_image.vk_image_tiling,
        vk_image_usage: vk_image.vk_image_usage,
        vk_image_allocation_size: vk_image.vk_image_allocation_size,
        vk_memory_type_index,
        exporting_device_uuid,
        tensor_layout,
        pixel_planes_within_one_allocation,
    }) {
        Ok(()) => {
            tracing::debug!(
                "[Surface share] register: surface '{}' for runtime '{}' ({} plane(s), \
                 produce_done={}, consume_done={})",
                surface_id,
                runtime_id,
                plane_fds.len(),
                has_produce_done_fd,
                has_consume_done_fd,
            );
            (serde_json::json!({"success": true}), Vec::new())
        }
        Err((leftover_planes, leftover_produce, leftover_consume)) => {
            for fd in &leftover_planes {
                unsafe { libc::close(*fd) };
            }
            if let Some(fd) = leftover_produce {
                unsafe { libc::close(fd) };
            }
            if let Some(fd) = leftover_consume {
                unsafe { libc::close(fd) };
            }
            tracing::warn!(
                "[Surface share] register: surface '{}' already exists",
                surface_id
            );
            (serde_json::json!({"success": false}), Vec::new())
        }
    }
}

/// Publish the layout a peer left a texture in. A peer that holds the slot
/// checked out has released it to the external queue family in that layout,
/// and that release is recorded for the slot's owner to acquire.
pub(in crate::linux::surface_share::unix_socket_service) fn handle_update_layout(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    lease_holder: SurfaceCheckOutLeaseHolderId,
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            return (
                serde_json::json!({"error": "missing surface_id"}),
                Vec::new(),
            );
        }
    };

    let layout = match request.get("current_image_layout").and_then(|v| v.as_i64()) {
        Some(v) => v as i32,
        None => {
            return (
                serde_json::json!({"error": "missing current_image_layout"}),
                Vec::new(),
            );
        }
    };

    let updated = state.update_image_layout(surface_id, layout);
    if updated
        && let Err(unrecorded_release) = state
            .check_out_leases()
            .record_image_layout_a_check_out_holder_released_in(surface_id, lease_holder, layout)
    {
        return (
            serde_json::json!({"error": unrecorded_release.to_string()}),
            Vec::new(),
        );
    }
    (serde_json::json!({"success": updated}), Vec::new())
}

pub(in crate::linux::surface_share::unix_socket_service) fn handle_unregister(
    state: &SurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<RawFd>) {
    (answer_unregister(state, request), Vec::new())
}

pub(in crate::linux::surface_share::unix_socket_service) fn handle_check_in(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    received_fds: &[RawFd],
) -> (serde_json::Value, Vec<RawFd>) {
    let runtime_id = requested_runtime_id(request);

    if received_fds.is_empty() {
        return (
            serde_json::json!({"error": "missing DMA-BUF fd(s)"}),
            Vec::new(),
        );
    }
    if received_fds.len() > MAX_DMA_BUF_PLANES {
        return (
            serde_json::json!({
                "error": format!(
                    "too many plane fds: {} > MAX_DMA_BUF_PLANES ({})",
                    received_fds.len(), MAX_DMA_BUF_PLANES
                )
            }),
            Vec::new(),
        );
    }

    let (plane_sizes, plane_offsets, plane_strides) =
        match parse_plane_arrays(request, received_fds.len()) {
            Some(arrays) => arrays,
            None => {
                return (
                    serde_json::json!({
                        "error": "plane_sizes/plane_offsets/plane_strides length mismatch"
                    }),
                    Vec::new(),
                );
            }
        };
    let drm_format_modifier = request
        .get("drm_format_modifier")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");
    let handle_type = request
        .get("handle_type")
        .and_then(|v| v.as_str())
        .unwrap_or("dma_buf");
    // Parse the optional VkImageCreateInfo block. `check_in` is the
    // subprocess-registers-pixel-buffer path that legacy DMA-BUF /
    // OPAQUE_FD buffer consumers ride; every field absent-defaults to
    // the OPAQUE_FD constructor's hardcoded shape so legacy callers
    // see no behavior change.
    let vk_image = parse_vk_image_create_info_fields(request);
    let RawHandleExportContractFields {
        vk_memory_type_index,
        exporting_device_uuid,
    } = parse_raw_handle_export_contract_fields(request);
    let pixel_planes_within_one_allocation =
        match pixel_planes_of_a_pixel_buffer_registration(request, &plane_sizes) {
            Ok(pixel_planes_within_one_allocation) => pixel_planes_within_one_allocation,
            Err(refusal) => return (serde_json::json!({"error": refusal}), Vec::new()),
        };

    let surface_id = uuid::Uuid::new_v4().to_string();

    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(received_fds.len());
    for fd in received_fds {
        let dup = unsafe { libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                Vec::new(),
            );
        }
        dup_fds.push(dup);
    }

    if let Err((leftover_planes, leftover_produce, leftover_consume)) =
        state.register_surface(SurfaceRegistration {
            surface_id: &surface_id,
            runtime_id,
            dma_buf_fds: dup_fds,
            plane_sizes,
            plane_offsets,
            plane_strides,
            width,
            height,
            format,
            resource_type,
            handle_type,
            drm_format_modifier,
            // check_in is the subprocess-registers-pixel-buffer path used by
            // CPU-readback / legacy DMA-BUF consumers. None of those carry
            // their own timelines through this op — host-registered
            // render-target paths (`handle_register`) provide the
            // single-writer-per-edge `produce_done` / `consume_done` FDs.
            produce_done_fd: None,
            consume_done_fd: None,
            // check_in surfaces are pixel buffers — `VkBuffer`s have no
            // image layout. Seed UNDEFINED; the field is unused for buffer
            // surfaces but must be populated for the wire format.
            current_image_layout: 0,
            vk_image_type: vk_image.vk_image_type,
            vk_image_mip_levels: vk_image.vk_image_mip_levels,
            vk_image_array_layers: vk_image.vk_image_array_layers,
            vk_image_samples: vk_image.vk_image_samples,
            vk_image_tiling: vk_image.vk_image_tiling,
            vk_image_usage: vk_image.vk_image_usage,
            vk_image_allocation_size: vk_image.vk_image_allocation_size,
            vk_memory_type_index,
            exporting_device_uuid,
            tensor_layout: None,
            pixel_planes_within_one_allocation,
        })
    {
        for fd in &leftover_planes {
            unsafe { libc::close(*fd) };
        }
        if let Some(fd) = leftover_produce {
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = leftover_consume {
            unsafe { libc::close(fd) };
        }
    }

    (serde_json::json!({"surface_id": surface_id}), Vec::new())
}
