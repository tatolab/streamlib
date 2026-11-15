// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;

/// The id the pool registers a slot under; a texture lookup of it is
/// refused because the slot is a pixel buffer, never a texture.
const PIXEL_BUFFER_SLOT_ID: &str = "fd-ownership-test-pool-slot";

/// How many descriptors of this process name the memfd `plane_name` —
/// the registration's own copy plus whatever a lookup left behind.
/// Counting by name keeps the measure immune to the other tests
/// opening and closing descriptors in the same process.
fn open_descriptors_of_the_plane(plane_name: &str) -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd is readable")
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter(|target| target.to_string_lossy().contains(plane_name))
        .count()
}

/// The service thread runs in this process too, and it closes its dup
/// of the plane only after `sendmsg` has already delivered a copy to
/// the lookup, so a count taken the instant a reply lands can include
/// a copy that is about to close. A leak never settles back; a dup in
/// flight does within microseconds.
fn open_descriptors_of_the_plane_once_the_service_thread_settled(
    plane_name: &str,
    expected: usize,
) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let count = open_descriptors_of_the_plane(plane_name);
        if count == expected || std::time::Instant::now() >= deadline {
            return count;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn memfd_named(plane_name: &str, bytes: &[u8]) -> RawFd {
    use std::io::Write;
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    let name = std::ffi::CString::new(plane_name).unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(bytes).expect("memfd write");
    file.into_raw_fd()
}

/// A live service with one pixel-buffer slot registered exactly the way
/// the pixel-buffer pool registers its slots — `format` spelled in the
/// pixel-buffer vocabulary, one plane — and a connected store.
fn store_against_a_service_holding_one_pixel_buffer_slot(
    plane_name: &str,
) -> (
    tempfile::TempDir,
    UnixSocketSurfaceService,
    UnixStream,
    SurfaceStore,
) {
    let socket_dir = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
        .expect("temp dir for the test socket");
    let socket_path = socket_dir.path().join("surface-share.sock");
    let mut service = UnixSocketSurfaceService::new(SurfaceShareState::new(), socket_path.clone());
    service.start().expect("service start");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !socket_path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let registering_connection = UnixStream::connect(&socket_path).expect("connect to register");
    let plane = memfd_named(plane_name, &[0u8; 64]);
    let (response, no_reply_fds) = streamlib_surface_client::send_request_with_fds(
        &registering_connection,
        &serde_json::json!({
            "op": "register",
            "surface_id": PIXEL_BUFFER_SLOT_ID,
            "runtime_id": "fd-ownership-test-runtime",
            "width": 4,
            "height": 4,
            "format": crate::core::rhi::PixelFormat::Rgba32.wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            "handle_type": "dma_buf",
            "plane_sizes": [64],
            "plane_offsets": [0],
            "plane_strides": [16],
        }),
        &[plane],
        0,
    )
    .expect("register request");
    unsafe { libc::close(plane) };
    assert!(no_reply_fds.is_empty());
    assert!(
        response.get("error").is_none(),
        "registration refused: {response}"
    );

    let store = SurfaceStore::new(
        socket_path.to_string_lossy().into_owned(),
        "fd-ownership-test-runtime".to_string(),
    );
    store.connect().expect("store connects to the service");
    (socket_dir, service, registering_connection, store)
}

#[test]
fn a_texture_lookup_of_a_pixel_buffer_slot_is_refused_and_closes_the_plane_it_received() {
    const PLANE: &str = "fd-ownership-test-plane-for-texture-lookup";
    let (_socket_dir, mut service, _registering_connection, store) =
        store_against_a_service_holding_one_pixel_buffer_slot(PLANE);

    let descriptors_before_any_lookup = open_descriptors_of_the_plane(PLANE);
    for _ in 0..9 {
        assert!(
            store.lookup_texture(PIXEL_BUFFER_SLOT_ID).is_err(),
            "a pixel-buffer slot is not a texture"
        );
    }
    assert_eq!(
        open_descriptors_of_the_plane_once_the_service_thread_settled(
            PLANE,
            descriptors_before_any_lookup
        ),
        descriptors_before_any_lookup,
        "every refused texture lookup must close the plane fd the service sent with its reply"
    );
    service.stop();
}

/// Without an import device the lookup is refused before any Vulkan
/// call, so on a machine with a GPU the test brings one up itself;
/// otherwise which refusal it proves would depend on whether an earlier
/// test in the binary happened to.
#[test]
fn a_buffer_lookup_that_cannot_import_closes_the_planes_it_received() {
    const PLANE: &str = "fd-ownership-test-plane-for-buffer-lookup";
    let import_device_is_up = crate::vulkan::rhi::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
        .get()
        .is_some()
        || crate::core::rhi::GpuDevice::new().is_ok();
    if !import_device_is_up {
        tracing::warn!(
            "no Vulkan device — this run proves the no-device refusal, not the DMA-BUF check"
        );
    }
    let (_socket_dir, mut service, _registering_connection, store) =
        store_against_a_service_holding_one_pixel_buffer_slot(PLANE);

    let descriptors_before_any_lookup = open_descriptors_of_the_plane(PLANE);
    for _ in 0..9 {
        let Err(refusal) = store.lookup_buffer(PIXEL_BUFFER_SLOT_ID) else {
            panic!("a memfd is not a DMA-BUF, so the import must refuse");
        };
        if import_device_is_up {
            assert!(
                refusal.to_string().contains("is not a DMA-BUF"),
                "with an import device up, the refusal must come before the driver call: {refusal}"
            );
        }
    }
    assert_eq!(
        open_descriptors_of_the_plane_once_the_service_thread_settled(
            PLANE,
            descriptors_before_any_lookup
        ),
        descriptors_before_any_lookup,
        "every failed buffer lookup must close the plane fds the service sent with its reply"
    );
    service.stop();
}
