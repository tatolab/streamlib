// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared scaffolding for the round-trip / crash subprocess tests.
//! Pulled in via `#[path = "common.rs"] mod common;` in each test file.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // Each test file uses a different subset.

use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use streamlib::adapter_support::HostVulkanTimelineSemaphore;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::{StreamTexture, TextureFormat};
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState, SurfaceTransportHandle,
    SurfaceUsage,
};
use streamlib_adapter_vulkan::{
    HostSurfaceRegistration, VulkanContext, VulkanLayout, VulkanSurfaceAdapter,
};

pub fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_vulkan=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

pub struct HostFixture {
    pub gpu: GpuContext,
    pub adapter: Arc<VulkanSurfaceAdapter>,
    pub ctx: VulkanContext,
}

impl HostFixture {
    pub fn try_new() -> Option<Self> {
        let gpu = try_init_gpu()?;
        let adapter = Arc::new(VulkanSurfaceAdapter::new(Arc::clone(
            gpu.device().vulkan_device(),
        )));
        let ctx = VulkanContext::new(Arc::clone(&adapter));
        Some(Self { gpu, adapter, ctx })
    }

    pub fn register_surface(
        &self,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
    ) -> RegisteredSurface {
        let texture = self
            .gpu
            .acquire_render_target_dma_buf_image(width, height, TextureFormat::Bgra8Unorm)
            .expect("acquire_render_target_dma_buf_image");
        let timeline = Arc::new(
            HostVulkanTimelineSemaphore::new_exportable(self.adapter.device().device(), 0)
                .expect("exportable timeline"),
        );
        self.adapter
            .register_host_surface(
                surface_id,
                HostSurfaceRegistration {
                    texture: texture.clone(),
                    timeline: Arc::clone(&timeline),
                    initial_layout: VulkanLayout::UNDEFINED,
                },
            )
            .expect("register host surface");
        let descriptor = StreamlibSurface::new(
            surface_id,
            width,
            height,
            SurfaceFormat::Bgra8,
            SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
            SurfaceTransportHandle::empty(),
            SurfaceSyncState::default(),
        );
        RegisteredSurface {
            descriptor,
            texture,
            timeline,
            width,
            height,
        }
    }
}

pub struct RegisteredSurface {
    pub descriptor: StreamlibSurface,
    pub texture: StreamTexture,
    pub timeline: Arc<HostVulkanTimelineSemaphore>,
    pub width: u32,
    pub height: u32,
}

/// Spawn a subprocess running the test helper binary with role `role`.
/// Returns the child handle and the parent end of the socketpair the
/// helper reads its descriptor + fds from.
pub fn spawn_helper(role: &str) -> (Child, UnixStream) {
    let (parent, child) = UnixStream::pair().expect("socketpair");
    // Move the child end's fd into the helper's fd table without
    // closing it on exec; the parent side is normal Rust ownership.
    let child_fd = child.into_raw_fd();
    // Clear FD_CLOEXEC on the inherited fd so it survives `execve`.
    unsafe {
        let flags = libc::fcntl(child_fd, libc::F_GETFD);
        libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
    }

    let bin_path = env!("CARGO_BIN_EXE_vulkan_adapter_subprocess_helper");
    let mut cmd = Command::new(bin_path);
    cmd.arg(role)
        .env("STREAMLIB_HELPER_SOCKET_FD", child_fd.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child_proc = cmd.spawn().expect("spawn subprocess helper");
    // The helper inherited child_fd; close our copy to drop the
    // refcount. The parent UnixStream owns its end.
    unsafe { libc::close(child_fd) };
    (child_proc, parent)
}

/// Send the helper its descriptor + DMA-BUF fds + timeline sync_fd via
/// SCM_RIGHTS. The fds passed in are dup'd by the kernel; the caller
/// retains ownership of their copies (close after send).
pub fn send_helper_request(
    parent: &UnixStream,
    descriptor: &serde_json::Value,
    dma_buf_fds: &[RawFd],
    sync_fd: RawFd,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(descriptor).expect("serialize");
    let mut all_fds: Vec<RawFd> = dma_buf_fds.to_vec();
    all_fds.push(sync_fd);
    // `send_message_with_fds` already prefixes with a 4-byte BE length;
    // the helper reads the prefix, then `recv_message_with_fds` for the
    // payload.
    streamlib_surface_client::send_message_with_fds(parent, &body, &all_fds)
}

/// Read the helper's response (length-prefixed JSON, no fds).
pub fn recv_helper_response(parent: &UnixStream) -> serde_json::Value {
    let mut len_buf = [0u8; 4];
    let mut total = 0;
    while total < 4 {
        let n = unsafe {
            libc::read(
                parent.as_raw_fd(),
                len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                4 - total,
            )
        };
        assert!(n > 0, "read response length: {}", std::io::Error::last_os_error());
        total += n as usize;
    }
    let msg_len = u32::from_be_bytes(len_buf) as usize;
    let (payload, fds) = streamlib_surface_client::recv_message_with_fds(parent, msg_len, 1)
        .expect("recv response");
    for fd in fds {
        unsafe { libc::close(fd) };
    }
    serde_json::from_slice(&payload).expect("parse helper response JSON")
}

/// Build the request descriptor the helper expects, using the
/// metadata stored on a registered surface.
pub fn helper_descriptor(
    role: &str,
    surface: &RegisteredSurface,
    wait_value: u64,
    clear_color: Option<[f32; 4]>,
) -> serde_json::Value {
    let plane_layout = surface
        .texture
        .vulkan_inner()
        .dma_buf_plane_layout()
        .expect("dma_buf_plane_layout");
    let plane_offsets: Vec<u64> = plane_layout.iter().map(|(o, _)| *o).collect();
    let plane_strides: Vec<u64> = plane_layout.iter().map(|(_, s)| *s).collect();
    let modifier = surface.texture.vulkan_inner().chosen_drm_format_modifier();

    serde_json::json!({
        "role": role,
        "width": surface.width,
        "height": surface.height,
        "drm_format_modifier": modifier,
        "plane_offsets": plane_offsets,
        "plane_strides": plane_strides,
        // Adapter buffer size is conservative: width * height * 4 bytes.
        "allocation_size": (surface.width as u64) * (surface.height as u64) * 4,
        "wait_value": wait_value,
        "clear_color": clear_color,
    })
}
