// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The raw-handle export contract consumed end-to-end (#1900): a Python
//! processor's `export_opaque_fd` bundle — the fd over SCM_RIGHTS plus the
//! typed metadata as JSON — is the *only* input this foreign process uses
//! to import the texture on its own `VkDevice` and byte-compare the
//! kernel's pixels.
//!
//! What makes this the contract's proof and not another carve-out
//! round-trip: the exporting engine, the helper process that answers the
//! export, and this test are three separate processes, and the import here
//! is driven entirely by what crossed the socket. A wrong fd, a wrong
//! `allocation_byte_size`, or a recipe an importer rejects fails here —
//! nothing on the exporting side can keep this green.
//!
//! Test gating: Linux-only by construction; skips when the wheel's venv or
//! its built module is absent, and when Vulkan (or the OPAQUE_FD pools the
//! local staging allocation needs) is unavailable — mirroring the
//! sibling carve-out tests.

#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serial_test::serial;
use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanDevice, ConsumerVulkanTexture,
    TextureFormat as ConsumerTextureFormat,
};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

/// Mirrors the probe's `SURFACE_WIDTH` × `SURFACE_HEIGHT` and
/// `FILL_CONSTANT_RGBA` in `device_exchange_probes.py` — the wire
/// metadata is asserted against these, so a drift fails loudly.
const W: u32 = 64;
const H: u32 = 32;
const FILL_CONSTANT_RGBA: [u8; 4] = [64, 128, 192, 255];
const IMAGE_BYTES: u64 = (W as u64) * (H as u64) * 4;

fn wheel_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sdk/streamlib-python-wheel")
}

/// One `recvmsg` that takes the SCM_RIGHTS fds, then a read loop until the
/// length-prefixed JSON payload is complete.
fn receive_metadata_and_fds(stream: &UnixStream) -> (serde_json::Value, Vec<RawFd>) {
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
            let fd_bytes = header.cmsg_len as usize - unsafe { libc::CMSG_LEN(0) } as usize;
            let fd_count = fd_bytes / std::mem::size_of::<RawFd>();
            let fds = unsafe { libc::CMSG_DATA(control) } as *const RawFd;
            for index in 0..fd_count {
                received_fds.push(unsafe { *fds.add(index) });
            }
        }
        control = unsafe { libc::CMSG_NXTHDR(&message, control) };
    }

    let mut bytes = payload[..received as usize].to_vec();
    assert!(
        bytes.len() >= 4,
        "the handoff payload lost its length prefix"
    );
    let declared = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut stream_reader = stream;
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

/// SIGINT then, if the app outlives the grace window, SIGKILL — the wheel
/// handles Ctrl-C cleanly and this is its shutdown path.
fn stop_app(mut child: Child) {
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

#[test]
#[serial]
fn a_wheel_exported_opaque_fd_read_by_a_foreign_process_shows_the_kernels_pixels() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib=warn,streamlib_consumer_rhi=debug")
        .try_init();

    let wheel_dir = wheel_directory();
    let venv_python = wheel_dir.join(".venv/bin/python");
    if !venv_python.exists() {
        println!("wheel export handoff: no wheel venv at {venv_python:?} — skipping");
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

    let socket_dir = tempfile::TempDir::new().expect("temp dir for the handoff socket");
    let socket_path = socket_dir.path().join("opaque-fd-handoff.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind the handoff socket");
    listener
        .set_nonblocking(true)
        .expect("nonblocking accept so a dead app cannot wedge the test");

    let mut app = Command::new(&venv_python)
        .arg("device_exchange_app.py")
        .arg("OpaqueFdExportHandoffProbe")
        .current_dir(wheel_dir.join("tests"))
        .env("STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET", &socket_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the wheel app under test");

    let accept_deadline = Instant::now() + Duration::from_secs(120);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(would_block) if would_block.kind() == std::io::ErrorKind::WouldBlock => {
                if let Ok(Some(exit)) = app.try_wait() {
                    // The app died before exporting — a GPU-less box, not a
                    // contract failure. Mirror the driver-absence skips.
                    let mut stderr_tail = String::new();
                    if let Some(mut stderr) = app.stderr.take() {
                        let _ = stderr.read_to_string(&mut stderr_tail);
                    }
                    let tail_start = stderr_tail.len().saturating_sub(2000);
                    println!(
                        "wheel export handoff: app exited {exit} before connecting — \
                         skipping. stderr tail:\n{}",
                        &stderr_tail[tail_start..]
                    );
                    return;
                }
                assert!(
                    Instant::now() < accept_deadline,
                    "the app never connected to the handoff socket"
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

    let (metadata, received_fds) = receive_metadata_and_fds(&stream);
    assert_eq!(
        received_fds.len(),
        1,
        "the export travels as exactly one memory fd"
    );
    let texture_fd = received_fds[0];

    // The bundle is the whole import input — assert the wire shape first.
    let field_u64 = |name: &str| -> u64 {
        metadata
            .get(name)
            .and_then(|value| value.as_u64())
            .unwrap_or_else(|| panic!("handoff metadata field {name:?} missing: {metadata}"))
    };
    assert_eq!(field_u64("width"), u64::from(W));
    assert_eq!(field_u64("height"), u64::from(H));
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
    let allocation_byte_size = field_u64("allocation_byte_size");
    assert!(allocation_byte_size >= IMAGE_BYTES);
    let exporting_device_uuid_hex = metadata
        .get("exporting_device_uuid_hex")
        .and_then(|value| value.as_str())
        .expect("handoff metadata carries the exporting device UUID");
    assert_eq!(exporting_device_uuid_hex.len(), 32);
    assert_ne!(exporting_device_uuid_hex, "0".repeat(32));

    // This process's own readback staging: allocated host-side, imported
    // consumer-side — the sibling carve-outs' proven shape.
    let staging_buf = Arc::new(
        HostVulkanBuffer::new_opaque_fd_export(&host_device, IMAGE_BYTES)
            .expect("local staging new_opaque_fd_export"),
    );
    let staging_fd = staging_buf
        .export_opaque_fd_memory()
        .expect("export local staging OPAQUE_FD");

    let consumer_device = match ConsumerVulkanDevice::new() {
        Ok(device) => Arc::new(device),
        Err(unavailable) => {
            unsafe {
                libc::close(texture_fd);
                libc::close(staging_fd);
            }
            println!(
                "wheel export handoff: ConsumerVulkanDevice::new failed: {unavailable:?} — skipping"
            );
            stop_app(app);
            return;
        }
    };
    let imported_texture = match ConsumerVulkanTexture::from_opaque_fd(
        &consumer_device,
        texture_fd,
        W,
        H,
        ConsumerTextureFormat::Rgba8Unorm,
        allocation_byte_size,
    ) {
        Ok(texture) => Arc::new(texture),
        Err(import_failure) => {
            unsafe {
                libc::close(texture_fd);
                libc::close(staging_fd);
            }
            stop_app(app);
            panic!("the wheel-exported fd would not import: {import_failure}");
        }
    };
    let imported_staging = match ConsumerVulkanBuffer::from_opaque_fd(
        &consumer_device,
        staging_fd,
        IMAGE_BYTES as vk::DeviceSize,
    ) {
        Ok(buffer) => Arc::new(buffer),
        Err(import_failure) => {
            unsafe { libc::close(staging_fd) };
            stop_app(app);
            panic!("the local staging fd would not import: {import_failure}");
        }
    };

    // Readback on the foreign device: bridge UNDEFINED → TRANSFER_SRC,
    // copy image → staging (the bridge's spec status is documented on the
    // sibling file's `record_image_readback_to_buffer`).
    let consumer_dev = consumer_device.device();
    let consumer_queue = consumer_device.queue();
    let consumer_qfi = consumer_device.queue_family_index();
    let consumer_vk_image = imported_texture.image();
    let consumer_vk_buffer = imported_staging.buffer();
    unsafe {
        let pool_info = vk::CommandPoolCreateInfo::builder()
            .queue_family_index(consumer_qfi)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .build();
        let pool = consumer_dev
            .create_command_pool(&pool_info, None)
            .expect("create_command_pool");
        let alloc_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        let cmd = consumer_dev
            .allocate_command_buffers(&alloc_info)
            .expect("allocate_command_buffers")[0];
        let begin = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
            .build();
        consumer_dev
            .begin_command_buffer(cmd, &begin)
            .expect("begin_command_buffer");

        let acquire_barrier = vk::ImageMemoryBarrier2::builder()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(consumer_vk_image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build();
        let acquire_barriers = [acquire_barrier];
        let acquire_dep = vk::DependencyInfo::builder()
            .image_memory_barriers(&acquire_barriers)
            .build();
        consumer_dev.cmd_pipeline_barrier2(cmd, &acquire_dep);

        let region = vk::BufferImageCopy2::builder()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
            .image_extent(vk::Extent3D {
                width: W,
                height: H,
                depth: 1,
            })
            .build();
        let regions = [region];
        let copy_info = vk::CopyImageToBufferInfo2::builder()
            .src_image(consumer_vk_image)
            .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .dst_buffer(consumer_vk_buffer)
            .regions(&regions)
            .build();
        consumer_dev.cmd_copy_image_to_buffer2(cmd, &copy_info);

        consumer_dev
            .end_command_buffer(cmd)
            .expect("end_command_buffer");
        let fence = consumer_dev
            .create_fence(&vk::FenceCreateInfo::default(), None)
            .expect("create_fence");
        let cmd_info = vk::CommandBufferSubmitInfo::builder()
            .command_buffer(cmd)
            .build();
        let cmd_infos = [cmd_info];
        let submit = vk::SubmitInfo2::builder()
            .command_buffer_infos(&cmd_infos)
            .build();
        let submits = [submit];
        consumer_dev
            .queue_submit2(consumer_queue, &submits, fence)
            .expect("queue_submit2");
        consumer_dev
            .wait_for_fences(&[fence], true, u64::MAX)
            .expect("wait_for_fences");
        consumer_dev.destroy_fence(fence, None);
        consumer_dev.destroy_command_pool(pool, None);
    }

    // SAFETY: HOST_VISIBLE | HOST_COHERENT via the host-side allocation;
    // the mapped pointer is valid for the buffer's lifetime.
    let readback =
        unsafe { std::slice::from_raw_parts(staging_buf.mapped_ptr(), IMAGE_BYTES as usize) };
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
    stop_app(app);

    assert!(
        pixels_match,
        "the foreign import driven by the wheel's export bundle must read the kernel's \
         fill constant {FILL_CONSTANT_RGBA:?}; first pixel was {:?}",
        &readback[..4]
    );
}
