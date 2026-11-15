// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Checking surfaces in and out, the leases a check-out takes, and what a connection releases when it closes.

use super::*;

/// A checkout claims the frame; a lookup does not. Both hand back the
/// same planes, but a lookup is the host reading its own table and holds
/// nothing — leasing it would pin a pool slot no consumer is reading.
#[test]
fn a_check_out_claims_the_frame_and_a_lookup_leaves_it_free() {
    let state = SurfaceShareState::new();
    let check_out_leases = Arc::clone(state.check_out_leases());
    let (_socket_dir, socket_path, mut service) = started_service(state);

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
    let surface_id = check_in_one_surface(&stream);

    ask(
        &stream,
        &serde_json::json!({"op": "lookup", "surface_id": surface_id}),
    );
    assert_eq!(
        check_out_leases
            .outstanding_check_out_count(&surface_id)
            .unwrap(),
        0,
        "a lookup holds nothing, so it must pin no pool slot"
    );

    ask(
        &stream,
        &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
    );
    assert_eq!(
        check_out_leases
            .outstanding_check_out_count(&surface_id)
            .unwrap(),
        1,
        "a checkout claims the frame against producer reuse"
    );

    drop(stream);
    service.stop();
}

/// The explicit release, and the rule that one connection can never unpin
/// another's frame.
#[test]
fn release_check_out_frees_only_the_connection_that_took_it() {
    let state = SurfaceShareState::new();
    let check_out_leases = Arc::clone(state.check_out_leases());
    let (_socket_dir, socket_path, mut service) = started_service(state);

    let holder = connect_to_surface_share_socket(&socket_path).expect("connect the holder");
    let surface_id = check_in_one_surface(&holder);
    ask(
        &holder,
        &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
    );

    let bystander = connect_to_surface_share_socket(&socket_path).expect("connect the bystander");
    let bystander_release = ask(
        &bystander,
        &serde_json::json!({"op": "release_check_out", "surface_id": surface_id}),
    );
    assert_eq!(
        bystander_release.get("released").and_then(|v| v.as_bool()),
        Some(false),
        "a connection that never checked the surface out has nothing to release"
    );
    assert_eq!(
        check_out_leases
            .outstanding_check_out_count(&surface_id)
            .unwrap(),
        1,
        "one connection must never be able to unpin another's frame"
    );

    let holder_release = ask(
        &holder,
        &serde_json::json!({"op": "release_check_out", "surface_id": surface_id}),
    );
    assert_eq!(
        holder_release.get("released").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        check_out_leases
            .outstanding_check_out_count(&surface_id)
            .unwrap(),
        0,
        "the frame returns to its producer once its reader lets go"
    );

    drop(holder);
    drop(bystander);
    service.stop();
}

/// The backstop: a child killed mid-frame never sends a release, so the
/// kernel closing its socket is what returns the slots. Unlike the
/// registration watchdog this is not gated on the peer being out of
/// process — a lease may never outlive its reader, whoever the reader is.
#[test]
fn a_dropped_connection_releases_every_lease_it_was_holding() {
    let state = SurfaceShareState::new();
    let check_out_leases = Arc::clone(state.check_out_leases());
    let (_socket_dir, socket_path, mut service) = started_service(state);

    let publisher = connect_to_surface_share_socket(&socket_path).expect("connect the publisher");
    let surface_id = check_in_one_surface(&publisher);

    let reader = connect_to_surface_share_socket(&socket_path).expect("connect the reader");
    for _ in 0..2 {
        ask(
            &reader,
            &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
        );
    }
    assert_eq!(
        check_out_leases
            .outstanding_check_out_count(&surface_id)
            .unwrap(),
        2
    );

    drop(reader);
    assert_eq!(
        lease_count_settling_to(&check_out_leases, &surface_id, 0),
        0,
        "a reader that died holding frames must not pin their slots forever"
    );

    drop(publisher);
    service.stop();
}

#[test]
fn check_in_check_out_roundtrip_preserves_fd_content() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");

    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let pattern = b"streamlib-runtime-surface-share-test-fd-contents-0123456789";
    let send_fd = make_memfd_with(pattern);

    let check_in_req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 640,
        "height": 480,
        "format": "bgra32",
        "resource_type": "pixel_buffer",
    });
    let (check_in_resp, check_in_fds) =
        send_request_with_fds(&stream, &check_in_req, &[send_fd], 0).expect("check_in request");
    unsafe { libc::close(send_fd) };
    assert!(check_in_fds.is_empty(), "check_in must not return an fd");
    let surface_id = check_in_resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in response")
        .to_string();
    assert!(!surface_id.is_empty());

    let check_out_req = serde_json::json!({
        "op": "check_out",
        "surface_id": surface_id,
    });
    let (check_out_resp, check_out_fds) =
        send_request_with_fds(&stream, &check_out_req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out request");
    assert_eq!(
        check_out_resp.get("width").and_then(|v| v.as_u64()),
        Some(640)
    );
    assert_eq!(
        check_out_resp.get("height").and_then(|v| v.as_u64()),
        Some(480)
    );
    assert_eq!(
        check_out_resp.get("format").and_then(|v| v.as_str()),
        Some("bgra32")
    );
    assert_eq!(check_out_fds.len(), 1, "single-plane: exactly one fd");
    let received_fd = check_out_fds[0];
    assert!(received_fd >= 0);
    let received = read_all_from_fd(received_fd);
    assert_eq!(received, pattern);

    let release_req = serde_json::json!({
        "op": "release",
        "surface_id": surface_id,
        "runtime_id": "test-runtime",
    });
    let _ = send_request_with_fds(&stream, &release_req, &[], 0).expect("release request");

    drop(stream);
    service.stop();
}

#[test]
fn check_out_unknown_surface_id_returns_error_no_fd() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
    let req = serde_json::json!({
        "op": "check_out",
        "surface_id": "never-registered",
    });
    let (resp, fds) =
        send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES).expect("check_out request");
    assert!(fds.is_empty());
    assert!(resp.get("error").and_then(|v| v.as_str()).is_some());

    drop(stream);
    service.stop();
}

/// Two memfds with distinct content registered under one surface_id via
/// `check_in` must round-trip intact through `check_out`, including the
/// plane-layout metadata that lets the consumer mmap each plane at its
/// own size. This is the defining multi-plane DMA-BUF test.
#[test]
fn check_in_check_out_multi_fd_roundtrip() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

    let pattern_y = b"plane-Y-bytes-A123456789";
    let pattern_uv = b"plane-UV-bytes-Z987654321";
    let fd_y = make_memfd_with(pattern_y);
    let fd_uv = make_memfd_with(pattern_uv);

    let check_in_req = serde_json::json!({
        "op": "check_in",
        "runtime_id": "test-runtime",
        "width": 1920,
        "height": 1080,
        "format": "nv12_video_range",
        "resource_type": "pixel_buffer",
        "plane_sizes": [pattern_y.len() as u64, pattern_uv.len() as u64],
        "plane_offsets": [0u64, 0u64],
        "plane_strides": [1920u64, 1920u64],
        "drm_format_modifier": 0u64,
    });
    let (check_in_resp, check_in_fds) =
        send_request_with_fds(&stream, &check_in_req, &[fd_y, fd_uv], 0).expect("check_in request");
    unsafe {
        libc::close(fd_y);
        libc::close(fd_uv);
    }
    assert!(check_in_fds.is_empty(), "check_in must not return any fd");
    let surface_id = check_in_resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in response")
        .to_string();

    let check_out_req = serde_json::json!({
        "op": "check_out",
        "surface_id": surface_id,
    });
    let (check_out_resp, check_out_fds) =
        send_request_with_fds(&stream, &check_out_req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out request");
    assert_eq!(
        check_out_fds.len(),
        2,
        "both planes delivered via SCM_RIGHTS"
    );
    assert_eq!(
        read_all_from_fd(check_out_fds[0]),
        pattern_y,
        "plane 0 content preserved"
    );
    assert_eq!(
        read_all_from_fd(check_out_fds[1]),
        pattern_uv,
        "plane 1 content preserved"
    );
    let sizes = check_out_resp
        .get("plane_sizes")
        .and_then(|v| v.as_array())
        .expect("plane_sizes array in response");
    assert_eq!(
        sizes
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![pattern_y.len() as u64, pattern_uv.len() as u64],
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

/// A helper connects lazily on its first claim, and the claim is what
/// keeps a shallow output pool from recycling the frame it names — so
/// time spent before the service accepts is time the frame can vanish in.
#[test]
fn a_new_connections_first_request_is_answered_without_waiting_on_the_accept_loop() {
    const FRESH_CONNECTIONS: u32 = 40;
    // Forty accepts that each wait on a 50 ms nap take about two seconds;
    // forty prompt accepts take a few ms, leaving room for a loaded CI
    // worker.
    const ALL_FRESH_CONNECTIONS_ANSWERED_WITHIN: std::time::Duration =
        std::time::Duration::from_millis(250);
    let (_socket_dir, socket_path, mut service) = started_service(SurfaceShareState::new());

    let started_at = std::time::Instant::now();
    for connection_index in 0..FRESH_CONNECTIONS {
        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let response = ask(
            &stream,
            &serde_json::json!({
                "op": "check_out",
                "surface_id": format!("no-such-surface-{connection_index}"),
            }),
        );
        assert!(
            response.get("error").is_some(),
            "an unknown id is refused, got {response}"
        );
    }
    let elapsed = started_at.elapsed();

    assert!(
        elapsed < ALL_FRESH_CONNECTIONS_ANSWERED_WITHIN,
        "{FRESH_CONNECTIONS} fresh connections took {elapsed:?} to be answered; the accept \
         loop is making each new connection wait"
    );
    service.stop();
}

/// Same-process wire connections (the host runtime opening a diagnostic
/// connection to its own surface-share socket to publish surfaces) must NOT
/// trigger the watchdog. SO_PEERCRED reports the same pid, so the disconnect
/// classification rejects it as a subprocess and cleanup is skipped.
/// Wire-level coverage of the actual subprocess-crash path lives in the
/// `surface_share_subprocess_crash` integration test.
#[test]
fn same_process_disconnect_does_not_trigger_watchdog() {
    let state = SurfaceShareState::new();
    let (_socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
    let runtime_id = "host-publishes-then-disconnects";
    let send_fd = make_memfd_with(b"host-published-fixture");
    let (resp, _) = send_request_with_fds(
        &stream,
        &serde_json::json!({
            "op": "check_in",
            "runtime_id": runtime_id,
            "width": 16,
            "height": 16,
            "format": "bgra32",
            "resource_type": "pixel_buffer",
        }),
        &[send_fd],
        0,
    )
    .expect("check_in request");
    unsafe { libc::close(send_fd) };
    let surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id")
        .to_string();
    assert_eq!(state.surface_ids_by_runtime(runtime_id).len(), 1);

    drop(stream);

    // Give the per-connection thread plenty of time to observe EOF and,
    // if the watchdog were to misfire, run cleanup. The surface MUST
    // survive: same-process publishers intentionally disconnect.
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        state.surface_ids_by_runtime(runtime_id),
        vec![surface_id],
        "same-process disconnect must not release host-published surfaces",
    );

    service.stop();
}

/// Watchdog primitive (pure-function): given a state populated with
/// surfaces under multiple runtime_ids, `release_every_surface_registered_by`
/// releases only the targeted runtime's surfaces and is idempotent on
/// second call.
#[test]
fn release_every_surface_registered_by_is_scoped_and_idempotent() {
    let state = SurfaceShareState::new();
    // Use real memfds so release_surface's libc::close calls operate on
    // valid fds (no fd-table corruption from -1 sentinels).
    let mk = |label: &str| -> Vec<RawFd> {
        let name = std::ffi::CString::new(label).unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(fd >= 0);
        vec![fd]
    };
    for (sid, rid, label) in [
        ("victim-1", "victim-runtime", "v1"),
        ("victim-2", "victim-runtime", "v2"),
        ("survivor", "survivor-runtime", "s"),
    ] {
        state
            .register_surface(SurfaceRegistration {
                surface_id: sid,
                runtime_id: rid,
                dma_buf_fds: mk(label),
                plane_sizes: vec![0],
                plane_offsets: vec![0],
                plane_strides: vec![0],
                width: 1,
                height: 1,
                format: "bgra32",
                resource_type: "pixel_buffer",
                handle_type: "dma_buf",
                drm_format_modifier: 0,
                produce_done_fd: None,
                consume_done_fd: None,
                current_image_layout: 0,
                vk_image_type: VK_IMAGE_TYPE_DEFAULT,
                vk_image_mip_levels: VK_IMAGE_MIP_LEVELS_DEFAULT,
                vk_image_array_layers: VK_IMAGE_ARRAY_LAYERS_DEFAULT,
                vk_image_samples: VK_IMAGE_SAMPLES_DEFAULT,
                vk_image_tiling: VK_IMAGE_TILING_DEFAULT,
                vk_image_usage: VK_IMAGE_USAGE_DEFAULT,
                vk_image_allocation_size: VK_IMAGE_ALLOCATION_SIZE_DEFAULT,
                vk_memory_type_index: None,
                exporting_device_uuid: None,
                tensor_layout: None,
                pixel_planes_within_one_allocation: None,
            })
            .expect("register");
    }

    release_every_surface_registered_by(&state, "victim-runtime");
    assert!(state.surface_ids_by_runtime("victim-runtime").is_empty());
    assert_eq!(
        state.surface_ids_by_runtime("survivor-runtime"),
        vec!["survivor".to_string()]
    );

    // Idempotent second call: nothing left for the victim, survivor
    // unaffected. Nothing panics.
    release_every_surface_registered_by(&state, "victim-runtime");
    assert_eq!(
        state.surface_ids_by_runtime("survivor-runtime"),
        vec!["survivor".to_string()]
    );

    // Cleanup of a runtime with no registrations is a no-op.
    release_every_surface_registered_by(&state, "never-registered");
    assert_eq!(
        state.surface_ids_by_runtime("survivor-runtime"),
        vec!["survivor".to_string()]
    );
}
