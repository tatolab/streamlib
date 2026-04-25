// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux polyglot consumer integration test (#394 / #420).
//!
//! Spawns a real Python 3 subprocess that loads `libstreamlib_python_native.so`
//! via ctypes, drives the `slpn_surface_*` / `slpn_gpu_surface_*` FFI exactly
//! like `subprocess_runner.py` does, and verifies:
//!   - the bytes read through `slpn_gpu_surface_base_address` match a host-written
//!     DMA-BUF pattern (byte-for-byte),
//!   - `slpn_gpu_surface_backend` reports the Vulkan import path took effect
//!     rather than silently falling back (the mmap fallback was removed in
//!     #420 so this is a sentinel against regressions).
//!
//! Catches breakage between the three moving parts that the native-lib's
//! in-process unit tests don't exercise together:
//!   - wire protocol through a second OS process (different FD table),
//!   - ctypes-level ABI for every FFI symbol Python calls,
//!   - `STREAMLIB_SURFACE_SOCKET` env propagation.
//!
//! The host side uses a real Vulkan-exported DMA-BUF
//! ([`common::polyglot_dma_buf_producer::TestDmaBufProducer`]) — memfd is
//! no longer a valid substrate because NVIDIA's Vulkan driver rejects memfd
//! fds as `DMA_BUF_EXT` handle types, and the subprocess consumer now imports
//! via `VkImportMemoryFdInfoKHR`.
//!
//! Skips gracefully when:
//!   - `python3` is not on PATH (non-Linux CI, minimal sandboxes),
//!   - `libstreamlib_python_native.so` has not been built under
//!     `target/debug/` (test ran before `cargo build -p streamlib-python-native`),
//!   - no Vulkan-capable device is present (matches the native-lib's
//!     lazy-init behavior on GPU-less hosts).

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::{Command, Stdio};

use streamlib::core::runtime::StreamRuntime;
use streamlib_surface_client::{connect_to_surface_share_socket, send_request_with_fds};

#[path = "common/polyglot_dma_buf_producer.rs"]
mod polyglot_dma_buf_producer;
use polyglot_dma_buf_producer::TestDmaBufProducer;

/// Locate `libstreamlib_python_native.so` under the workspace target dir.
///
/// `CARGO_MANIFEST_DIR` points at `libs/streamlib`; go up two levels to the
/// workspace root, then descend into `target/{debug,release}`.
fn locate_native_lib() -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    for profile in &["debug", "release"] {
        let candidate = workspace
            .join("target")
            .join(profile)
            .join("libstreamlib_python_native.so");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the Python driver that runs inside the subprocess. Kept as an
/// embedded string so the test is self-contained and doesn't have to ship
/// a fixture .py file.
fn python_driver_source() -> &'static str {
    r#"
import ctypes
import os
import sys

native_lib_path = os.environ["TEST_NATIVE_LIB"]
socket_path = os.environ["STREAMLIB_SURFACE_SOCKET"]
runtime_id = os.environ["STREAMLIB_RUNTIME_ID"]
surface_id = os.environ["TEST_SURFACE_ID"]
expected_width = int(os.environ["TEST_WIDTH"])
expected_height = int(os.environ["TEST_HEIGHT"])
expected_bpr = int(os.environ["TEST_BYTES_PER_ROW"])
expected_head_hex = os.environ["TEST_EXPECTED_HEAD_HEX"]
expected_backend = int(os.environ["TEST_EXPECTED_BACKEND"])

lib = ctypes.cdll.LoadLibrary(native_lib_path)

lib.slpn_surface_connect.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
lib.slpn_surface_connect.restype = ctypes.c_void_p
lib.slpn_surface_disconnect.argtypes = [ctypes.c_void_p]
lib.slpn_surface_disconnect.restype = None
lib.slpn_surface_resolve_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
lib.slpn_surface_resolve_surface.restype = ctypes.c_void_p
lib.slpn_surface_unregister_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
lib.slpn_surface_unregister_surface.restype = None

lib.slpn_gpu_surface_lock.argtypes = [ctypes.c_void_p, ctypes.c_int32]
lib.slpn_gpu_surface_lock.restype = ctypes.c_int32
lib.slpn_gpu_surface_unlock.argtypes = [ctypes.c_void_p, ctypes.c_int32]
lib.slpn_gpu_surface_unlock.restype = ctypes.c_int32
lib.slpn_gpu_surface_base_address.argtypes = [ctypes.c_void_p]
lib.slpn_gpu_surface_base_address.restype = ctypes.c_void_p
lib.slpn_gpu_surface_width.argtypes = [ctypes.c_void_p]
lib.slpn_gpu_surface_width.restype = ctypes.c_uint32
lib.slpn_gpu_surface_height.argtypes = [ctypes.c_void_p]
lib.slpn_gpu_surface_height.restype = ctypes.c_uint32
lib.slpn_gpu_surface_bytes_per_row.argtypes = [ctypes.c_void_p]
lib.slpn_gpu_surface_bytes_per_row.restype = ctypes.c_uint32
lib.slpn_gpu_surface_backend.argtypes = [ctypes.c_void_p]
lib.slpn_gpu_surface_backend.restype = ctypes.c_uint32
lib.slpn_gpu_surface_release.argtypes = [ctypes.c_void_p]
lib.slpn_gpu_surface_release.restype = None

def fatal(msg):
    sys.stderr.write("FAIL: " + msg + "\n")
    sys.stderr.flush()
    sys.exit(1)

conn = lib.slpn_surface_connect(socket_path.encode("utf-8"), runtime_id.encode("utf-8"))
if not conn:
    fatal("slpn_surface_connect returned null")

handle = lib.slpn_surface_resolve_surface(conn, surface_id.encode("utf-8"))
if not handle:
    fatal("slpn_surface_resolve_surface returned null")

width = lib.slpn_gpu_surface_width(handle)
height = lib.slpn_gpu_surface_height(handle)
bpr = lib.slpn_gpu_surface_bytes_per_row(handle)
if width != expected_width:
    fatal("width: expected %d got %d" % (expected_width, width))
if height != expected_height:
    fatal("height: expected %d got %d" % (expected_height, height))
if bpr != expected_bpr:
    fatal("bytes_per_row: expected %d got %d" % (expected_bpr, bpr))

if lib.slpn_gpu_surface_lock(handle, 1) != 0:
    fatal("slpn_gpu_surface_lock returned non-zero")

backend = lib.slpn_gpu_surface_backend(handle)
if backend != expected_backend:
    fatal("backend: expected %d (Vulkan) got %d" % (expected_backend, backend))

base = lib.slpn_gpu_surface_base_address(handle)
if not base:
    fatal("base_address null after lock")

n = len(expected_head_hex) // 2
head = (ctypes.c_uint8 * n).from_address(base)
actual_hex = bytes(head).hex()
if actual_hex != expected_head_hex:
    fatal("first %d bytes mismatch: expected %s got %s" % (n, expected_head_hex, actual_hex))

if lib.slpn_gpu_surface_unlock(handle, 1) != 0:
    fatal("slpn_gpu_surface_unlock returned non-zero")

# Second resolve_surface should hit the cache and return identical metadata.
handle2 = lib.slpn_surface_resolve_surface(conn, surface_id.encode("utf-8"))
if not handle2:
    fatal("second resolve_surface returned null (cache miss should still succeed)")
if lib.slpn_gpu_surface_width(handle2) != expected_width:
    fatal("cached handle width mismatch")
lib.slpn_gpu_surface_release(handle2)

lib.slpn_surface_unregister_surface(conn, surface_id.encode("utf-8"))
lib.slpn_gpu_surface_release(handle)
lib.slpn_surface_disconnect(conn)

sys.stdout.write("OK " + actual_hex + "\n")
sys.stdout.flush()
"#
}

/// `slpn_gpu_surface_backend` value that indicates the Vulkan import path
/// was used — must match `gpu_surface::SURFACE_BACKEND_VULKAN` in the Python
/// native lib.
const EXPECTED_BACKEND_VULKAN: u32 = 2;

#[test]
fn python_subprocess_resolves_and_vulkan_imports_host_published_surface() {
    let native_lib = match locate_native_lib() {
        Some(p) => p,
        None => {
            eprintln!(
                "polyglot_linux_check_out: libstreamlib_python_native.so not built; \
                 run `cargo build -p streamlib-python-native` first — skipping"
            );
            return;
        }
    };
    if !python3_available() {
        eprintln!("polyglot_linux_check_out: python3 not on PATH — skipping");
        return;
    }

    // 0. Skip cleanly if no Vulkan-capable device is present. Matches the
    //    native-lib's lazy-init behavior on GPU-less hosts.
    let producer = match TestDmaBufProducer::try_new() {
        Ok(p) => p,
        Err(reason) => {
            eprintln!(
                "polyglot_linux_check_out: no Vulkan DMA-BUF producer — skipping ({})",
                reason
            );
            return;
        }
    };

    // 1. Stand up a real StreamRuntime — it owns the surface-sharing service
    //    on a per-runtime Unix socket. No external surface-share daemon, no manual
    //    SurfaceShareState/UnixSocketSurfaceService construction.
    let runtime = StreamRuntime::new().expect("StreamRuntime::new");
    let socket_path = runtime.surface_socket_path().to_path_buf();
    let runtime_id = runtime.runtime_id().to_string();

    // 2. Host: allocate a real Vulkan-exported DMA-BUF seeded with a
    //    deterministic pattern, and check_in to the runtime-internal surface-share service.
    let width: u32 = 64;
    let height: u32 = 4;
    let bpp: u32 = 4;
    let size: usize = (width * height * bpp) as usize;
    let mut pattern = Vec::with_capacity(size);
    for i in 0..size {
        pattern.push(((i * 13 + 7) & 0xFF) as u8);
    }
    let fd = match producer.produce(&pattern) {
        Ok(fd) => fd,
        Err(reason) => {
            eprintln!(
                "polyglot_linux_check_out: Vulkan DMA-BUF producer failed — skipping ({})",
                reason
            );
            return;
        }
    };

    let host_stream = connect_to_surface_share_socket(&socket_path).expect("host connect");
    let check_in_req = serde_json::json!({
        "op": "check_in",
        "runtime_id": runtime_id,
        "width": width,
        "height": height,
        "format": "Bgra32",
        "resource_type": "pixel_buffer",
    });
    let (resp, _) =
        send_request_with_fds(&host_stream, &check_in_req, &[fd], 0).expect("host check_in");
    unsafe { libc::close(fd) };
    let surface_id = resp
        .get("surface_id")
        .and_then(|v| v.as_str())
        .expect("surface_id")
        .to_string();
    drop(host_stream);

    // 3. Spawn a Python subprocess driving the native-lib FFI.
    let head_bytes = 32usize.min(size);
    let expected_head_hex: String = pattern[..head_bytes]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    let driver = python_driver_source();
    let output = Command::new("python3")
        .arg("-c")
        .arg(driver)
        .env("STREAMLIB_SURFACE_SOCKET", &socket_path)
        .env("STREAMLIB_RUNTIME_ID", &runtime_id)
        .env("TEST_NATIVE_LIB", &native_lib)
        .env("TEST_SURFACE_ID", &surface_id)
        .env("TEST_WIDTH", width.to_string())
        .env("TEST_HEIGHT", height.to_string())
        .env("TEST_BYTES_PER_ROW", (width * bpp).to_string())
        .env("TEST_EXPECTED_HEAD_HEX", &expected_head_hex)
        .env("TEST_EXPECTED_BACKEND", EXPECTED_BACKEND_VULKAN.to_string())
        .output()
        .expect("spawn python3");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Drop the runtime — UnixSocketSurfaceService::Drop tears the service
    // down and removes the socket file. Explicit so the cleanup happens
    // before the test asserts (and the ordering matches the prior
    // service.stop() placement).
    drop(runtime);

    assert!(
        output.status.success(),
        "python subprocess failed (exit={:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout,
        stderr
    );
    assert!(
        stdout.starts_with("OK "),
        "expected OK prefix from python driver, got:\nstdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains(&expected_head_hex),
        "expected head hex '{}' in stdout '{}'",
        expected_head_hex,
        stdout
    );
}

#[test]
fn python_subprocess_reports_clear_error_on_missing_surface_socket() {
    let native_lib = match locate_native_lib() {
        Some(p) => p,
        None => {
            eprintln!(
                "polyglot_linux_check_out: libstreamlib_python_native.so not built; skipping"
            );
            return;
        }
    };
    if !python3_available() {
        eprintln!("polyglot_linux_check_out: python3 not on PATH — skipping");
        return;
    }

    // Driver that lazy-connects to a bogus socket and expects the first
    // resolve to fail (null handle), without crashing the subprocess.
    let driver = r#"
import ctypes
import os
import sys

native_lib_path = os.environ["TEST_NATIVE_LIB"]
socket_path = os.environ["STREAMLIB_SURFACE_SOCKET"]

lib = ctypes.cdll.LoadLibrary(native_lib_path)
lib.slpn_surface_connect.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
lib.slpn_surface_connect.restype = ctypes.c_void_p
lib.slpn_surface_resolve_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
lib.slpn_surface_resolve_surface.restype = ctypes.c_void_p
lib.slpn_surface_disconnect.argtypes = [ctypes.c_void_p]
lib.slpn_surface_disconnect.restype = None

conn = lib.slpn_surface_connect(socket_path.encode("utf-8"), b"lazy-test")
assert conn, "connect must succeed lazily even for a bad socket"
surface = lib.slpn_surface_resolve_surface(conn, b"any-surface")
assert not surface, "resolve must return null when the socket is unreachable"
lib.slpn_surface_disconnect(conn)

sys.stdout.write("LAZY_FAIL_OK\n")
sys.stdout.flush()
"#;

    let output = Command::new("python3")
        .arg("-c")
        .arg(driver)
        .env("STREAMLIB_SURFACE_SOCKET", "/nonexistent/surface-share.sock")
        .env("TEST_NATIVE_LIB", &native_lib)
        .output()
        .expect("spawn python3");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "python subprocess must exit cleanly on lazy-connect failure (exit={:?})\nstderr:\n{}",
        output.status.code(),
        stderr
    );
    assert!(
        stdout.contains("LAZY_FAIL_OK"),
        "expected LAZY_FAIL_OK in stdout, got:\nstdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );
}
