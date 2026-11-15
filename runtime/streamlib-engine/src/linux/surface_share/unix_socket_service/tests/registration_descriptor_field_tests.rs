// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The raw-handle export fields, tensor shapes, image create info and image layout a registration carries.

use super::*;

/// The raw-handle export contract fields round-trip through the wire,
/// and their absence stays absence: a producer that registers
/// `vk_memory_type_index` / `exporting_device_uuid` sees both echoed
/// on lookup, and a producer that never states them gets a response
/// with neither key — `0` is a valid memory type index, so no default
/// may be conjured on the wire.
#[test]
fn raw_handle_export_fields_round_trip_and_absence_stays_absent() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path, mut service) = started_service(state);
    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let send_fd = make_memfd_with(b"raw-handle-export-fixture");
    let register_req = serde_json::json!({
        "op": "register",
        "surface_id": "raw-handle-export-rt",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 64,
        "format": "rgba16_float",
        "resource_type": "texture",
        "handle_type": "opaque_fd",
        "vk_image_allocation_size": 65_536u64,
        "vk_memory_type_index": 7u32,
        "exporting_device_uuid": "00112233445566778899aabbccddeeff",
    });
    let (register_resp, _) =
        send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
    assert_eq!(
        register_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "register must succeed: {register_resp:?}",
    );

    let (lookup_resp, lookup_fds) = send_request_with_fds(
        &stream,
        &serde_json::json!({"op": "lookup", "surface_id": "raw-handle-export-rt"}),
        &[],
        MAX_DMA_BUF_PLANES,
    )
    .expect("lookup request");
    close_every_fd(&lookup_fds);
    assert_eq!(
        lookup_resp
            .get("vk_memory_type_index")
            .and_then(|v| v.as_u64()),
        Some(7),
    );
    assert_eq!(
        lookup_resp
            .get("exporting_device_uuid")
            .and_then(|v| v.as_str()),
        Some("00112233445566778899aabbccddeeff"),
    );

    let bare_register_req = serde_json::json!({
        "op": "register",
        "surface_id": "raw-handle-export-bare",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 64,
        "format": "bgra32",
        "resource_type": "pixel_buffer",
    });
    let (bare_register_resp, _) = send_request_with_fds(&stream, &bare_register_req, &[send_fd], 0)
        .expect("bare register request");
    unsafe { libc::close(send_fd) };
    assert_eq!(
        bare_register_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
    );
    let (bare_lookup_resp, bare_lookup_fds) = send_request_with_fds(
        &stream,
        &serde_json::json!({"op": "lookup", "surface_id": "raw-handle-export-bare"}),
        &[],
        MAX_DMA_BUF_PLANES,
    )
    .expect("bare lookup request");
    close_every_fd(&bare_lookup_fds);
    assert!(bare_lookup_resp.get("vk_memory_type_index").is_none());
    assert!(bare_lookup_resp.get("exporting_device_uuid").is_none());

    drop(stream);
    service.stop();
}

/// A `check_in` keeps the raw-handle export contract the same way a
/// `register` does: an OPAQUE_FD pixel buffer checked in with both fields
/// sees them echoed on lookup and check-out — index `0` included — and a
/// check-in that states neither gets neither back.
#[test]
fn a_check_in_echoes_the_raw_handle_export_fields_it_states_and_no_others() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path, mut service) = started_service(state);
    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
    let send_fd = make_memfd_with(b"check-in-raw-handle-export-fixture");

    let check_in = |request: serde_json::Value| -> String {
        let (check_in_resp, _) =
            send_request_with_fds(&stream, &request, &[send_fd], 0).expect("check_in request");
        check_in_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("check_in must answer a surface id: {check_in_resp:?}"))
            .to_string()
    };
    let stated_surface_id = check_in(serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 32,
        "format": "rgba32",
        "handle_type": "opaque_fd",
        "plane_sizes": [8192u64],
        "plane_offsets": [0u64],
        "vk_memory_type_index": 0u32,
        "exporting_device_uuid": "00112233445566778899aabbccddeeff",
    }));
    let bare_surface_id = check_in(serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 32,
        "format": "rgba32",
        "handle_type": "dma_buf",
        "plane_sizes": [8192u64],
        "plane_offsets": [0u64],
    }));
    unsafe { libc::close(send_fd) };

    for op in ["lookup", "check_out"] {
        let (stated_resp, stated_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": op, "surface_id": stated_surface_id}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("request for the stated surface");
        close_every_fd(&stated_fds);
        assert_eq!(
            stated_resp
                .get("vk_memory_type_index")
                .and_then(|v| v.as_u64()),
            Some(0),
            "{op} must echo the stated index, zero included: {stated_resp:?}"
        );
        assert_eq!(
            stated_resp
                .get("exporting_device_uuid")
                .and_then(|v| v.as_str()),
            Some("00112233445566778899aabbccddeeff"),
            "{op}: {stated_resp:?}"
        );

        let (bare_resp, bare_fds) = send_request_with_fds(
            &stream,
            &serde_json::json!({"op": op, "surface_id": bare_surface_id}),
            &[],
            MAX_DMA_BUF_PLANES,
        )
        .expect("request for the bare surface");
        close_every_fd(&bare_fds);
        assert!(bare_resp.get("vk_memory_type_index").is_none(), "{op}");
        assert!(bare_resp.get("exporting_device_uuid").is_none(), "{op}");
    }

    drop(stream);
    service.stop();
}

/// A `storage_buffer` registration's tensor shape and dtype round-trip
/// through a checkout, and one that omits them is refused before the table
/// takes its fd.
#[test]
fn a_tensor_storage_buffer_registration_round_trips_its_shape_and_dtype() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path, mut service) = started_service(state.clone());
    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
    let send_fd = make_memfd_with(b"tensor-storage-buffer-fixture");

    let (register_resp, _) = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "register",
            "surface_id": "tensor-storage-buffer",
            "runtime_id": "test-runtime",
            "resource_type": "storage_buffer",
            "handle_type": "opaque_fd",
            "plane_sizes": [462u64],
            "plane_offsets": [0u64],
            "plane_strides": [0u64],
            "shape": [3u64, 7, 11],
            "dtype": "float16",
            "vk_memory_type_index": 2u32,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
        }),
        &[send_fd],
        0,
    )
    .expect("register request");
    assert_eq!(
        register_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "register must succeed: {register_resp:?}",
    );

    let (check_out_resp, check_out_fds) = send_request_with_fds(
        &stream,
        &serde_json::json!({"op": "check_out", "surface_id": "tensor-storage-buffer"}),
        &[],
        MAX_DMA_BUF_PLANES,
    )
    .expect("check_out request");
    assert_eq!(
        check_out_fds.len(),
        1,
        "the tensor's memory crosses as one fd"
    );
    close_every_fd(&check_out_fds);
    assert_eq!(check_out_resp["resource_type"], "storage_buffer");
    assert_eq!(check_out_resp["shape"], serde_json::json!([3, 7, 11]));
    assert_eq!(check_out_resp["dtype"], "float16");
    assert_eq!(check_out_resp["plane_sizes"], serde_json::json!([462]));

    let (refused_resp, _) = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "register",
            "surface_id": "tensor-without-a-shape",
            "runtime_id": "test-runtime",
            "resource_type": "storage_buffer",
            "handle_type": "opaque_fd",
            "dtype": "float32",
        }),
        &[send_fd],
        0,
    )
    .expect("register request");
    unsafe { libc::close(send_fd) };
    assert!(
        refused_resp
            .get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|refusal| refusal.contains("no shape")),
        "a tensor registration without its shape must be refused: {refused_resp:?}",
    );
    assert!(state.get_surface_planes("tensor-without-a-shape").is_none());

    drop(stream);
    service.stop();
}

/// Full `VkImageCreateInfo` round-trip through the wire: a
/// producer ships every `vk_image_*` field on register; the consumer
/// reads them off lookup. Locks the OPAQUE_FD VkImage consumer's
/// ability to rebuild a matching `VkImage` whose
/// `cudaExternalMemoryMipmappedArrayDesc` equals the host's
/// byte-for-byte. Mentally revert any of the seven echo writes in
/// `handle_lookup` and this test fails — the seven assertions are
/// independent and field-specific.
#[test]
fn vk_image_create_info_round_trip_through_register_lookup() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let send_fd = make_memfd_with(b"opaque-fd-vkimage-fixture");
    // Non-default values for every field so a regression that zeroes
    // one is caught against a deliberate non-zero assert.
    let register_req = serde_json::json!({
        "op": "register",
        "surface_id": "vk-image-wire-rt",
        "runtime_id": "test-runtime",
        "width": 256,
        "height": 256,
        "format": "rgba16_float",
        "resource_type": "texture",
        "handle_type": "opaque_fd",
        "vk_image_type": 2,               // VK_IMAGE_TYPE_3D
        "vk_image_mip_levels": 9,
        "vk_image_array_layers": 6,
        "vk_image_samples": 4,            // _4
        "vk_image_tiling": VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
        "vk_image_usage": 0x4Fu32,
        "vk_image_allocation_size": 16_777_216u64,
    });
    let (register_resp, _) =
        send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
    unsafe { libc::close(send_fd) };
    assert_eq!(
        register_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "register must succeed: {:?}",
        register_resp
    );

    let lookup_req = serde_json::json!({
        "op": "lookup",
        "surface_id": "vk-image-wire-rt",
    });
    let (lookup_resp, lookup_fds) =
        send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
            .expect("lookup request");
    for fd in &lookup_fds {
        unsafe { libc::close(*fd) };
    }

    assert_eq!(
        lookup_resp.get("vk_image_type").and_then(|v| v.as_i64()),
        Some(2),
    );
    assert_eq!(
        lookup_resp
            .get("vk_image_mip_levels")
            .and_then(|v| v.as_u64()),
        Some(9),
    );
    assert_eq!(
        lookup_resp
            .get("vk_image_array_layers")
            .and_then(|v| v.as_u64()),
        Some(6),
    );
    assert_eq!(
        lookup_resp.get("vk_image_samples").and_then(|v| v.as_i64()),
        Some(4),
    );
    assert_eq!(
        lookup_resp.get("vk_image_tiling").and_then(|v| v.as_i64()),
        Some(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
    );
    assert_eq!(
        lookup_resp.get("vk_image_usage").and_then(|v| v.as_u64()),
        Some(0x4F),
    );
    assert_eq!(
        lookup_resp
            .get("vk_image_allocation_size")
            .and_then(|v| v.as_u64()),
        Some(16_777_216),
    );

    let _ = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "release",
            "surface_id": "vk-image-wire-rt",
            "runtime_id": "test-runtime",
        }),
        &[],
        0,
    );
    drop(stream);
    service.stop();
}

/// Back-compat sibling of [`vk_image_create_info_round_trip_through_register_lookup`]:
/// a register payload that omits the seven `vk_image_*` fields must
/// surface the documented defaults on lookup. This is the contract
/// every legacy producer (DMA-BUF VkImage, OPAQUE_FD VkBuffer)
/// relies on — without the defaults, a daemon serving an old client
/// would echo zero / unknown values and break the consumer's
/// VkImageCreateInfo reconstruction. Mentally revert one of the
/// `unwrap_or(...)` defaults in `parse_vk_image_create_info_fields`
/// and this test fails on the corresponding assertion.
///
/// Default constants live in `surface_share_wire_verbs::VK_IMAGE_*_DEFAULT` —
/// the test mirrors their literal values so a default change
/// surfaces here too.
#[test]
fn vk_image_create_info_absent_fields_surface_documented_defaults() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let send_fd = make_memfd_with(b"legacy-no-vk-image-fields");
    let register_req = serde_json::json!({
        "op": "register",
        "surface_id": "legacy-defaults",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 64,
        "format": "rgba8_unorm",
        "resource_type": "texture",
        // No vk_image_* fields — exercise the defaults path.
    });
    let (register_resp, _) =
        send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
    unsafe { libc::close(send_fd) };
    assert_eq!(
        register_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "legacy register must succeed: {:?}",
        register_resp
    );

    let lookup_req = serde_json::json!({
        "op": "lookup",
        "surface_id": "legacy-defaults",
    });
    let (lookup_resp, lookup_fds) =
        send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
            .expect("lookup request");
    for fd in &lookup_fds {
        unsafe { libc::close(*fd) };
    }

    // VK_IMAGE_TYPE_2D = 1, mip = 1, layers = 1, samples = 1,
    // tiling = OPTIMAL = 0,
    // usage = TRANSFER_SRC | TRANSFER_DST | SAMPLED | STORAGE = 0x0F,
    // allocation_size = 0 (consumer derives from width * height * bpp).
    assert_eq!(
        lookup_resp.get("vk_image_type").and_then(|v| v.as_i64()),
        Some(1),
        "absent vk_image_type defaults to VK_IMAGE_TYPE_2D",
    );
    assert_eq!(
        lookup_resp
            .get("vk_image_mip_levels")
            .and_then(|v| v.as_u64()),
        Some(1),
    );
    assert_eq!(
        lookup_resp
            .get("vk_image_array_layers")
            .and_then(|v| v.as_u64()),
        Some(1),
    );
    assert_eq!(
        lookup_resp.get("vk_image_samples").and_then(|v| v.as_i64()),
        Some(1),
    );
    assert_eq!(
        lookup_resp.get("vk_image_tiling").and_then(|v| v.as_i64()),
        Some(0),
        "absent vk_image_tiling defaults to VK_IMAGE_TILING_OPTIMAL",
    );
    assert_eq!(
        lookup_resp.get("vk_image_usage").and_then(|v| v.as_u64()),
        Some(0x0F),
        "absent vk_image_usage defaults to TRANSFER_SRC|TRANSFER_DST|SAMPLED|STORAGE",
    );
    assert_eq!(
        lookup_resp
            .get("vk_image_allocation_size")
            .and_then(|v| v.as_u64()),
        Some(0),
        "absent vk_image_allocation_size defaults to 0 (consumer-derived)",
    );

    let _ = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "release",
            "surface_id": "legacy-defaults",
            "runtime_id": "test-runtime",
        }),
        &[],
        0,
    );
    drop(stream);
    service.stop();
}

/// Round-trip `current_image_layout` through register → lookup, and
/// flip it through `update_layout`. Producers seed the layout at registration and
/// re-publish it after their QFOT release barrier records, so the
/// next consumer's lookup sees the post-release layout instead of
/// the synthesized UNDEFINED.
#[test]
fn current_image_layout_round_trip_through_register_lookup_update() {
    // VK_IMAGE_LAYOUT_GENERAL = 1, SHADER_READ_ONLY_OPTIMAL = 5.
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let send_fd = make_memfd_with(b"layout-test");
    let register_req = serde_json::json!({
        "op": "register",
        "surface_id": "layout-rt",
        "runtime_id": "test-runtime",
        "width": 16,
        "height": 16,
        "format": "rgba8_unorm",
        "resource_type": "texture",
        "current_image_layout": 1,
    });
    let (register_resp, _) =
        send_request_with_fds(&stream, &register_req, &[send_fd], 0).expect("register request");
    unsafe { libc::close(send_fd) };
    assert_eq!(
        register_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "register must succeed: {:?}",
        register_resp
    );

    let lookup_req = serde_json::json!({
        "op": "lookup",
        "surface_id": "layout-rt",
    });
    let (lookup_resp, lookup_fds) =
        send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
            .expect("lookup request");
    for fd in &lookup_fds {
        unsafe { libc::close(*fd) };
    }
    assert_eq!(
        lookup_resp
            .get("current_image_layout")
            .and_then(|v| v.as_i64()),
        Some(1),
        "lookup must echo the producer-declared layout"
    );

    let update_req = serde_json::json!({
        "op": "update_layout",
        "surface_id": "layout-rt",
        "current_image_layout": 5,
    });
    let (update_resp, _) =
        send_request_with_fds(&stream, &update_req, &[], 0).expect("update_layout request");
    assert_eq!(
        update_resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "update_layout must succeed: {:?}",
        update_resp
    );

    let (lookup_resp, lookup_fds) =
        send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
            .expect("lookup after update");
    for fd in &lookup_fds {
        unsafe { libc::close(*fd) };
    }
    assert_eq!(
        lookup_resp
            .get("current_image_layout")
            .and_then(|v| v.as_i64()),
        Some(5),
        "subsequent lookup must see post-update layout"
    );

    // Updating an unknown surface_id reports failure rather than a
    // crash — handle_update_layout must be idempotent against the
    // race with release_every_surface_registered_by.
    let bad_update = serde_json::json!({
        "op": "update_layout",
        "surface_id": "missing",
        "current_image_layout": 0,
    });
    let (bad_resp, _) = send_request_with_fds(&stream, &bad_update, &[], 0).expect("bad update");
    assert_eq!(
        bad_resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "missing surface_id must report success=false"
    );

    drop(stream);
    service.stop();
}
