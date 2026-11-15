// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface-share service's tests, split by request kind, and the fixtures they share.

use super::*;
use crate::core::context::SurfaceCheckOutLeaseRegistry;
use crate::core::context::surface_share_wire_verbs::release_every_surface_registered_by;
use crate::linux::surface_share::ViewedFrameSource;
use std::os::unix::io::FromRawFd;
use streamlib_surface_client::{connect_to_surface_share_socket, send_request_with_fds};
use tempfile::TempDir;

mod check_in_check_out_and_lease_tests;
mod fd_and_plane_wire_tests;
mod pool_slot_claim_table_and_viewed_frame_tests;
mod registration_descriptor_field_tests;

/// `VkImageTiling::DRM_FORMAT_MODIFIER_EXT` as the wire carries it.
/// `src/linux/` deliberately takes no vulkanalia dependency, so the raw
/// value is spelled here rather than read off `vk::ImageTiling`.
const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i64 = 1_000_158_000;

fn make_memfd_with(contents: &[u8]) -> RawFd {
    use std::io::{Seek, SeekFrom, Write};

    let name = std::ffi::CString::new("streamlib-runtime-surface-share-test").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(
        fd >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(contents).expect("memfd write");
    file.seek(SeekFrom::Start(0)).expect("memfd rewind");
    use std::os::unix::io::IntoRawFd;
    file.into_raw_fd()
}

fn read_all_from_fd(fd: RawFd) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.seek(SeekFrom::Start(0)).expect("recv memfd rewind");
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("recv memfd read");
    buf
}

/// A socket path in a directory of its own. `sun_path` is 108 bytes and a
/// unique name in a shared temp dir spends most of that budget before
/// `TMPDIR` is even accounted for; a private dir keeps the path short and
/// unlinks it on drop. The returned [`TempDir`] owns that lifetime.
fn tmp_socket_path() -> (TempDir, PathBuf) {
    let dir = crate::core::test_support::a_temporary_directory_at_owner_only_mode()
        .expect("temp dir for test socket");
    let path = dir.path().join("surface-share.sock");
    (dir, path)
}

/// Register a one-plane surface over the wire and return the id it lives
/// under.
fn check_in_one_surface(stream: &UnixStream) -> String {
    let send_fd = make_memfd_with(b"surface-check-out-lease-test");
    let (response, _no_reply_fds) = send_request_with_fds(
        stream,
        &serde_json::json!({
            "op": "check_in",
            "runtime_id": "lease-test-runtime",
            "width": 64,
            "height": 64,
            "format": "bgra32",
            "resource_type": "pixel_buffer",
        }),
        &[send_fd],
        0,
    )
    .expect("check_in request");
    unsafe { libc::close(send_fd) };
    response
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in the check_in response")
        .to_string()
}

fn close_every_fd(fds: &[RawFd]) {
    for fd in fds {
        unsafe { libc::close(*fd) };
    }
}

fn ask(stream: &UnixStream, request: &serde_json::Value) -> serde_json::Value {
    let (response, reply_fds) =
        send_request_with_fds(stream, request, &[], MAX_DMA_BUF_PLANES).expect("wire request");
    close_every_fd(&reply_fds);
    response
}

fn started_service(state: SurfaceShareState) -> (TempDir, PathBuf, UnixSocketSurfaceService) {
    let (socket_dir, socket_path) = tmp_socket_path();
    let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
    service.start().expect("service start");
    std::thread::sleep(std::time::Duration::from_millis(50));
    (socket_dir, socket_path, service)
}

/// The reclaim runs on the connection's own thread once its read loop
/// ends, so the drop is observed rather than assumed.
fn lease_count_settling_to(
    check_out_leases: &SurfaceCheckOutLeaseRegistry,
    surface_id: &str,
    expected: u32,
) -> u32 {
    for _ in 0..200 {
        let outstanding = check_out_leases
            .outstanding_check_out_count(surface_id)
            .expect("the lease table stays readable");
        if outstanding == expected {
            return outstanding;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    check_out_leases
        .outstanding_check_out_count(surface_id)
        .expect("the lease table stays readable")
}
