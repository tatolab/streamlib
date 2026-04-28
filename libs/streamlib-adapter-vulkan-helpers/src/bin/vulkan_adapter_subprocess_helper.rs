// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess test helper for round-trip and crash-mid-write tests.
//!
//! Receives DMA-BUF fds + a timeline-semaphore opaque-fd via SCM_RIGHTS
//! over a socketpair the parent passed in `STREAMLIB_HELPER_SOCKET_FD`.
//! Imports both into a fresh `VkDevice` (this process's own — sidestepping
//! the dual-VkDevice crash because no GPU work is active in the parent
//! when the subprocess starts), then performs the role specified in
//! argv[1]:
//!
//! - `read`        — wait on timeline `wait_value`, vkCmdCopyImageToBuffer
//!                   into a staging VkBuffer, send the bytes back over
//!                   the socketpair, signal `wait_value + 1`.
//! - `write`       — wait on `wait_value`, vkCmdClearColorImage with the
//!                   provided RGBA tuple, signal `wait_value + 1`.
//! - `wait-only`   — wait, signal +1, exit. Used by the concurrent-reads
//!                   test (no GPU work, just contention shape).
//! - `crash-mid-write` — begin a write (acquire timeline value + start
//!                   command submission), then `abort()` mid-flight.
//!                   Verifies host-side cleanup via the EPOLLHUP
//!                   watchdog or `SubprocessCrashHarness`.

#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::Arc;

use streamlib::host_rhi::{HostVulkanDevice, HostVulkanTexture, HostVulkanTimelineSemaphore};
use streamlib::core::rhi::TextureFormat;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

#[derive(Debug, serde::Deserialize)]
struct HelperRequest {
    #[allow(dead_code)] // role is dispatched on argv[1]; this echoes it for debug
    role: String,
    width: u32,
    height: u32,
    drm_format_modifier: u64,
    plane_offsets: Vec<u64>,
    plane_strides: Vec<u64>,
    allocation_size: u64,
    /// Timeline value the helper waits on.
    wait_value: u64,
    /// For `write`: 4 bytes of clear color.
    clear_color: Option<[f32; 4]>,
}

#[derive(Debug, serde::Serialize)]
struct HelperResponse {
    ok: bool,
    note: String,
    bytes_read: Option<Vec<u8>>,
}

fn die(socket: Option<&UnixStream>, msg: String) -> ExitCode {
    tracing::error!(error = %msg, "[helper] FATAL");
    if let Some(s) = socket {
        let resp = HelperResponse {
            ok: false,
            note: msg,
            bytes_read: None,
        };
        let body = serde_json::to_vec(&resp).unwrap_or_default();
        let _ = streamlib_surface_client::send_message_with_fds(s, &body, &[]);
    }
    ExitCode::from(1)
}

fn run() -> ExitCode {
    let role = std::env::args().nth(1).unwrap_or_else(|| "wait-only".to_string());

    let sock_fd_str = match std::env::var("STREAMLIB_HELPER_SOCKET_FD") {
        Ok(v) => v,
        Err(_) => return die(None, "STREAMLIB_HELPER_SOCKET_FD unset".into()),
    };
    let sock_fd: RawFd = match sock_fd_str.parse() {
        Ok(v) => v,
        Err(_) => return die(None, "STREAMLIB_HELPER_SOCKET_FD not an integer".into()),
    };
    let socket = unsafe { UnixStream::from_raw_fd(sock_fd) };

    // Read length-prefixed JSON + fds via SCM_RIGHTS.
    let mut len_buf = [0u8; 4];
    let mut total = 0;
    while total < 4 {
        let n = unsafe {
            libc::read(
                socket.as_raw_fd(),
                len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                4 - total,
            )
        };
        if n <= 0 {
            return die(Some(&socket), "read length prefix failed".into());
        }
        total += n as usize;
    }
    let msg_len = u32::from_be_bytes(len_buf) as usize;
    let (payload, fds) = match streamlib_surface_client::recv_message_with_fds(
        &socket,
        msg_len,
        streamlib_surface_client::MAX_DMA_BUF_PLANES + 1,
    ) {
        Ok(p) => p,
        Err(e) => return die(Some(&socket), format!("recv_message_with_fds: {e}")),
    };

    let req: HelperRequest = match serde_json::from_slice(&payload) {
        Ok(r) => r,
        Err(e) => return die(Some(&socket), format!("parse request: {e}")),
    };

    if fds.len() < 2 {
        return die(
            Some(&socket),
            format!("expected ≥2 fds (DMA-BUF + sync), got {}", fds.len()),
        );
    }
    let sync_fd = *fds.last().unwrap();
    let dma_buf_fds: Vec<RawFd> = fds[..fds.len() - 1].to_vec();

    // Build our own VkDevice. The parent has no GPU work in flight when
    // it spawns us, so the dual-VkDevice crash on NVIDIA does not apply.
    let device = match HostVulkanDevice::new() {
        Ok(d) => Arc::new(d),
        Err(e) => return die(Some(&socket), format!("HostVulkanDevice::new: {e}")),
    };

    // Import VkImage with the host-chosen DRM modifier.
    let texture = match HostVulkanTexture::import_render_target_dma_buf(
        &device,
        &dma_buf_fds,
        &req.plane_offsets,
        &req.plane_strides,
        req.drm_format_modifier,
        req.width,
        req.height,
        TextureFormat::Bgra8Unorm,
        req.allocation_size,
    ) {
        Ok(t) => t,
        Err(e) => return die(Some(&socket), format!("import_render_target_dma_buf: {e}")),
    };

    // Import the timeline semaphore. Vulkan takes ownership of `sync_fd`
    // on success; we close every other fd.
    let timeline = match HostVulkanTimelineSemaphore::from_imported_opaque_fd(
        device.device(),
        sync_fd,
    ) {
        Ok(t) => t,
        Err(e) => {
            for f in &dma_buf_fds {
                unsafe { libc::close(*f) };
            }
            return die(Some(&socket), format!("import sync_fd: {e}"));
        }
    };

    // dma_buf_fds were dup'd by the kernel during SCM_RIGHTS; the
    // VkImage import does NOT take ownership in our import path (the
    // memory_fd helper inside HostVulkanDevice dups internally). Close our
    // copies — without this they leak across the helper's lifetime.
    for f in &dma_buf_fds {
        unsafe { libc::close(*f) };
    }

    // Wait for the parent to finish writing / hand off.
    if let Err(e) = timeline.wait(req.wait_value, 5_000_000_000u64) {
        return die(
            Some(&socket),
            format!("timeline.wait({}): {e}", req.wait_value),
        );
    }

    let response = match role.as_str() {
        "wait-only" => {
            // Just signal the next value and exit. Used by contention tests.
            if let Err(e) = timeline.signal_host(req.wait_value + 1) {
                return die(Some(&socket), format!("signal_host: {e}"));
            }
            HelperResponse {
                ok: true,
                note: "wait-only complete".into(),
                bytes_read: None,
            }
        }
        "write" => {
            let color = req.clear_color.unwrap_or([1.0, 0.5, 0.25, 1.0]);
            if let Err(e) = subprocess_clear_image(
                &device,
                texture.image().expect("imported image"),
                color,
            ) {
                return die(Some(&socket), format!("subprocess_clear_image: {e}"));
            }
            if let Err(e) = timeline.signal_host(req.wait_value + 1) {
                return die(Some(&socket), format!("signal_host post-write: {e}"));
            }
            HelperResponse {
                ok: true,
                note: format!("wrote clear color {color:?}"),
                bytes_read: None,
            }
        }
        "read" => {
            let bytes = match subprocess_readback_image(
                &device,
                texture.image().expect("imported image"),
                req.width,
                req.height,
            ) {
                Ok(b) => b,
                Err(e) => return die(Some(&socket), format!("subprocess_readback_image: {e}")),
            };
            if let Err(e) = timeline.signal_host(req.wait_value + 1) {
                return die(Some(&socket), format!("signal_host post-read: {e}"));
            }
            HelperResponse {
                ok: true,
                note: format!("read {} bytes", bytes.len()),
                bytes_read: Some(bytes),
            }
        }
        "crash-mid-write" => {
            // Spec'd to crash mid-flight; never reach the response send.
            std::process::abort();
        }
        other => {
            return die(Some(&socket), format!("unknown role {other}"));
        }
    };

    let body = serde_json::to_vec(&response).unwrap_or_default();
    if let Err(e) = streamlib_surface_client::send_message_with_fds(&socket, &body, &[]) {
        tracing::error!(error = %e, "[helper] send response failed");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Subprocess `vkCmdClearColorImage` — equivalent to a plain GPU write.
fn subprocess_clear_image(
    device: &Arc<HostVulkanDevice>,
    image: vk::Image,
    color: [f32; 4],
) -> streamlib::core::Result<()> {
    use streamlib::core::StreamError;
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();

    let pool = unsafe {
        dev.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .build(),
            None,
        )
    }
    .map_err(|e| StreamError::GpuError(format!("create_command_pool: {e}")))?;

    let cmd = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .map_err(|e| StreamError::GpuError(format!("allocate_command_buffers: {e}")))?[0];

    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .map_err(|e| StreamError::GpuError(format!("begin_command_buffer: {e}")))?;

    // UNDEFINED → TRANSFER_DST_OPTIMAL barrier so vkCmdClearColorImage
    // sees a known layout.
    let barrier = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let barriers = [barrier];
    let dep = vk::DependencyInfo::builder()
        .image_memory_barriers(&barriers)
        .build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let clear_value = vk::ClearColorValue {
        float32: color,
    };
    let range = vk::ImageSubresourceRange::builder()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
        .build();
    let ranges = [range];
    unsafe {
        dev.cmd_clear_color_image(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &clear_value,
            &ranges,
        )
    };

    unsafe { dev.end_command_buffer(cmd) }
        .map_err(|e| StreamError::GpuError(format!("end_command_buffer: {e}")))?;

    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let submits = [vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }?;
    unsafe { dev.queue_wait_idle(queue) }
        .map_err(|e| StreamError::GpuError(format!("queue_wait_idle: {e}")))?;
    unsafe { dev.destroy_command_pool(pool, None) };
    Ok(())
}

/// Subprocess readback — `vkCmdCopyImageToBuffer` into a staging buffer,
/// then map and read.
fn subprocess_readback_image(
    device: &Arc<HostVulkanDevice>,
    image: vk::Image,
    width: u32,
    height: u32,
) -> streamlib::core::Result<Vec<u8>> {
    use streamlib::core::StreamError;
    let dev = device.device();
    let queue = device.queue();
    let qf = device.queue_family_index();

    let bytes = (width as u64) * (height as u64) * 4;

    // Staging buffer (HOST_VISIBLE | HOST_COHERENT).
    let buffer_info = vk::BufferCreateInfo::builder()
        .size(bytes)
        .usage(vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .build();
    let buffer = unsafe { dev.create_buffer(&buffer_info, None) }
        .map_err(|e| StreamError::GpuError(format!("create_buffer: {e}")))?;
    let mem_req = unsafe { dev.get_buffer_memory_requirements(buffer) };

    // Find a host-visible memory type from the device's physical
    // memory properties.
    let inst = device.instance();
    let phys = device.physical_device();
    let mem_props = unsafe { inst.get_physical_device_memory_properties(phys) };
    let needed = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let mem_type_idx = (0..mem_props.memory_type_count)
        .find(|i| {
            let bit = 1u32 << i;
            (mem_req.memory_type_bits & bit) != 0
                && mem_props.memory_types[*i as usize].property_flags.contains(needed)
        })
        .ok_or_else(|| StreamError::GpuError("no host-visible memory type".into()))?;

    let alloc_info = vk::MemoryAllocateInfo::builder()
        .allocation_size(mem_req.size)
        .memory_type_index(mem_type_idx)
        .build();
    let mem = unsafe { dev.allocate_memory(&alloc_info, None) }
        .map_err(|e| {
            unsafe { dev.destroy_buffer(buffer, None) };
            StreamError::GpuError(format!("allocate_memory: {e}"))
        })?;
    unsafe { dev.bind_buffer_memory(buffer, mem, 0) }
        .map_err(|e| {
            unsafe { dev.free_memory(mem, None) };
            unsafe { dev.destroy_buffer(buffer, None) };
            StreamError::GpuError(format!("bind_buffer_memory: {e}"))
        })?;

    // Command buffer: transition image to TRANSFER_SRC, copy to buffer.
    let pool = unsafe {
        dev.create_command_pool(
            &vk::CommandPoolCreateInfo::builder()
                .queue_family_index(qf)
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .build(),
            None,
        )
    }
    .map_err(|e| StreamError::GpuError(format!("create_command_pool: {e}")))?;
    let cmd = unsafe {
        dev.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1)
                .build(),
        )
    }
    .map_err(|e| StreamError::GpuError(format!("allocate_command_buffers: {e}")))?[0];

    unsafe {
        dev.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build(),
        )
    }
    .map_err(|e| StreamError::GpuError(format!("begin_command_buffer: {e}")))?;

    // GENERAL is what the parent's adapter left it in; transition to
    // TRANSFER_SRC_OPTIMAL.
    let barrier = vk::ImageMemoryBarrier2::builder()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(qf)
        .dst_queue_family_index(qf)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1)
                .build(),
        )
        .build();
    let barriers = [barrier];
    let dep = vk::DependencyInfo::builder()
        .image_memory_barriers(&barriers)
        .build();
    unsafe { dev.cmd_pipeline_barrier2(cmd, &dep) };

    let copy = vk::BufferImageCopy::builder()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(
            vk::ImageSubresourceLayers::builder()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1)
                .build(),
        )
        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
        .image_extent(vk::Extent3D { width, height, depth: 1 })
        .build();
    let regions = [copy];
    unsafe {
        dev.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer,
            &regions,
        )
    };

    unsafe { dev.end_command_buffer(cmd) }
        .map_err(|e| StreamError::GpuError(format!("end_command_buffer: {e}")))?;

    let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
        .command_buffer(cmd)
        .build()];
    let submits = [vk::SubmitInfo2::builder()
        .command_buffer_infos(&cmd_infos)
        .build()];
    unsafe { device.submit_to_queue(queue, &submits, vk::Fence::null()) }?;
    unsafe { dev.queue_wait_idle(queue) }
        .map_err(|e| StreamError::GpuError(format!("queue_wait_idle: {e}")))?;

    let mapped = unsafe { dev.map_memory(mem, 0, bytes, vk::MemoryMapFlags::empty()) }
        .map_err(|e| StreamError::GpuError(format!("map_memory: {e}")))?;
    let slice = unsafe { std::slice::from_raw_parts(mapped as *const u8, bytes as usize) };
    let out = slice.to_vec();
    unsafe { dev.unmap_memory(mem) };
    unsafe { dev.destroy_command_pool(pool, None) };
    unsafe { dev.destroy_buffer(buffer, None) };
    unsafe { dev.free_memory(mem, None) };
    Ok(out)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    run()
}
