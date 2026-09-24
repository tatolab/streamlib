// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::io::Write as _;
use std::sync::Arc;

use objc2_io_surface::IOSurfaceLockOptions;
use objc2_metal::MTLSharedEvent as _;
use pyo3::prelude::*;
use streamlib_consumer_rhi::ConsumerVulkanDevice;

use super::super::super::{HelperCheckedOutSurface, HelperProcessGpuExchangeClient};
use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;

fn exchange_client_on(share: &SurfaceShareUnderTest) -> Arc<HelperProcessGpuExchangeClient> {
    Python::initialize();
    Python::attach(|python| {
        Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            python.None(),
            share.channel_name_for_the_helper(),
            "helper:texture-check-out-under-test".to_string(),
        ))
    })
}

fn a_vulkan_device_is_available() -> bool {
    match ConsumerVulkanDevice::new() {
        Ok(_) => true,
        Err(unavailable) => {
            let _ = writeln!(
                std::io::stdout(),
                "Skipping test — no consumer Vulkan device: {unavailable}"
            );
            false
        }
    }
}

fn byte_the_producer_wrote_at(row: usize, column: usize) -> u8 {
    (row.wrapping_mul(37) ^ column.wrapping_mul(11)) as u8
}

/// The CPU view of a checked-out texture is its IOSurface's own rows at the
/// surface's stride, so what the producer wrote there reads back through the
/// door; dropping the surface signals this side's `consume_done` on the
/// engine's own shared event and gives the checkout lease back.
#[test]
fn a_checked_out_texture_reads_its_iosurface_rows_and_releases_on_its_shared_event() {
    if !a_vulkan_device_is_available() {
        return;
    }
    let (width, height) = (13, 5);
    let share = SurfaceShareUnderTest::start("texture-check-out");
    let Some(registered) = share.register_an_iosurface_texture_as("texture-slot", width, height)
    else {
        return;
    };
    let iosurface = &registered.iosurface;
    let row_byte_len = width as usize * 4;
    // SAFETY: a read-write lock on a surface this test allocated.
    assert_eq!(
        unsafe { iosurface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) },
        0
    );
    let base = iosurface.base_address().as_ptr().cast::<u8>();
    for row in 0..height as usize {
        for column in 0..row_byte_len {
            // SAFETY: inside the locked surface, at its own stride.
            unsafe {
                *base.add(row * iosurface.bytes_per_row() + column) =
                    byte_the_producer_wrote_at(row, column);
            }
        }
    }
    // SAFETY: pairs the lock above.
    unsafe { iosurface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };

    let exchange_client = exchange_client_on(&share);
    let checked_out = exchange_client
        .check_out_and_import("texture-slot")
        .expect("the checkout and import");
    let HelperCheckedOutSurface::Texture(texture_surface) = checked_out else {
        panic!("a texture registration checks out as a texture");
    };
    assert_eq!(share.outstanding_claims_on("texture-slot"), 1);

    texture_surface
        .lock_the_iosurface_for_cpu_access_once(true)
        .expect("the CPU door's lock");
    let view = texture_surface
        .host_visible_pixel_plane_view()
        .expect("a single-plane texture has a host view");
    assert_eq!(
        view.base_address, base,
        "the view is the surface's own rows"
    );
    assert_eq!(view.bytes_per_row, iosurface.bytes_per_row() as u64);
    assert_eq!(view.format, streamlib::sdk::rhi::PixelFormat::Rgba32);
    let mismatches = (0..height as usize)
        .flat_map(|row| (0..row_byte_len).map(move |column| (row, column)))
        .filter(|&(row, column)| {
            // SAFETY: inside the surface the view spans, which the lock holds.
            let read = unsafe {
                *view
                    .base_address
                    .add(row * view.bytes_per_row as usize + column)
            };
            read != byte_the_producer_wrote_at(row, column)
        })
        .count();
    assert_eq!(mismatches, 0);
    texture_surface
        .unlock_the_iosurface_after_cpu_access()
        .expect("the CPU door's unlock");

    assert_eq!(registered.consume_done.signaledValue(), 0);
    drop(texture_surface);
    assert_eq!(
        registered.consume_done.signaledValue(),
        1,
        "the release signals consume_done on the engine's own shared event"
    );
    assert_eq!(share.outstanding_claims_on("texture-slot"), 0);
}
