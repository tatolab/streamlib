// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux polyglot Deno consumer integration test (#394 / #420).
//!
//! Deno twin of `polyglot_linux_check_out.rs`. Spawns `deno` with an inline
//! TypeScript driver that loads `libstreamlib_deno_native.so` via
//! `Deno.dlopen`, drives the `sldn_broker_*` / `sldn_gpu_surface_*` FFI like
//! `subprocess_runner.ts` does, and verifies the bytes read through
//! `sldn_gpu_surface_base_address` match the host-written DMA-BUF pattern
//! AND that `sldn_gpu_surface_backend` reports the Vulkan import path was
//! used (sentinel against regression to an mmap fallback).
//!
//! Skip conditions: no `deno` on PATH → skip; no `libstreamlib_deno_native.so`
//! under target/debug|release → skip; no Vulkan-capable device → skip.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use streamlib::core::runtime::StreamRuntime;
use streamlib_broker_client::{connect_to_broker, send_request_with_fds};

#[path = "common/polyglot_dma_buf_producer.rs"]
mod polyglot_dma_buf_producer;
use polyglot_dma_buf_producer::TestDmaBufProducer;

fn locate_native_lib() -> Option<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let workspace = PathBuf::from(&manifest_dir).join("..").join("..");
    for profile in &["debug", "release"] {
        let candidate = workspace
            .join("target")
            .join(profile)
            .join("libstreamlib_deno_native.so");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn deno_available() -> bool {
    Command::new("deno")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `sldn_gpu_surface_backend` value that indicates the Vulkan import path
/// was used — must match `gpu_surface::SURFACE_BACKEND_VULKAN` in the Deno
/// native lib.
const EXPECTED_BACKEND_VULKAN: u32 = 2;

fn deno_driver() -> &'static str {
    // `Deno.dlopen` uses `buffer` for C-string args, `pointer` for opaque
    // handles. `Deno.UnsafePointerView` gives us byte access to the mmap'd
    // region once we get a base_address pointer back from the native lib.
    r#"
const nativeLibPath = Deno.env.get("TEST_NATIVE_LIB")!;
const socketPath = Deno.env.get("STREAMLIB_BROKER_SOCKET")!;
const surfaceId = Deno.env.get("TEST_SURFACE_ID")!;
const expectedWidth = parseInt(Deno.env.get("TEST_WIDTH")!);
const expectedHeight = parseInt(Deno.env.get("TEST_HEIGHT")!);
const expectedBpr = parseInt(Deno.env.get("TEST_BYTES_PER_ROW")!);
const expectedHeadHex = Deno.env.get("TEST_EXPECTED_HEAD_HEX")!;
const expectedBackend = parseInt(Deno.env.get("TEST_EXPECTED_BACKEND")!);

const lib = Deno.dlopen(nativeLibPath, {
  sldn_broker_connect: { parameters: ["buffer"], result: "pointer" },
  sldn_broker_disconnect: { parameters: ["pointer"], result: "void" },
  sldn_broker_resolve_surface: { parameters: ["pointer", "buffer"], result: "pointer" },
  sldn_broker_unregister_surface: { parameters: ["pointer", "buffer"], result: "void" },
  sldn_gpu_surface_lock: { parameters: ["pointer", "i32"], result: "i32" },
  sldn_gpu_surface_unlock: { parameters: ["pointer", "i32"], result: "i32" },
  sldn_gpu_surface_base_address: { parameters: ["pointer"], result: "pointer" },
  sldn_gpu_surface_width: { parameters: ["pointer"], result: "u32" },
  sldn_gpu_surface_height: { parameters: ["pointer"], result: "u32" },
  sldn_gpu_surface_bytes_per_row: { parameters: ["pointer"], result: "u32" },
  sldn_gpu_surface_backend: { parameters: ["pointer"], result: "u32" },
  sldn_gpu_surface_release: { parameters: ["pointer"], result: "void" },
});

function cStr(s: string): Uint8Array {
  return new TextEncoder().encode(s + "\0");
}

function fatal(msg: string): never {
  console.error("FAIL: " + msg);
  Deno.exit(1);
}

const broker = lib.symbols.sldn_broker_connect(cStr(socketPath));
if (broker === null) fatal("sldn_broker_connect returned null");

const handle = lib.symbols.sldn_broker_resolve_surface(broker, cStr(surfaceId));
if (handle === null) fatal("sldn_broker_resolve_surface returned null");

const width = lib.symbols.sldn_gpu_surface_width(handle);
const height = lib.symbols.sldn_gpu_surface_height(handle);
const bpr = lib.symbols.sldn_gpu_surface_bytes_per_row(handle);
if (width !== expectedWidth) fatal(`width: expected ${expectedWidth} got ${width}`);
if (height !== expectedHeight) fatal(`height: expected ${expectedHeight} got ${height}`);
if (bpr !== expectedBpr) fatal(`bytes_per_row: expected ${expectedBpr} got ${bpr}`);

if (lib.symbols.sldn_gpu_surface_lock(handle, 1) !== 0) fatal("lock returned non-zero");

const backend = lib.symbols.sldn_gpu_surface_backend(handle);
if (backend !== expectedBackend) {
  fatal(`backend: expected ${expectedBackend} (Vulkan) got ${backend}`);
}

const base = lib.symbols.sldn_gpu_surface_base_address(handle);
if (base === null) fatal("base_address null after lock");

const n = expectedHeadHex.length / 2;
const view = new Deno.UnsafePointerView(base);
const headBuf = new Uint8Array(n);
view.copyInto(headBuf);
const actualHex = Array.from(headBuf).map((b) => b.toString(16).padStart(2, "0")).join("");
if (actualHex !== expectedHeadHex) {
  fatal(`first ${n} bytes mismatch: expected ${expectedHeadHex} got ${actualHex}`);
}

if (lib.symbols.sldn_gpu_surface_unlock(handle, 1) !== 0) fatal("unlock returned non-zero");

// Second resolve — cache hit path.
const handle2 = lib.symbols.sldn_broker_resolve_surface(broker, cStr(surfaceId));
if (handle2 === null) fatal("cached resolve_surface returned null");
if (lib.symbols.sldn_gpu_surface_width(handle2) !== expectedWidth) fatal("cached width mismatch");
lib.symbols.sldn_gpu_surface_release(handle2);

lib.symbols.sldn_broker_unregister_surface(broker, cStr(surfaceId));
lib.symbols.sldn_gpu_surface_release(handle);
lib.symbols.sldn_broker_disconnect(broker);
lib.close();

console.log("OK " + actualHex);
"#
}

#[test]
fn deno_subprocess_resolves_and_vulkan_imports_host_published_surface() {
    let native_lib = match locate_native_lib() {
        Some(p) => p,
        None => {
            eprintln!(
                "polyglot_linux_check_out_deno: libstreamlib_deno_native.so not built; \
                 run `cargo build -p streamlib-deno-native` first — skipping"
            );
            return;
        }
    };
    if !deno_available() {
        eprintln!("polyglot_linux_check_out_deno: deno not on PATH — skipping");
        return;
    }
    let producer = match TestDmaBufProducer::try_new() {
        Ok(p) => p,
        Err(reason) => {
            eprintln!(
                "polyglot_linux_check_out_deno: no Vulkan DMA-BUF producer — skipping ({})",
                reason
            );
            return;
        }
    };

    // Stand up a real StreamRuntime — owns the per-runtime surface-sharing
    // socket. No external broker daemon, no manual fixture.
    let runtime = StreamRuntime::new().expect("StreamRuntime::new");
    let socket_path = runtime.surface_socket_path().to_path_buf();
    let runtime_id = runtime.runtime_id().to_string();

    let width: u32 = 64;
    let height: u32 = 4;
    let bpp: u32 = 4;
    let size: usize = (width * height * bpp) as usize;
    let mut pattern = Vec::with_capacity(size);
    for i in 0..size {
        pattern.push(((i * 19 + 11) & 0xFF) as u8);
    }
    let fd = match producer.produce(&pattern) {
        Ok(fd) => fd,
        Err(reason) => {
            eprintln!(
                "polyglot_linux_check_out_deno: Vulkan DMA-BUF producer failed — skipping ({})",
                reason
            );
            return;
        }
    };

    let host_stream = connect_to_broker(&socket_path).expect("host connect");
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

    let head_bytes = 32usize.min(size);
    let expected_head_hex: String = pattern[..head_bytes]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    // `deno run -` reads the script from stdin. Stage the script to a tmpfile
    // and pass the path instead — simpler than juggling ChildStdin, and deno
    // flushes / closes on EOF either way.
    let mut script_file = tempfile::NamedTempFile::new().expect("tempfile");
    script_file
        .write_all(deno_driver().as_bytes())
        .expect("write driver");
    let script_path = script_file.path().to_path_buf();

    let output = Command::new("deno")
        .arg("run")
        .arg("--allow-ffi")
        .arg("--allow-env")
        .arg("--allow-read")
        .arg(&script_path)
        .env("STREAMLIB_BROKER_SOCKET", &socket_path)
        .env("TEST_NATIVE_LIB", &native_lib)
        .env("TEST_SURFACE_ID", &surface_id)
        .env("TEST_WIDTH", width.to_string())
        .env("TEST_HEIGHT", height.to_string())
        .env("TEST_BYTES_PER_ROW", (width * bpp).to_string())
        .env("TEST_EXPECTED_HEAD_HEX", &expected_head_hex)
        .env("TEST_EXPECTED_BACKEND", EXPECTED_BACKEND_VULKAN.to_string())
        .output()
        .expect("spawn deno");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Drop the runtime — UnixSocketSurfaceService::Drop tears the service
    // down and removes the socket file. Explicit drop matches the prior
    // service.stop() placement.
    drop(runtime);

    assert!(
        output.status.success(),
        "deno subprocess failed (exit={:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout,
        stderr
    );
    assert!(
        stdout.contains("OK "),
        "expected OK prefix from deno driver, got:\nstdout:\n{}\nstderr:\n{}",
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
