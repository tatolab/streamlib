// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess fixture for the surface-share EPOLLHUP watchdog integration test.
//!
//! Connects to the surface-share Unix socket, `check_in`s a memfd-backed
//! surface, prints `SURFACE_ID=<uuid>` on stdout, then sleeps until SIGKILL.
//! Communicates with the parent via stdout because tracing would route through
//! the host's logging pathway, which the test doesn't want to interleave.

#![cfg(target_os = "linux")]
#![allow(clippy::disallowed_macros)]

use std::io::Write;
use std::os::unix::io::RawFd;

use streamlib_surface_client::{connect_to_surface_share_socket, send_request_with_fds};

fn make_memfd_with(contents: &[u8]) -> RawFd {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    let name = std::ffi::CString::new("streamlib-surface-share-crash-helper").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(fd >= 0, "memfd_create failed: {}", std::io::Error::last_os_error());
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(contents).expect("memfd write");
    file.seek(SeekFrom::Start(0)).expect("memfd rewind");
    file.into_raw_fd()
}

fn main() {
    let socket_path = std::env::var_os("STREAMLIB_SURFACE_SOCKET")
        .map(std::path::PathBuf::from)
        .expect("STREAMLIB_SURFACE_SOCKET must be set");
    let runtime_id = std::env::var("STREAMLIB_RUNTIME_ID")
        .expect("STREAMLIB_RUNTIME_ID must be set");

    let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
    let send_fd = make_memfd_with(b"crash-helper-fixture-payload");

    let req = serde_json::json!({
        "op": "check_in",
        "runtime_id": runtime_id,
        "width": 32,
        "height": 32,
        "format": "Bgra32",
        "resource_type": "pixel_buffer",
    });
    let (resp, _) =
        send_request_with_fds(&stream, &req, &[send_fd], 0).expect("check_in request");
    unsafe { libc::close(send_fd) };
    let surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in check_in response")
        .to_string();

    println!("SURFACE_ID={}", surface_id);
    std::io::stdout().flush().ok();

    // Hold the connection open and idle until the parent SIGKILLs us.
    // 60 s is generous — the harness kills well within a fraction of that.
    std::thread::sleep(std::time::Duration::from_secs(60));
}
