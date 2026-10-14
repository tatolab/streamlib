// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{FromRawFd as _, OwnedFd};

use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};

use super::*;
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

const PIXEL_BUFFER_WIDTH: u32 = 64;
const PIXEL_BUFFER_HEIGHT: u32 = 16;
const PIXEL_BUFFER_BYTE_SIZE: u64 = 64 * 4 * 16;
const ENGINE_WRITTEN_STAMP: u8 = 0xA7;

fn exchange_client_on(share: &SurfaceShareUnderTest) -> Arc<HelperProcessGpuExchangeClient> {
    Python::initialize();
    Python::attach(|python| {
        Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            share.socket_path.clone(),
            "helper:opaque-fd-pixel-buffer-under-test".to_string(),
        ))
    })
}

fn hex_of_device_uuid(device_uuid: [u8; 16]) -> String {
    device_uuid
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// An engine-exported OPAQUE_FD pixel buffer — what a pooled frame is on a
/// device without DMA-BUF export — checks out through the helper's pixel arm,
/// maps at the memory type index the exporter states, and reads the bytes the
/// engine wrote. Needs a Vulkan device; skips where none comes up.
#[test]
fn an_opaque_fd_pixel_buffer_imports_at_its_stated_memory_type_index_and_reads_the_engines_bytes() {
    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let exported =
        match HostVulkanBuffer::new_opaque_fd_export(&host_device, PIXEL_BUFFER_BYTE_SIZE) {
            Ok(exported) => exported,
            Err(unavailable) => {
                tracing::warn!(
                    "skipping: no OPAQUE_FD buffer export on this device ({unavailable})"
                );
                return;
            }
        };
    // SAFETY: the export is a live mapping of PIXEL_BUFFER_BYTE_SIZE bytes.
    unsafe {
        std::ptr::write_bytes(
            exported.mapped_ptr(),
            ENGINE_WRITTEN_STAMP,
            PIXEL_BUFFER_BYTE_SIZE as usize,
        )
    };
    let stated_memory_type_index = exported
        .vma_allocation_memory_type_index()
        .expect("the exporter states its memory type index");
    // SAFETY: a fresh fd the export just minted; ours alone.
    let exported_memory_fd =
        unsafe { OwnedFd::from_raw_fd(exported.export_opaque_fd_memory().expect("export")) };

    let share = SurfaceShareUnderTest::start("opaque-pixel-import");
    let exchange_client = exchange_client_on(&share);
    // `register`, the op the engine's own pool slots publish through, carries
    // the OPAQUE_FD contract fields a pixel buffer's import binds by.
    let surface_id = "opaque-fd-pixel-buffer-under-test".to_string();
    let (register_response, _no_fds) = exchange_client
        .surface_share_request_with_fds(
            &serde_json::json!({
                "op": "register",
                "surface_id": surface_id,
                "runtime_id": "helper:opaque-fd-pixel-buffer-under-test",
                "width": PIXEL_BUFFER_WIDTH,
                "height": PIXEL_BUFFER_HEIGHT,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
                "handle_type": "opaque_fd",
                "plane_sizes": [PIXEL_BUFFER_BYTE_SIZE],
                "plane_offsets": [0u64],
                "plane_strides": [PIXEL_BUFFER_BYTE_SIZE / u64::from(PIXEL_BUFFER_HEIGHT)],
                "vk_memory_type_index": stated_memory_type_index,
                "exporting_device_uuid": hex_of_device_uuid(host_device.physical_device_uuid()),
            }),
            &[std::os::fd::AsRawFd::as_raw_fd(&exported_memory_fd)],
        )
        .expect("the register round trip completes");
    drop(exported_memory_fd);
    assert_eq!(
        register_response
            .get("success")
            .and_then(|value| value.as_bool()),
        Some(true),
        "the registration must succeed: {register_response}"
    );

    let HelperCheckedOutSurface::PixelBuffer(pixel_surface) = exchange_client
        .check_out_and_import(&surface_id)
        .expect("a single-plane OPAQUE_FD pixel buffer imports")
    else {
        panic!("a pixel_buffer registration must check out as a pixel buffer");
    };

    let import_device = exchange_client
        .consumer_vulkan_device()
        .expect("the import above created it");
    if import_device.device_is_software() {
        assert!(
            !import_device.supports_dma_buf_memory_import(),
            "a software import device must take this frame with DMA-BUF import off"
        );
    }

    let plane_view = pixel_surface.host_visible_pixel_plane_view();
    assert!(!plane_view.base_address.is_null());
    assert_eq!(
        plane_view.bytes_per_row,
        PIXEL_BUFFER_BYTE_SIZE / u64::from(PIXEL_BUFFER_HEIGHT)
    );
    // SAFETY: the mapping spans the plane's PIXEL_BUFFER_BYTE_SIZE bytes.
    let seen = unsafe {
        std::slice::from_raw_parts(plane_view.base_address, PIXEL_BUFFER_BYTE_SIZE as usize)
    };
    assert!(
        seen.iter().all(|byte| *byte == ENGINE_WRITTEN_STAMP),
        "the helper must map the engine's own allocation, not a fresh one"
    );

    let dma_buf_refusal = pixel_surface
        .export_dma_buf()
        .expect_err("an OPAQUE_FD pixel buffer has no DMA-BUF fd to export")
        .to_string();
    assert!(
        dma_buf_refusal.contains("OPAQUE_FD") && dma_buf_refusal.contains(&surface_id),
        "the refusal must name the flavour and the surface: {dma_buf_refusal:?}"
    );
    let checked_out = HelperCheckedOutSurface::PixelBuffer(pixel_surface);
    let opaque_fd_refusal = checked_out
        .export_opaque_fd()
        .err()
        .expect("export_opaque_fd hands out texture memory only")
        .to_string();
    assert!(
        opaque_fd_refusal.contains("OPAQUE_FD pixel buffer"),
        "the refusal must say what this surface is: {opaque_fd_refusal:?}"
    );
}
