// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cuda::tests::persistent_command_pool` — locks
//! the #620 amortisation invariant for the host-pipeline producer
//! copy path.
//!
//! Before #620 the cuda adapter created and destroyed a
//! `vk::CommandPool` on every `submit_host_copy_image_to_buffer`
//! call, churning `vkCreateCommandPool` + `vkDestroyCommandPool`
//! once per host-pipeline frame. The fix introduced
//! `AdapterPersistentSubmitContext` — a single pool + command buffer
//! + completion fence reset and reused across every submit. This
//! test locks that invariant: after N>1 submits the adapter's
//! `submit_pool_create_count()` must stay at 1.
//!
//! The submit only exercises Vulkan — no CUDA. So the test runs on
//! any Linux box with a Vulkan device + the OPAQUE_FD pool (the cuda
//! adapter's registration shape requires OPAQUE_FD-exportable
//! buffers).
//!
//! Mentally revert the fix (e.g. force the lazy-init branch on every
//! call) and the assertion fires — that's how this test stays
//! load-bearing rather than feel-good.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::{PixelFormat, TextureFormat};
use streamlib::host_rhi::{
    HostVulkanDevice, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore,
};
use streamlib_adapter_abi::SurfaceId;
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};

const W: u32 = 32;
const H: u32 = 32;
const SURFACE_ID: SurfaceId = 0xC0DE_0001;

fn try_init_gpu() -> Option<Arc<GpuContext>> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_cuda=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok().map(Arc::new)
}

#[test]
fn persistent_pool_count_stays_at_one_across_repeated_submits() {
    let Some(gpu) = try_init_gpu() else {
        println!("cuda persistent_pool: skipping — no Vulkan device available");
        return;
    };
    let host_device: Arc<HostVulkanDevice> = Arc::clone(gpu.device().vulkan_device());
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!(
            "cuda persistent_pool: skipping — OPAQUE_FD buffer pool unavailable on this driver"
        );
        return;
    }

    // Allocate the OPAQUE_FD-exportable staging buffer + timeline the
    // cuda adapter's registration shape requires. DEVICE_LOCAL is the
    // host-pipeline producer flow `submit_host_copy_image_to_buffer`
    // is built for; HOST_VISIBLE would also work but DEVICE_LOCAL is
    // the on-path scenario for this hot-path test.
    let pixel_buffer = match HostVulkanPixelBuffer::new_opaque_fd_export_device_local(
        &host_device,
        W,
        H,
        4,
        PixelFormat::Bgra32,
    ) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            println!(
                "cuda persistent_pool: new_opaque_fd_export_device_local failed: {e} — skipping"
            );
            return;
        }
    };
    let timeline = match HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            println!("cuda persistent_pool: new_exportable timeline failed: {e} — skipping");
            return;
        }
    };

    // Source `VkImage` for the copy. The host pipeline producer path
    // takes any DMA-BUF render-target image; this is the same shape
    // the camera-to-cuda example registers.
    let source_texture =
        match gpu.acquire_render_target_dma_buf_image(W, H, TextureFormat::Bgra8Unorm) {
            Ok(t) => t,
            Err(e) => {
                println!(
                    "cuda persistent_pool: acquire_render_target_dma_buf_image failed: {e} — skipping"
                );
                return;
            }
        };
    let texture_arc = Arc::clone(source_texture.vulkan_inner());

    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    adapter
        .register_host_surface(
            SURFACE_ID,
            HostSurfaceRegistration {
                pixel_buffer,
                timeline,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register_host_surface");

    // Pre-condition: lazy-init hasn't fired yet.
    assert_eq!(
        adapter.submit_pool_create_count(),
        0,
        "adapter should not have created its persistent pool before the first submit"
    );

    // First submit materialises the pool exactly once.
    adapter
        .submit_host_copy_image_to_buffer(SURFACE_ID, texture_arc.as_ref(), VulkanLayout::GENERAL)
        .expect("first submit_host_copy_image_to_buffer");
    assert_eq!(
        adapter.submit_pool_create_count(),
        1,
        "first submit must materialise the persistent pool exactly once"
    );

    // N additional submits must not grow the live pool count.
    let cycles = 32usize;
    for i in 0..cycles {
        adapter
            .submit_host_copy_image_to_buffer(
                SURFACE_ID,
                texture_arc.as_ref(),
                VulkanLayout::GENERAL,
            )
            .unwrap_or_else(|e| panic!("submit cycle {i}: {e:?}"));
    }
    assert_eq!(
        adapter.submit_pool_create_count(),
        1,
        "after {cycles} additional submits, pool count grew — \
         the persistent pool is being re-created per submit"
    );
}
