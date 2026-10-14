// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

const ADOPTED_WIDTH: u32 = 64;
const ADOPTED_HEIGHT: u32 = 16;
const ADOPTED_BYTE_SIZE: u64 = 64 * 4 * 16;

fn exchange_client_on(share: &SurfaceShareUnderTest) -> Arc<HelperProcessGpuExchangeClient> {
    Python::initialize();
    Python::attach(|python| {
        Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            share.socket_path.clone(),
            "helper:adoption-under-test".to_string(),
        ))
    })
}

/// A sized memfd standing in for a foreign DMA-BUF.
fn a_foreign_memory_fd() -> OwnedFd {
    let raw_fd =
        unsafe { libc::memfd_create(c"adopted-foreign-plane".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw_fd >= 0, "memfd_create failed");
    // SAFETY: a fresh fd memfd_create just returned; ours alone.
    let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let sized = unsafe { libc::ftruncate(owned.as_raw_fd(), ADOPTED_BYTE_SIZE as i64) };
    assert_eq!(sized, 0, "ftruncate failed");
    owned
}

fn check_in_request() -> serde_json::Value {
    serde_json::json!({
        "op": "check_in",
        "runtime_id": "helper:adoption-under-test",
        "width": ADOPTED_WIDTH,
        "height": ADOPTED_HEIGHT,
        "format": "bgra32",
        "resource_type": "pixel_buffer",
        "handle_type": "dma_buf",
        "plane_sizes": [ADOPTED_BYTE_SIZE],
        "plane_offsets": [0u64],
        "plane_strides": [ADOPTED_BYTE_SIZE / u64::from(ADOPTED_HEIGHT)],
    })
}

/// The fd crosses, the service mints a resolvable id, and the checkout
/// hands the plane back with the geometry the adoption declared.
#[test]
fn an_adopted_foreign_fd_is_resolvable_with_its_declared_geometry() {
    let share = SurfaceShareUnderTest::start("adoption");
    let exchange_client = exchange_client_on(&share);
    let foreign_fd = a_foreign_memory_fd();

    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in_request(), &[foreign_fd.as_raw_fd()])
        .expect("the check_in round trip completes");
    let adopted_surface_id = check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .expect("check_in answers with a surface_id")
        .to_string();
    // The original stays the caller's — the kernel and the service each
    // dup'd it on the way over.
    drop(foreign_fd);

    let (check_out_response, received_fds) = exchange_client
        .check_out_surface(&adopted_surface_id)
        .expect("the adopted surface checks out");
    assert!(
        check_out_response.get("error").is_none(),
        "the checkout must not refuse: {check_out_response}"
    );
    assert_eq!(
        check_out_response.get("width").and_then(|v| v.as_u64()),
        Some(u64::from(ADOPTED_WIDTH)),
    );
    assert_eq!(
        check_out_response.get("height").and_then(|v| v.as_u64()),
        Some(u64::from(ADOPTED_HEIGHT)),
    );
    assert_eq!(
        check_out_response.get("format").and_then(|v| v.as_str()),
        Some("bgra32"),
    );
    assert_eq!(
        check_out_response
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .map(|sizes| sizes.iter().filter_map(|s| s.as_u64()).collect::<Vec<_>>()),
        Some(vec![ADOPTED_BYTE_SIZE]),
    );
    assert_eq!(
        received_fds.len(),
        1,
        "one adopted plane crosses back as one fd"
    );
    drop(received_fds);
    let _ = exchange_client.release_check_out(&adopted_surface_id);
}

/// The unregister debt's op removes the registration: the id stops
/// resolving, and a second release reports nothing left to remove.
#[test]
fn releasing_an_adoption_removes_the_registration() {
    let share = SurfaceShareUnderTest::start("adoption-release");
    let exchange_client = exchange_client_on(&share);
    let foreign_fd = a_foreign_memory_fd();

    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in_request(), &[foreign_fd.as_raw_fd()])
        .expect("the check_in round trip completes");
    let adopted_surface_id = check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .expect("check_in answers with a surface_id")
        .to_string();

    exchange_client
        .unregister_foreign_surface(&adopted_surface_id)
        .expect("the adoption unregisters");

    let (after_release, _no_fds_after) = exchange_client
        .check_out_surface(&adopted_surface_id)
        .expect("the socket round trip still completes");
    assert!(
        after_release.get("error").is_some(),
        "a released adoption must stop resolving: {after_release}"
    );
    let second_release = exchange_client.unregister_foreign_surface(&adopted_surface_id);
    assert!(
        second_release.is_err(),
        "a second release must report nothing left to remove"
    );
}

/// Register `plane_count` memfd planes under `handle_type` with the extra
/// registration fields `extra_fields`, and return the minted surface id.
///
/// No Vulkan runs on these paths: every refusal below fires before the
/// helper creates its import device, so they hold on a box with no GPU.
fn a_pixel_registration_of(
    exchange_client: &HelperProcessGpuExchangeClient,
    handle_type: &str,
    plane_count: usize,
    extra_fields: serde_json::Value,
) -> String {
    let backing_fds: Vec<OwnedFd> = (0..plane_count).map(|_| a_foreign_memory_fd()).collect();
    let mut check_in = check_in_request();
    check_in["handle_type"] = handle_type.into();
    check_in["plane_sizes"] = vec![ADOPTED_BYTE_SIZE; plane_count].into();
    check_in["plane_offsets"] = vec![0u64; plane_count].into();
    check_in["plane_strides"] =
        vec![ADOPTED_BYTE_SIZE / u64::from(ADOPTED_HEIGHT); plane_count].into();
    for (field, value) in extra_fields.as_object().into_iter().flatten() {
        check_in[field] = value.clone();
    }
    let raw_backing_fds: Vec<_> = backing_fds.iter().map(|fd| fd.as_raw_fd()).collect();
    let (check_in_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(&check_in, &raw_backing_fds)
        .expect("the check_in round trip completes");
    check_in_response
        .get("surface_id")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("check_in answers with a surface_id: {check_in_response}"))
        .to_string()
}

/// Every pooled frame is one RGBA allocation, so a multi-plane OPAQUE_FD
/// pixel buffer has no producer; it is refused naming the surface and the
/// plane count rather than imported as its first plane.
#[test]
fn a_multi_plane_opaque_fd_pixel_buffer_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("opaque-pixel-multi-plane");
    let exchange_client = exchange_client_on(&share);
    let surface_id = a_pixel_registration_of(
        &exchange_client,
        "opaque_fd",
        2,
        serde_json::json!({ "vk_memory_type_index": 3u32 }),
    );

    let refusal = exchange_client
        .check_out_and_import(&surface_id)
        .err()
        .expect("a two-plane opaque_fd pixel buffer must be refused")
        .to_string();
    assert!(
        refusal.contains(&format!("{surface_id:?}")),
        "the refusal must name the surface: {refusal:?}"
    );
    assert!(
        refusal.contains("opaque_fd") && refusal.contains("2 planes"),
        "the refusal must name the flavour and the plane count: {refusal:?}"
    );
}

/// A single-plane OPAQUE_FD pixel buffer imports at the memory type index
/// its exporter allocated from; a registration that does not state one is
/// refused naming the field rather than imported at a guessed index.
#[test]
fn an_opaque_fd_pixel_buffer_without_a_memory_type_index_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("opaque-pixel-no-index");
    let exchange_client = exchange_client_on(&share);
    let surface_id =
        a_pixel_registration_of(&exchange_client, "opaque_fd", 1, serde_json::json!({}));

    let refusal = exchange_client
        .check_out_and_import(&surface_id)
        .err()
        .expect("an opaque_fd pixel buffer without its memory type index must be refused")
        .to_string();
    assert!(
        refusal.contains(&format!("{surface_id:?}")),
        "the refusal must name the surface: {refusal:?}"
    );
    assert!(
        refusal.contains("vk_memory_type_index"),
        "the refusal must name the missing field: {refusal:?}"
    );
}

#[test]
fn a_pixel_buffer_of_an_unknown_handle_type_is_refused_by_name() {
    let share = SurfaceShareUnderTest::start("unknown-pixel-handle");
    let exchange_client = exchange_client_on(&share);
    let surface_id = a_pixel_registration_of(
        &exchange_client,
        "win32_kmt",
        1,
        serde_json::json!({ "vk_memory_type_index": 3u32 }),
    );

    let refusal = exchange_client
        .check_out_and_import(&surface_id)
        .err()
        .expect("a pixel buffer of a handle type this consumer does not know must be refused")
        .to_string();
    assert!(
        refusal.contains(&format!("{surface_id:?}")) && refusal.contains("\"win32_kmt\""),
        "the refusal must name the surface and the handle type: {refusal:?}"
    );
}
