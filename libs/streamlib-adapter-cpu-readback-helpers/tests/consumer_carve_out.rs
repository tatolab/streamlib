// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Carve-out semantic test for `streamlib-adapter-cpu-readback`'s
//! Path E shape (#562).
//!
//! Validates the import-side carve-out the cdylibs ride: a host-side
//! `vkCmdCopyImageToBuffer` lands its bytes in a HOST_VISIBLE staging
//! `VkBuffer`; the buffer's DMA-BUF FD is exported and re-imported on
//! a separate `ConsumerVulkanDevice`; the consumer sees the same
//! bytes through its own mapped pointer. This is the primitive the
//! polyglot blur example exercises end-to-end through the cdylib +
//! escalate IPC; the helper crate exercises the same primitive
//! in-process so a regression in `ConsumerVulkanPixelBuffer::from_dma_buf_fd`
//! lights up here without needing a full subprocess spawn.
//!
//! Same `#[serial]` discipline as the adapter-vulkan helper:
//! concurrent `VkInstance` / `VkDevice` creation on NVIDIA Linux trips
//! the dual-device crash, and the test serializes against any other
//! Vulkan-touching test in this binary.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use serial_test::serial;
use streamlib::core::context::GpuContext;
use streamlib::core::rhi::{PixelFormat, TextureFormat};
use streamlib::host_rhi::{
    HostMarker, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore,
};
use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_cpu_readback::{
    CpuReadbackCopyTrigger, CpuReadbackSurfaceAdapter, HostSurfaceRegistration,
    InProcessCpuReadbackCopyTrigger, VulkanLayout,
};
use streamlib_consumer_rhi::{
    ConsumerVulkanDevice, ConsumerVulkanPixelBuffer, ConsumerVulkanTimelineSemaphore,
    PixelFormat as ConsumerPixelFormat,
};

const W: u32 = 32;
const H: u32 = 32;
const SURFACE_ID: u64 = 0xCA52_0001;

fn try_init_gpu() -> Option<Arc<GpuContext>> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_cpu_readback=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok().map(Arc::new)
}

#[test]
#[serial]
fn host_image_to_consumer_staging_byte_equal_round_trip() {
    let Some(gpu) = try_init_gpu() else {
        println!("carve-out round-trip: no Vulkan device — skipping");
        return;
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());

    // Allocate the host source render-target VkImage.
    let stream_texture =
        match gpu.acquire_render_target_dma_buf_image(W, H, TextureFormat::Bgra8Unorm) {
            Ok(t) => t,
            Err(e) => {
                println!(
                    "carve-out round-trip: acquire_render_target_dma_buf_image failed: {e} — skipping"
                );
                return;
            }
        };
    let texture_arc = Arc::clone(stream_texture.vulkan_inner());

    // Allocate the host-side HOST_VISIBLE staging buffer. We keep an
    // independent Arc so we can export its DMA-BUF FD AFTER
    // registration without unwrapping the registration's vec.
    let staging = HostVulkanPixelBuffer::new(&host_device, W, H, 4, PixelFormat::Bgra32)
        .expect("HostVulkanPixelBuffer::new");
    let staging_arc = Arc::new(staging);

    // Allocate the exportable timeline. Keep our own Arc so we can
    // export its OPAQUE_FD post-registration.
    let timeline_arc = Arc::new(
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .expect("HostVulkanTimelineSemaphore::new_exportable"),
    );

    // Build the host adapter with the in-process trigger.
    let trigger = Arc::new(InProcessCpuReadbackCopyTrigger::new(Arc::clone(
        &host_device,
    ))) as Arc<dyn CpuReadbackCopyTrigger<HostMarker>>;
    let host_adapter = Arc::new(CpuReadbackSurfaceAdapter::new(
        Arc::clone(&host_device),
        trigger,
    ));
    host_adapter
        .register_host_surface(
            SURFACE_ID,
            HostSurfaceRegistration::<HostMarker> {
                texture: Some(texture_arc),
                staging_planes: vec![Arc::clone(&staging_arc)],
                timeline: Arc::clone(&timeline_arc),
                initial_image_layout: VulkanLayout::UNDEFINED,
                format: SurfaceFormat::Bgra8,
                width: W,
                height: H,
            },
        )
        .expect("register_host_surface");

    // Phase 1 — write a known pattern through the host adapter so the
    // pattern lands in the host source `VkImage`.
    let surface = StreamlibSurface::new(
        SURFACE_ID,
        W,
        H,
        SurfaceFormat::Bgra8,
        SurfaceUsage::CPU_READBACK,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );
    let pattern: Vec<u8> = (0..(W as usize * H as usize * 4))
        .map(|i| ((i * 31) & 0xFF) as u8)
        .collect();
    {
        let mut wguard = host_adapter
            .acquire_write(&surface)
            .expect("host acquire_write");
        wguard
            .view_mut()
            .plane_mut(0)
            .bytes_mut()
            .copy_from_slice(&pattern);
        // Drop runs `vkCmdCopyBufferToImage` — pattern lands in the
        // host VkImage.
    }

    // Phase 2 — re-acquire for read so the host runs
    // `vkCmdCopyImageToBuffer` against the staging buffer. After this,
    // `staging_arc` holds bytes equal to `pattern` and the timeline
    // has been signaled at least to value 3:
    //   acquire_write   trigger.run_copy_image_to_buffer  -> signals 1
    //   end_write_access trigger.run_copy_buffer_to_image -> signals 2
    //   acquire_read    trigger.run_copy_image_to_buffer  -> signals 3
    // Wait on value 1 below — by the time we reach the consumer-side
    // wait, the host has long since signaled past it, so a working
    // `ConsumerVulkanTimelineSemaphore::wait` returns immediately.
    // A regression that broke import-side semaphore wait would
    // surface as a 5-second timeout on the `expect` below.
    {
        let rguard = host_adapter
            .acquire_read(&surface)
            .expect("host acquire_read");
        // Sanity: the host's view sees the pattern.
        assert_eq!(
            rguard.view().plane(0).bytes(),
            pattern.as_slice(),
            "host adapter's read view must observe the written pattern"
        );
    }
    // First value the trigger ever signals on this timeline — the
    // host has signaled at least 3 by now, so `wait(1, ...)` is
    // unconditionally past.
    let consumer_wait_value: u64 = 1;

    // Phase 3 — export the host staging buffer's DMA-BUF FD and the
    // timeline's OPAQUE_FD; import them on a fresh
    // `ConsumerVulkanDevice` (separate VkDevice, separate VkInstance —
    // the same boundary the cdylibs cross).
    let dma_buf_fd = staging_arc
        .export_dma_buf_fd()
        .expect("HostVulkanPixelBuffer::export_dma_buf_fd");
    let sync_fd = timeline_arc
        .export_opaque_fd()
        .expect("HostVulkanTimelineSemaphore::export_opaque_fd");

    let consumer = match ConsumerVulkanDevice::new() {
        Ok(d) => Arc::new(d),
        Err(e) => {
            println!("carve-out round-trip: ConsumerVulkanDevice::new failed: {e} — skipping");
            unsafe {
                libc::close(dma_buf_fd);
                libc::close(sync_fd);
            }
            return;
        }
    };
    let consumer_staging = ConsumerVulkanPixelBuffer::from_dma_buf_fd(
        &consumer,
        dma_buf_fd,
        W,
        H,
        4,
        ConsumerPixelFormat::Bgra32,
        (W as u64) * (H as u64) * 4,
    )
    .expect("ConsumerVulkanPixelBuffer::from_dma_buf_fd");
    let consumer_timeline =
        ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(&consumer, sync_fd)
            .expect("ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd");

    // Wait on the imported timeline at value 1 — the host has
    // signaled at least 3 by now, so this returns immediately. A
    // 5-second timeout would indicate a real regression in
    // ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd or
    // wait, so use `expect` rather than swallowing the error.
    consumer_timeline
        .wait(consumer_wait_value, 5_000_000_000)
        .expect(
            "ConsumerVulkanTimelineSemaphore::wait timed out at value 1 — \
             the host signaled past 1 before the consumer imported the \
             timeline; either the OPAQUE_FD import is broken or the \
             consumer-side wait is not reading the right kernel object",
        );

    // Phase 4 — read the consumer's mapped bytes; assert byte-equal.
    let consumer_bytes = unsafe {
        std::slice::from_raw_parts(
            consumer_staging.mapped_ptr(),
            consumer_staging.size() as usize,
        )
    };
    assert_eq!(
        consumer_bytes,
        pattern.as_slice(),
        "consumer's mapped pointer over the imported DMA-BUF must observe \
         the same bytes the host's vkCmdCopyImageToBuffer wrote into the \
         shared staging buffer"
    );
}
