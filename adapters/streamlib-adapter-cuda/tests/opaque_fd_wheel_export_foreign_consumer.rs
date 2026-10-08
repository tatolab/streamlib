// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The raw-handle export consumed by a foreign process (#1900): a Python
//! processor's `export_opaque_fd` hands its fd over SCM_RIGHTS plus the
//! typed metadata as JSON, and this test — a separate process from both
//! the `tatolabd` hosting the stream and the processor interpreter that
//! answered the export — imports that fd on its own `VkDevice` and
//! byte-compares the kernel's pixels.
//!
//! What this locks: the exported fd names the kernel-written allocation
//! (a wrong fd reads wrong pixels or refuses to import), and the wire
//! metadata arrives shaped as the contract states (asserted field by
//! field against the known probe). What it deliberately does not lock:
//! the in-tree consumer importer hardcodes its own image recipe and
//! clamps an undersized `allocation_byte_size` up to the driver's
//! requirement, so those fields are shape-asserted here rather than
//! driven through the import — the importer's recipe/memoryTypeIndex
//! conformance is the separate work noted on the change.
//!
//! The stream is the integration suite's own
//! (`tests/stream-on-runtime/device_exchange_streams.py`), compiled by the
//! suite venv's interpreter and hosted by the runtime unit's `tatolabd`
//! (`$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else `target/tatolab-runtime`,
//! built by `cargo xtask build-runtime`), whose processor interpreters start
//! from that same venv.
//!
//! Test gating: Linux-only by construction; skips when the suite venv or
//! the runtime unit is absent, and when Vulkan (or the OPAQUE_FD pools the
//! local staging allocation needs) is unavailable — mirroring the
//! sibling carve-out tests.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::fs::File;
use std::io::Read;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanDevice, ConsumerVulkanTexture,
    TextureFormat as ConsumerTextureFormat,
};
use vulkanalia::vk;

/// Mirrors the probe's `SURFACE_WIDTH` × `SURFACE_HEIGHT` and
/// `FILL_CONSTANT_RGBA` in `device_exchange_probes.py` — the wire
/// metadata is asserted against these, so a drift fails loudly.
const SURFACE_WIDTH_PIXELS: u32 = 64;
const SURFACE_HEIGHT_PIXELS: u32 = 32;
const FILL_CONSTANT_RGBA: [u8; 4] = [64, 128, 192, 255];
const IMAGE_BYTES: u64 = (SURFACE_WIDTH_PIXELS as u64) * (SURFACE_HEIGHT_PIXELS as u64) * 4;

/// The `@stream` in the suite's `device_exchange_streams` that hosts the probe alone.
const OPAQUE_FD_EXPORT_HANDOFF_STREAM: &str = "opaque_fd_export_handoff_probe_alone";

const RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_RUNTIME_UNIT_DIRECTORY";

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn stream_on_runtime_suite_directory() -> PathBuf {
    repository_root().join("tests/stream-on-runtime")
}

fn runtime_unit_directory() -> PathBuf {
    std::env::var_os(RUNTIME_UNIT_DIRECTORY_ENVIRONMENT_VARIABLE)
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join("target/tatolab-runtime"))
}

/// Compile the suite's handoff stream to a graph file with the suite venv's
/// interpreter, from the suite directory so its modules import.
fn compile_the_handoff_stream_graph(
    suite_venv_interpreter: &Path,
    stream_graph_file: &Path,
) -> Result<(), String> {
    let compiled = Command::new(suite_venv_interpreter)
        .arg("-c")
        .arg(format!(
            "import json, sys\n\
             from tatolab.stream import compile_stream_to_graph\n\
             import device_exchange_streams\n\
             json.dump(compile_stream_to_graph(device_exchange_streams.{OPAQUE_FD_EXPORT_HANDOFF_STREAM}), sys.stdout)\n"
        ))
        .current_dir(stream_on_runtime_suite_directory())
        .env_remove("PYTHONPATH")
        .stdin(Stdio::null())
        .output()
        .map_err(|spawn_failure| format!("the suite interpreter would not start: {spawn_failure}"))?;
    if !compiled.status.success() {
        return Err(format!(
            "compiling the handoff stream exited {}:\n{}",
            compiled.status,
            String::from_utf8_lossy(&compiled.stderr)
        ));
    }
    std::fs::write(stream_graph_file, &compiled.stdout)
        .map_err(|write_failure| format!("writing the graph file failed: {write_failure}"))
}

/// The spawned `tatolabd` plus the files its stdout/stderr drain into. It
/// hosts the stream until signalled, so `Drop` — not the stream — is what
/// ends it: SIGINT, a grace window, then SIGKILL to its process group. RAII
/// so every panic path in the test reaps the GPU-holding runtime and its
/// processor interpreters instead of orphaning them for the rest of a
/// `#[serial]` rig session.
struct TatolabdHostingTheHandoffStream {
    tatolabd_process: Child,
    stdout_capture_path: PathBuf,
    stderr_capture_path: PathBuf,
}

impl TatolabdHostingTheHandoffStream {
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.tatolabd_process.try_wait().ok().flatten()
    }

    /// The last stretch of a captured output file, for diagnostics.
    fn capture_tail(capture_path: &Path) -> String {
        let captured = std::fs::read(capture_path).unwrap_or_default();
        let tail_start = captured.len().saturating_sub(4000);
        String::from_utf8_lossy(&captured[tail_start..]).into_owned()
    }

    fn diagnostic_tails(&self) -> String {
        format!(
            "tatolabd stdout tail:\n{}\ntatolabd stderr tail:\n{}",
            Self::capture_tail(&self.stdout_capture_path),
            Self::capture_tail(&self.stderr_capture_path),
        )
    }

    fn kill_its_process_group(&mut self) {
        let process_group_id = self.tatolabd_process.id() as libc::pid_t;
        unsafe { libc::killpg(process_group_id, libc::SIGKILL) };
        let _ = self.tatolabd_process.kill();
        let _ = self.tatolabd_process.wait();
    }
}

impl Drop for TatolabdHostingTheHandoffStream {
    fn drop(&mut self) {
        // A `tatolabd` that already exited was (or is here) reaped by this
        // `try_wait`; signalling its pid afterwards could hit an unrelated
        // process on a recycled pid. Its process group may still hold a
        // processor interpreter, which the group kill reaches.
        if matches!(self.tatolabd_process.try_wait(), Ok(Some(_))) {
            let process_group_id = self.tatolabd_process.id() as libc::pid_t;
            unsafe { libc::killpg(process_group_id, libc::SIGKILL) };
            return;
        }
        unsafe { libc::kill(self.tatolabd_process.id() as libc::pid_t, libc::SIGINT) };
        let grace_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.tatolabd_process.try_wait() {
                Ok(Some(_)) => {
                    let process_group_id = self.tatolabd_process.id() as libc::pid_t;
                    unsafe { libc::killpg(process_group_id, libc::SIGKILL) };
                    return;
                }
                Ok(None) if Instant::now() < grace_deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                _ => {
                    self.kill_its_process_group();
                    return;
                }
            }
        }
    }
}

/// Receive the probe's single `socket.send_fds` message: SCM_RIGHTS fds
/// plus a length-prefixed JSON payload. Bespoke rather than
/// `streamlib_surface_client::recv_message_with_fds` because the framing
/// differs — the probe sends prefix, payload and fds in one `sendmsg`,
/// while the surface-share wire splits its envelope across two.
fn receive_metadata_and_fds(stream: &UnixStream) -> (serde_json::Value, Vec<OwnedFd>) {
    let mut payload = vec![0u8; 64 * 1024];
    let mut cmsg_space = [0u8; 256];
    let mut io_vector = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut message = unsafe { std::mem::zeroed::<libc::msghdr>() };
    message.msg_iov = &mut io_vector;
    message.msg_iovlen = 1;
    message.msg_control = cmsg_space.as_mut_ptr().cast();
    message.msg_controllen = cmsg_space.len();

    let received =
        unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    assert!(
        received > 0,
        "recvmsg on the handoff socket failed: {}",
        std::io::Error::last_os_error()
    );
    let mut received_fds = Vec::new();
    let mut control = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !control.is_null() {
        let header = unsafe { &*control };
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            let fd_bytes =
                (header.cmsg_len as usize).saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
            let fd_count = fd_bytes / std::mem::size_of::<RawFd>();
            let fds = unsafe { libc::CMSG_DATA(control) } as *const RawFd;
            for index in 0..fd_count {
                // SAFETY: SCM_RIGHTS delivered a fresh descriptor this
                // process now owns; wrapping it makes every later panic
                // path close it.
                received_fds.push(unsafe { OwnedFd::from_raw_fd(*fds.add(index)) });
            }
        }
        control = unsafe { libc::CMSG_NXTHDR(&message, control) };
    }
    // Checked after the fds are owned, so a truncated message closes what
    // did arrive instead of leaking it.
    assert_eq!(
        message.msg_flags & libc::MSG_CTRUNC,
        0,
        "the ancillary buffer truncated; a dropped fd would fail later as a wrong import"
    );

    let mut bytes = payload[..received as usize].to_vec();
    let mut stream_reader = stream;
    while bytes.len() < 4 {
        let mut more = [0u8; 4096];
        let extra = stream_reader
            .read(&mut more)
            .expect("reading the handoff length prefix");
        assert!(extra > 0, "the handoff socket closed before its prefix");
        bytes.extend_from_slice(&more[..extra]);
    }
    let declared = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    while bytes.len() < declared + 4 {
        let mut more = [0u8; 4096];
        let extra = stream_reader
            .read(&mut more)
            .expect("reading the rest of the handoff payload");
        assert!(extra > 0, "the handoff socket closed mid-payload");
        bytes.extend_from_slice(&more[..extra]);
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&bytes[4..4 + declared]).expect("handoff metadata parses as JSON");
    (metadata, received_fds)
}

#[test]
#[serial]
fn a_wheel_exported_opaque_fd_read_by_a_foreign_process_shows_the_kernels_pixels() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib=warn,streamlib_consumer_rhi=debug")
        .try_init();

    let suite_venv_interpreter = stream_on_runtime_suite_directory().join(".venv/bin/python");
    if !suite_venv_interpreter.exists() {
        println!(
            "wheel export handoff: no integration suite venv at {suite_venv_interpreter:?} \
             (`uv sync` in tests/stream-on-runtime) — skipping"
        );
        return;
    }
    let tatolabd = runtime_unit_directory().join("bin/tatolabd");
    if !tatolabd.exists() {
        println!(
            "wheel export handoff: no runtime unit's tatolabd at {tatolabd:?} \
             (`cargo xtask build-runtime`) — skipping"
        );
        return;
    }
    // The local staging allocation and the consumer import need the same
    // driver support the sibling carve-outs skip without.
    let host_device = match HostVulkanDevice::new() {
        Ok(device) => device,
        Err(unavailable) => {
            println!("wheel export handoff: no Vulkan host device — skipping ({unavailable})");
            return;
        }
    };
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!("wheel export handoff: OPAQUE_FD HOST_VISIBLE buffer pool unavailable — skipping");
        return;
    }

    // Under the shared temporary directory, short: the runtime directory
    // under `XDG_RUNTIME_DIR` holds Unix sockets whose paths must fit `sun_path`.
    let run_state_directory = tempfile::Builder::new()
        .prefix("sl-fd-")
        .tempdir_in("/tmp")
        .expect("a temporary state directory for tatolabd");
    let xdg_runtime_directory = run_state_directory.path().join("xdg");
    let streamlib_home = run_state_directory.path().join("home");
    for private_directory in [&xdg_runtime_directory, &streamlib_home] {
        std::fs::create_dir(private_directory).expect("a private state directory");
        std::fs::set_permissions(private_directory, std::fs::Permissions::from_mode(0o700))
            .expect("the state directory is private");
    }
    let stream_graph_file = run_state_directory.path().join("stream-graph.json");
    compile_the_handoff_stream_graph(&suite_venv_interpreter, &stream_graph_file)
        .unwrap_or_else(|compile_failure| panic!("{compile_failure}"));

    let socket_path = run_state_directory.path().join("opaque-fd-handoff.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind the handoff socket");
    listener
        .set_nonblocking(true)
        .expect("nonblocking accept so a dead tatolabd cannot wedge the test");

    // Both output streams drain into files: a pipe left undrained would
    // block `tatolabd` instead.
    let stdout_capture_path = run_state_directory.path().join("tatolabd-stdout.log");
    let stderr_capture_path = run_state_directory.path().join("tatolabd-stderr.log");
    let tatolabd_process = Command::new(&tatolabd)
        .arg("--stream-graph")
        .arg(&stream_graph_file)
        .arg("--project")
        .arg(stream_on_runtime_suite_directory())
        .arg("--interpreter")
        .arg(&suite_venv_interpreter)
        .current_dir(run_state_directory.path())
        .env("XDG_RUNTIME_DIR", &xdg_runtime_directory)
        .env("STREAMLIB_HOME", &streamlib_home)
        .env("STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET", &socket_path)
        .env_remove("STREAMLIB_RUNTIME_NAME")
        .env_remove("STREAMLIB_APP_DIRECTORY")
        .env_remove("STREAMLIB_QUIET")
        .env_remove("STREAMLIB_ICEORYX2_DOMAIN_ROOT")
        .env_remove("PYTHONPATH")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            File::create(&stdout_capture_path).expect("create the stdout capture"),
        ))
        .stderr(Stdio::from(
            File::create(&stderr_capture_path).expect("create the stderr capture"),
        ))
        .spawn()
        .expect("spawn tatolabd hosting the handoff stream");
    let mut hosting_tatolabd = TatolabdHostingTheHandoffStream {
        tatolabd_process,
        stdout_capture_path,
        stderr_capture_path,
    };

    let accept_deadline = Instant::now() + Duration::from_secs(120);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(would_block) if would_block.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(exit) = hosting_tatolabd.exited() {
                    // tatolabd ended before the probe exported — a GPU-less
                    // box, not a contract failure. Mirror the driver-absence
                    // skips, with both output tails so a real failure is legible.
                    println!(
                        "wheel export handoff: tatolabd exited {exit} before the probe \
                         connected — skipping.\n{}",
                        hosting_tatolabd.diagnostic_tails()
                    );
                    return;
                }
                assert!(
                    Instant::now() < accept_deadline,
                    "the probe never connected to the handoff socket.\n{}",
                    hosting_tatolabd.diagnostic_tails()
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(accept_failure) => panic!("accept on the handoff socket failed: {accept_failure}"),
        }
    };
    stream
        .set_nonblocking(false)
        .expect("blocking reads once connected");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("bounded reads on the handoff socket");

    let (metadata, mut received_fds) = receive_metadata_and_fds(&stream);
    assert_eq!(
        received_fds.len(),
        1,
        "the export travels as exactly one memory fd"
    );
    let exported_texture_fd = received_fds.remove(0);

    // The wire shape, field by field against the known probe. Shape
    // assertions only — see the module doc for why the recipe fields are
    // not driven through this importer.
    let metadata_u64_field = |name: &str| -> u64 {
        metadata
            .get(name)
            .and_then(|value| value.as_u64())
            .unwrap_or_else(|| panic!("handoff metadata field {name:?} missing: {metadata}"))
    };
    assert_eq!(metadata_u64_field("width"), u64::from(SURFACE_WIDTH_PIXELS));
    assert_eq!(
        metadata_u64_field("height"),
        u64::from(SURFACE_HEIGHT_PIXELS)
    );
    assert_eq!(
        metadata.get("format").and_then(|value| value.as_str()),
        Some("rgba8_unorm")
    );
    assert_eq!(
        metadata
            .get("dedicated_allocation")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    let allocation_byte_size = metadata_u64_field("allocation_byte_size");
    assert!(allocation_byte_size >= IMAGE_BYTES);
    // The recipe constants `new_opaque_fd_export` hardcodes, read back off
    // the wire the probe crossed.
    assert_eq!(metadata_u64_field("vk_image_tiling"), 0); // OPTIMAL
    assert_eq!(metadata_u64_field("vk_image_usage_flags"), 0x0F);
    assert_eq!(metadata_u64_field("vk_image_mip_levels"), 1);
    assert_eq!(metadata_u64_field("vk_image_array_layers"), 1);
    assert_eq!(metadata_u64_field("vk_image_samples"), 1);
    assert!(metadata_u64_field("vk_memory_type_index") < 32); // VK_MAX_MEMORY_TYPES
    let exporting_device_uuid_hex = metadata
        .get("exporting_device_uuid_hex")
        .and_then(|value| value.as_str())
        .expect("handoff metadata carries the exporting device UUID");
    assert_eq!(exporting_device_uuid_hex.len(), 32);
    assert_ne!(exporting_device_uuid_hex, "0".repeat(32));

    // This process's own readback staging: allocated host-side, imported
    // consumer-side — the sibling carve-outs' proven shape.
    let local_staging_buffer = Arc::new(
        HostVulkanBuffer::new_opaque_fd_export(&host_device, IMAGE_BYTES)
            .expect("local staging new_opaque_fd_export"),
    );
    let local_staging_fd = unsafe {
        // SAFETY: `export_opaque_fd_memory` mints a fresh caller-owned fd.
        OwnedFd::from_raw_fd(
            local_staging_buffer
                .export_opaque_fd_memory()
                .expect("export local staging OPAQUE_FD"),
        )
    };

    let consumer_vulkan_device = match ConsumerVulkanDevice::new() {
        Ok(device) => Arc::new(device),
        Err(unavailable) => {
            println!(
                "wheel export handoff: ConsumerVulkanDevice::new failed: {unavailable:?} — \
                 skipping (likely a UUID mismatch on a multi-GPU rig)"
            );
            return;
        }
    };
    // Both imports adopt their fd on success and leave it owned here on
    // failure, so each is released only at its call.
    let imported_texture = match ConsumerVulkanTexture::from_opaque_fd(
        &consumer_vulkan_device,
        exported_texture_fd.as_raw_fd(),
        SURFACE_WIDTH_PIXELS,
        SURFACE_HEIGHT_PIXELS,
        ConsumerTextureFormat::Rgba8Unorm,
        allocation_byte_size,
    ) {
        Ok(texture) => {
            let _adopted_by_vulkan = exported_texture_fd.into_raw_fd();
            Arc::new(texture)
        }
        Err(import_failure) => {
            panic!(
                "the exported fd would not import: {import_failure}\n{}",
                hosting_tatolabd.diagnostic_tails()
            );
        }
    };
    let imported_staging = match ConsumerVulkanBuffer::from_opaque_fd(
        &consumer_vulkan_device,
        local_staging_fd.as_raw_fd(),
        IMAGE_BYTES as vk::DeviceSize,
    ) {
        Ok(buffer) => {
            let _adopted_by_vulkan = local_staging_fd.into_raw_fd();
            Arc::new(buffer)
        }
        Err(import_failure) => {
            panic!(
                "the local staging fd would not import: {import_failure}\n{}",
                hosting_tatolabd.diagnostic_tails()
            );
        }
    };

    let consumer_device_handle = consumer_vulkan_device.device();
    let imported_vk_image = imported_texture.image();
    let imported_vk_buffer = imported_staging.buffer();
    unsafe {
        common::submit_one_shot(
            consumer_device_handle,
            consumer_vulkan_device.queue(),
            consumer_vulkan_device.queue_family_index(),
            |command_buffer| {
                common::record_image_readback_to_buffer(
                    consumer_device_handle,
                    command_buffer,
                    imported_vk_image,
                    imported_vk_buffer,
                    SURFACE_WIDTH_PIXELS,
                    SURFACE_HEIGHT_PIXELS,
                );
            },
        );
    }

    // SAFETY: HOST_VISIBLE | HOST_COHERENT via the host-side allocation;
    // the mapped pointer is valid for the buffer's lifetime, and the
    // fence inside `submit_one_shot` already retired the device writes
    // into this shared memory.
    let readback = unsafe {
        std::slice::from_raw_parts(local_staging_buffer.mapped_ptr(), IMAGE_BYTES as usize)
    };
    let pixels_match = readback
        .chunks_exact(4)
        .all(|pixel| pixel == FILL_CONSTANT_RGBA);
    let verdict: &[u8] = if pixels_match {
        b"pixels-match"
    } else {
        b"mismatch"
    };
    use std::io::Write;
    let _ = (&stream).write_all(verdict);
    drop(stream);
    drop(hosting_tatolabd);

    assert!(
        pixels_match,
        "the foreign import driven by the processor's export bundle must read the kernel's \
         fill constant {FILL_CONSTANT_RGBA:?}; first pixel was {:?}",
        &readback[..4]
    );
}
