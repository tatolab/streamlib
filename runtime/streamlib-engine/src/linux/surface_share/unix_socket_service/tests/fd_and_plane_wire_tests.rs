// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Plane fds, handle types and DRM modifiers across the wire, and the cap on fds per message.

use super::*;

/// `handle_type` rides along through the register/lookup wire path.
/// Subprocess consumers read
/// it to dispatch on the import API: `"dma_buf"` →
/// `ConsumerVulkanBuffer::from_dma_buf_fds`, `"opaque_fd"` →
/// `ConsumerVulkanBuffer::from_opaque_fd`. The default-when-absent
/// behavior (`"dma_buf"`) preserves backward compatibility with every
/// adapter that predates this field. Locking the round-trip contract
/// for both halves: explicit-OPAQUE_FD and default-DMA-BUF.
#[test]
fn handle_type_round_trips_explicit_opaque_fd_and_default_dma_buf() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    // Half 1: explicit OPAQUE_FD round-trip. The wire treats the FD
    // opaquely — a memfd stands in for what the host RHI would emit
    // from `HostVulkanBuffer::export_opaque_fd_memory`.
    let send_fd = make_memfd_with(b"opaque-fd-fixture");
    let opaque_req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 64,
        "format": "bgra8_unorm",
        "resource_type": "pixel_buffer",
        "handle_type": "opaque_fd",
        "plane_sizes": [64u64 * 64 * 4],
        "plane_offsets": [0u64],
        "plane_strides": [0u64],
    });
    let (opaque_resp, _) =
        send_request_with_fds(&stream, &opaque_req, &[send_fd], 0).expect("check_in opaque_fd");
    unsafe { libc::close(send_fd) };
    let opaque_surface_id = opaque_resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id")
        .to_string();

    let lookup_resp = {
        let req = serde_json::json!({
            "op": "check_out",
            "surface_id": opaque_surface_id,
        });
        let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out opaque_fd");
        for fd in &fds {
            unsafe { libc::close(*fd) };
        }
        resp
    };
    assert_eq!(
        lookup_resp.get("handle_type").and_then(|v| v.as_str()),
        Some("opaque_fd"),
        "OPAQUE_FD round-trip: lookup must echo registered handle_type",
    );

    // Half 2: handle_type-absent register defaults to dma_buf on both
    // wire-parse and lookup-emit. Lock the back-compat contract so a
    // future schema change can't silently flip the default.
    let send_fd2 = make_memfd_with(b"legacy-dma-buf-fixture");
    let legacy_req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 64,
        "format": "bgra8_unorm",
        "resource_type": "pixel_buffer",
        "plane_sizes": [64u64 * 64 * 4],
        "plane_offsets": [0u64],
        "plane_strides": [0u64],
        // no handle_type — must default to "dma_buf"
    });
    let (legacy_resp, _) =
        send_request_with_fds(&stream, &legacy_req, &[send_fd2], 0).expect("check_in legacy");
    unsafe { libc::close(send_fd2) };
    let legacy_surface_id = legacy_resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id")
        .to_string();

    let legacy_lookup = {
        let req = serde_json::json!({
            "op": "check_out",
            "surface_id": legacy_surface_id,
        });
        let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out legacy");
        for fd in &fds {
            unsafe { libc::close(*fd) };
        }
        resp
    };
    assert_eq!(
        legacy_lookup.get("handle_type").and_then(|v| v.as_str()),
        Some("dma_buf"),
        "absent handle_type must default to dma_buf on both register and lookup",
    );

    for sid in [&opaque_surface_id, &legacy_surface_id] {
        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": sid,
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );
    }

    drop(stream);
    service.stop();
}

/// `drm_format_modifier` and `plane_strides` ride along through the
/// register/lookup path. The host adapter writes the modifier into the
/// `SurfaceTransportHandle` field defined in `streamlib-surface-adapter`; the
/// consumer reads it from the lookup response and passes it to
/// `EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT` (or the Vulkan equivalent).
/// A round-trip through the wire is the test that locks the contract.
#[test]
fn drm_format_modifier_and_strides_round_trip() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let send_fd = make_memfd_with(b"rt-render-target-payload");
    // An NVIDIA-tiled modifier from the live probe (one of the values
    // NVIDIA EGL samples linear DMA-BUFs only).
    // The wire treats it opaquely — no need to validate the exact bits.
    let chosen_modifier: u64 = 0x0300_0000_0060_6014;
    let pitch: u64 = 1920 * 4;
    let req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 1920,
        "height": 1080,
        "format": "bgra8_unorm",
        "resource_type": "texture",
        "plane_sizes": [pitch * 1080],
        "plane_offsets": [0u64],
        "plane_strides": [pitch],
        "drm_format_modifier": chosen_modifier,
    });
    let (resp, _) = send_request_with_fds(&stream, &req, &[send_fd], 0).expect("check_in request");
    unsafe { libc::close(send_fd) };
    let surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in response")
        .to_string();

    let lookup_req = serde_json::json!({
        "op": "check_out",
        "surface_id": surface_id,
    });
    let (lookup_resp, lookup_fds) =
        send_request_with_fds(&stream, &lookup_req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out request");
    for fd in &lookup_fds {
        unsafe { libc::close(*fd) };
    }

    assert_eq!(
        lookup_resp
            .get("drm_format_modifier")
            .and_then(|v| v.as_u64()),
        Some(chosen_modifier),
        "modifier round-trip: lookup must echo the registered value verbatim",
    );
    assert_eq!(
        lookup_resp
            .get("plane_strides")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).collect::<Vec<_>>()),
        Some(vec![pitch]),
        "plane_strides round-trip: lookup must echo registered values verbatim",
    );

    let _ = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": "test-runtime",
        }),
        &[],
        0,
    );
    drop(stream);
    service.stop();
}

/// A zero `drm_format_modifier` is `DRM_FORMAT_MOD_LINEAR` when the image
/// was created with `DRM_FORMAT_MODIFIER_EXT` tiling and "no modifier at
/// all" otherwise. The consumer can only tell the two apart if the tiling
/// crosses the wire beside the modifier, so both must survive the
/// round trip.
#[test]
fn a_linear_modifier_stays_distinguishable_from_no_modifier_across_the_wire() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let send_fd = make_memfd_with(b"linear-modifier-payload");
    let pitch: u64 = 64 * 4;
    let req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 64,
        "height": 64,
        "format": "bgra8_unorm",
        "resource_type": "texture",
        "plane_sizes": [pitch * 64],
        "plane_offsets": [0u64],
        "plane_strides": [pitch],
        // DRM_FORMAT_MOD_LINEAR — a real modifier the driver chose,
        // whose numeric value is indistinguishable from "unset".
        "drm_format_modifier": 0u64,
        "vk_image_tiling": VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
    });
    let (resp, _) = send_request_with_fds(&stream, &req, &[send_fd], 0).expect("check_in request");
    unsafe { libc::close(send_fd) };
    let surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in response")
        .to_string();

    let (lookup_resp, lookup_fds) = send_request_with_fds(
        &stream,
        &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
        &[],
        MAX_DMA_BUF_PLANES,
    )
    .expect("check_out request");
    for fd in &lookup_fds {
        unsafe { libc::close(*fd) };
    }

    assert_eq!(
        lookup_resp
            .get("drm_format_modifier")
            .and_then(|v| v.as_u64()),
        Some(0),
        "the LINEAR modifier is zero and must echo back as zero",
    );
    assert_eq!(
        lookup_resp.get("vk_image_tiling").and_then(|v| v.as_i64()),
        Some(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
        "without the tiling beside it, a consumer reads the zero modifier \
         as an image that never went through VK_EXT_image_drm_format_modifier",
    );

    let _ = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": "test-runtime",
        }),
        &[],
        0,
    );
    drop(stream);
    service.stop();
}

/// The service must refuse a check_in whose fd count exceeds the wire
/// helper's SCM_RIGHTS cap instead of truncating silently. The check
/// runs in the wire helper before the handler, so this exercises the
/// shared back-pressure path every caller relies on.
///
/// Cap shape:
/// - `MAX_DMA_BUF_PLANES = 4` — the data-plane cap that adapters
///   enforce; "this many plane fds, no more."
/// - `MAX_SCM_RIGHTS_FDS = MAX_DMA_BUF_PLANES + 1 = 5` — the wire-level
///   cap that leaves a slot for an optional timeline-semaphore fd
///   alongside the plane fds.
///   Sentinel: `MAX_SCM_RIGHTS_FDS + 1 = 6` is "one over the cap."
#[test]
fn oversize_fd_vec_rejected() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    // MAX_SCM_RIGHTS_FDS + 1 fds — one over the wire-level cap.
    let fds: Vec<RawFd> = (0..=MAX_SCM_RIGHTS_FDS)
        .map(|i| make_memfd_with(format!("plane-{}", i).as_bytes()))
        .collect();

    let check_in_req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 640,
        "height": 480,
        "format": "Custom",
        "plane_sizes": vec![0u64; fds.len()],
        "plane_offsets": vec![0u64; fds.len()],
    });

    // The wire helper rejects with InvalidInput *before* any syscall,
    // without closing the caller-owned fds.
    let err = send_request_with_fds(&stream, &check_in_req, &fds, 0)
        .expect_err("oversize vec must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    // Every caller fd is still valid; no leaks, no double-closes.
    for fd in &fds {
        let rc = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        assert!(rc >= 0, "caller fd {} must still be valid", fd);
        unsafe { libc::close(*fd) };
    }

    drop(stream);
    service.stop();
}

/// Positive-case sibling of [`oversize_fd_vec_rejected`] — the wire
/// helper MUST accept exactly [`MAX_SCM_RIGHTS_FDS`] fds. Locks in
/// the high edge of the cap so a future cap shift fails loudly on
/// whichever edge actually moved (without this, only the
/// over-the-cap edge is regression-tested and a cap *increase* could
/// silently widen the wire surface).
#[test]
fn at_cap_fd_vec_accepted_by_wire_helper() {
    // The wire helper itself is what we're gating — no daemon is
    // needed beyond a connected socket so the helper's syscall path
    // can run. Spinning up a real service-and-handler combo would
    // also work but introduces ordering noise; the unit-level helper
    // gate is the narrowest test.
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    // Exactly MAX_SCM_RIGHTS_FDS fds — the upper edge of the cap.
    // Send-only: we don't care whether the daemon parses the JSON
    // payload (it won't — `Custom` is a sentinel format string), only
    // that the wire helper itself doesn't reject for fd-count.
    let fds: Vec<RawFd> = (0..MAX_SCM_RIGHTS_FDS)
        .map(|i| make_memfd_with(format!("at-cap-{}", i).as_bytes()))
        .collect();
    assert_eq!(fds.len(), MAX_SCM_RIGHTS_FDS);

    let req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime-at-cap",
        "width": 640,
        "height": 480,
        "format": "Custom",
        "plane_sizes": vec![0u64; fds.len()],
        "plane_offsets": vec![0u64; fds.len()],
    });

    // The wire helper must NOT reject for fd-count. The daemon may
    // still error on payload parsing — we don't gate on the response,
    // only on the helper's own input-validation step. A successful
    // send (regardless of subsequent response shape) proves the cap
    // edge is accepted.
    let send_result = send_message_with_fds(&stream, req.to_string().as_bytes(), &fds);
    assert!(
        send_result.is_ok(),
        "wire helper must accept exactly MAX_SCM_RIGHTS_FDS={} fds; got {:?}",
        MAX_SCM_RIGHTS_FDS,
        send_result
    );

    // Caller fds are independent of what the daemon does — close them.
    for fd in &fds {
        let rc = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        assert!(rc >= 0, "caller fd {} must still be valid", fd);
        unsafe { libc::close(*fd) };
    }

    drop(stream);
    service.stop();
}
