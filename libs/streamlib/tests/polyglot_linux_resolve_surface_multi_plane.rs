// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Multi-plane DMA-BUF round-trip through the polyglot consumer shims.
//!
//! This is the shim-level exit-criterion test for the multi-FD SCM_RIGHTS
//! widening (#423). A real `StreamRuntime` runs the surface-sharing service;
//! the host side `check_in`s a 2-plane surface built from two memfds seeded
//! with distinct byte patterns; the shim's `*_broker_resolve_surface`
//! resolves the surface_id, and `*_gpu_surface_plane_mmap` plus
//! `*_gpu_surface_plane_base_address` expose each plane's bytes so the test
//! can assert the patterns survived the wire.
//!
//! memfds stand in for real DMA-BUFs here. The shim never maps them as
//! GPU memory (no Vulkan `lock` call in this test), so SCM_RIGHTS +
//! `mmap(MAP_SHARED)` is enough to verify the wire + per-plane layout.
//! The Vulkan `lock` path is covered by the existing
//! `polyglot_linux_check_out*` tests with real DMA-BUFs.
//!
//! Skip conditions:
//!   - `libstreamlib_{python,deno}_native.so` not under target/ → skip (as
//!     with the existing polyglot tests).
//!   - no Vulkan-capable device available to the shim → skip; the shim's
//!     resolve_surface gates on `BrokerVulkanDevice::try_new()` even for
//!     mmap-only consumers.

#![cfg(target_os = "linux")]

use std::ffi::{CString, c_void};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
use std::path::PathBuf;

use streamlib::core::runtime::StreamRuntime;
use streamlib_broker_client::{connect_to_broker, send_request_with_fds};

fn locate_native_lib(basename: &str) -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    for profile in &["debug", "release"] {
        let candidate = workspace.join("target").join(profile).join(basename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn make_memfd_with(name: &str, contents: &[u8]) -> RawFd {
    let name = CString::new(name).unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(
        fd >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(contents).expect("memfd write");
    file.seek(SeekFrom::Start(0)).expect("memfd rewind");
    file.into_raw_fd()
}

/// Shared bytes for each shim's test: the two planes (Y and UV for NV12).
/// Distinct byte patterns so a cross-plane swap is visible.
fn plane_patterns() -> (Vec<u8>, Vec<u8>) {
    let plane_y: Vec<u8> = (0..(64 * 4))
        .map(|i| (((i as u32) * 19 + 3) & 0xFF) as u8)
        .collect();
    let plane_uv: Vec<u8> = (0..(64 * 4))
        .map(|i| (((i as u32) * 31 + 137) & 0xFF) as u8)
        .collect();
    (plane_y, plane_uv)
}

/// Check_in 2 memfds as planes of a single surface, return the surface_id.
fn check_in_multi_plane(
    socket_path: &std::path::Path,
    runtime_id: &str,
    plane_y: &[u8],
    plane_uv: &[u8],
) -> String {
    let fd_y = make_memfd_with("multi-plane-test-y", plane_y);
    let fd_uv = make_memfd_with("multi-plane-test-uv", plane_uv);

    let stream = connect_to_broker(socket_path).expect("host connect");
    let req = serde_json::json!({
        "op": "check_in",
        "runtime_id": runtime_id,
        "width": 64u32,
        "height": 4u32,
        "format": "Nv12VideoRange",
        "resource_type": "pixel_buffer",
        "plane_sizes": [plane_y.len() as u64, plane_uv.len() as u64],
        "plane_offsets": [0u64, 0u64],
    });
    let (resp, resp_fds) =
        send_request_with_fds(&stream, &req, &[fd_y, fd_uv], 0).expect("host check_in");
    assert!(resp_fds.is_empty());
    unsafe {
        libc::close(fd_y);
        libc::close(fd_uv);
    }
    let surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id in check_in response")
        .to_string();
    drop(stream);
    surface_id
}

/// How the shim's `broker_connect` is called. The Python shim takes
/// `(socket_path, runtime_id)`; the Deno shim takes only `(socket_path)`.
/// The two cdylibs are ABI-independent, so we resolve each signature
/// exactly rather than gambling on x86_64 calling convention leniency.
enum ConnectFlavor {
    TwoArg, // Python: slpn_broker_connect(socket_path, runtime_id)
    OneArg, // Deno:   sldn_broker_connect(socket_path)
}

/// Shared test body: load `lib_path` (a `libstreamlib_*_native.so`), call
/// the per-shim FFI entry points (differing only by the `prefix`, e.g.
/// `slpn_` or `sldn_`), and assert both plane contents round-trip through
/// the shim intact.
fn run_shim_test(lib_path: PathBuf, prefix: &str, flavor: ConnectFlavor) {
    let runtime = StreamRuntime::new().expect("StreamRuntime::new");
    let socket_path = runtime.surface_socket_path().to_path_buf();
    let runtime_id = runtime.runtime_id().to_string();

    let (plane_y, plane_uv) = plane_patterns();
    let surface_id = check_in_multi_plane(&socket_path, &runtime_id, &plane_y, &plane_uv);

    let lib = unsafe { libloading::Library::new(&lib_path) }.expect("load native lib");

    let broker_disconnect: libloading::Symbol<unsafe extern "C" fn(*mut c_void)> =
        unsafe { lib.get(format!("{}broker_disconnect", prefix).as_bytes()) }
            .expect("broker_disconnect");
    let broker_resolve_surface: libloading::Symbol<
        unsafe extern "C" fn(*mut c_void, *const i8) -> *mut c_void,
    > = unsafe { lib.get(format!("{}broker_resolve_surface", prefix).as_bytes()) }
        .expect("broker_resolve_surface");
    let gpu_surface_release: libloading::Symbol<unsafe extern "C" fn(*mut c_void)> =
        unsafe { lib.get(format!("{}gpu_surface_release", prefix).as_bytes()) }
            .expect("gpu_surface_release");
    let gpu_surface_plane_count: libloading::Symbol<
        unsafe extern "C" fn(*const c_void) -> u32,
    > = unsafe { lib.get(format!("{}gpu_surface_plane_count", prefix).as_bytes()) }
        .expect("gpu_surface_plane_count");
    let gpu_surface_plane_size: libloading::Symbol<
        unsafe extern "C" fn(*const c_void, u32) -> u64,
    > = unsafe { lib.get(format!("{}gpu_surface_plane_size", prefix).as_bytes()) }
        .expect("gpu_surface_plane_size");
    let gpu_surface_plane_mmap: libloading::Symbol<
        unsafe extern "C" fn(*mut c_void, u32) -> i32,
    > = unsafe { lib.get(format!("{}gpu_surface_plane_mmap", prefix).as_bytes()) }
        .expect("gpu_surface_plane_mmap");
    let gpu_surface_plane_base_address: libloading::Symbol<
        unsafe extern "C" fn(*const c_void, u32) -> *mut u8,
    > = unsafe {
        lib.get(format!("{}gpu_surface_plane_base_address", prefix).as_bytes())
    }
    .expect("gpu_surface_plane_base_address");

    let socket_path_c = CString::new(socket_path.to_str().expect("path utf8")).unwrap();
    let broker = match flavor {
        ConnectFlavor::TwoArg => {
            let broker_connect: libloading::Symbol<
                unsafe extern "C" fn(*const i8, *const i8) -> *mut c_void,
            > = unsafe { lib.get(format!("{}broker_connect", prefix).as_bytes()) }
                .expect("broker_connect");
            let runtime_id_c = CString::new("multi-plane-subprocess").unwrap();
            unsafe { broker_connect(socket_path_c.as_ptr(), runtime_id_c.as_ptr()) }
        }
        ConnectFlavor::OneArg => {
            let broker_connect: libloading::Symbol<
                unsafe extern "C" fn(*const i8) -> *mut c_void,
            > = unsafe { lib.get(format!("{}broker_connect", prefix).as_bytes()) }
                .expect("broker_connect");
            unsafe { broker_connect(socket_path_c.as_ptr()) }
        }
    };
    if broker.is_null() {
        eprintln!(
            "{}resolve_surface_multi_plane: broker_connect returned null — skipping",
            prefix
        );
        return;
    }

    let surface_id_c = CString::new(surface_id.as_str()).unwrap();
    let surface = unsafe { broker_resolve_surface(broker, surface_id_c.as_ptr()) };
    if surface.is_null() {
        // resolve_surface gates on Vulkan device creation — skip cleanly
        // rather than fail when the host has no Vulkan-capable driver.
        eprintln!(
            "{}resolve_surface_multi_plane: resolve_surface returned null — skipping (no Vulkan device?)",
            prefix
        );
        unsafe { broker_disconnect(broker) };
        return;
    }

    // Assert plane count and per-plane sizes match what we check_in'd.
    let plane_count = unsafe { gpu_surface_plane_count(surface) };
    assert_eq!(plane_count, 2, "{}: plane count should be 2", prefix);

    let size0 = unsafe { gpu_surface_plane_size(surface, 0) };
    let size1 = unsafe { gpu_surface_plane_size(surface, 1) };
    assert_eq!(size0, plane_y.len() as u64, "{}: plane 0 size", prefix);
    assert_eq!(size1, plane_uv.len() as u64, "{}: plane 1 size", prefix);

    // mmap each plane and compare bytes to the source patterns.
    let rc0 = unsafe { gpu_surface_plane_mmap(surface, 0) };
    let rc1 = unsafe { gpu_surface_plane_mmap(surface, 1) };
    assert_eq!(rc0, 0, "{}: plane 0 mmap", prefix);
    assert_eq!(rc1, 0, "{}: plane 1 mmap", prefix);

    let p0 = unsafe { gpu_surface_plane_base_address(surface, 0) };
    let p1 = unsafe { gpu_surface_plane_base_address(surface, 1) };
    assert!(!p0.is_null(), "{}: plane 0 base_address", prefix);
    assert!(!p1.is_null(), "{}: plane 1 base_address", prefix);

    let mapped_y = unsafe { std::slice::from_raw_parts(p0, size0 as usize) };
    let mapped_uv = unsafe { std::slice::from_raw_parts(p1, size1 as usize) };
    assert_eq!(mapped_y, plane_y.as_slice(), "{}: plane 0 content", prefix);
    assert_eq!(
        mapped_uv,
        plane_uv.as_slice(),
        "{}: plane 1 content",
        prefix
    );

    // Out-of-range index returns 0 / null, never the wrong plane.
    assert_eq!(unsafe { gpu_surface_plane_size(surface, 7) }, 0);
    assert!(unsafe { gpu_surface_plane_base_address(surface, 7) }.is_null());

    unsafe { gpu_surface_release(surface) };
    unsafe { broker_disconnect(broker) };

    // Best-effort release on the broker.
    let stream = connect_to_broker(&socket_path).expect("host reconnect for release");
    let release_req = serde_json::json!({
        "op": "release",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
    });
    let _ = send_request_with_fds(&stream, &release_req, &[], 0);
}

#[test]
fn python_native_resolve_surface_multi_plane() {
    let lib = match locate_native_lib("libstreamlib_python_native.so") {
        Some(p) => p,
        None => {
            eprintln!(
                "python_native_resolve_surface_multi_plane: libstreamlib_python_native.so not \
                 built — run `cargo build -p streamlib-python-native` first — skipping"
            );
            return;
        }
    };
    run_shim_test(lib, "slpn_", ConnectFlavor::TwoArg);
}

#[test]
fn deno_native_resolve_surface_multi_plane() {
    let lib = match locate_native_lib("libstreamlib_deno_native.so") {
        Some(p) => p,
        None => {
            eprintln!(
                "deno_native_resolve_surface_multi_plane: libstreamlib_deno_native.so not built \
                 — run `cargo build -p streamlib-deno-native` first — skipping"
            );
            return;
        }
    };
    run_shim_test(lib, "sldn_", ConnectFlavor::OneArg);
}
