// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::cpu_readable_plane_aware` —
//! exercises the plane-aware shape of [`CpuReadable`] through a
//! `&dyn CpuReadable` so trait-generic callers iterate every plane on
//! multi-plane surfaces (NV12) and observe single-plane semantics on
//! BGRA/RGBA via the trait's defaults.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::{CpuReadable, StreamlibSurface, SurfaceFormat};

use crate::common::HostFixture;

const Y_BYTE: u8 = 0x42;
const UV_BYTES: [u8; 2] = [0x80, 0xC0];

fn register_nv12_or_skip(
    fixture: &HostFixture,
    id: u64,
    width: u32,
    height: u32,
    test_name: &str,
) -> Option<StreamlibSurface> {
    match fixture.try_register_surface_with_format(
        id,
        width,
        height,
        SurfaceFormat::Nv12,
        TextureFormat::Nv12,
    ) {
        Ok(d) => Some(d),
        Err(e) => {
            println!(
                "{test_name}: skipping — host can't allocate NV12 \
                 render-target DMA-BUF on this driver ({e})"
            );
            None
        }
    }
}

#[test]
fn cpu_readable_default_plane_count_is_one() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "cpu_readable_default_plane_count_is_one: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let descriptor = fixture.register_surface(1, 32, 16);
    let pattern: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write");
        let bytes = guard.view_mut().plane_mut(0).bytes_mut();
        for chunk in bytes.chunks_exact_mut(4) {
            chunk.copy_from_slice(&pattern);
        }
    }

    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();
    let dyn_view: &dyn CpuReadable = view;

    assert_eq!(
        dyn_view.plane_count(),
        1,
        "BGRA single-plane surface must report plane_count()=1 through &dyn CpuReadable"
    );
    assert_eq!(
        dyn_view.plane_bytes(0),
        dyn_view.read_bytes(),
        "plane_bytes(0) must alias read_bytes() for single-plane formats"
    );
    assert_eq!(dyn_view.plane_bytes(0).len(), 32 * 16 * 4);
    for chunk in dyn_view.plane_bytes(0).chunks_exact(4) {
        assert_eq!(chunk, &pattern);
    }
}

#[test]
fn cpu_readable_walks_all_planes_for_nv12() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "cpu_readable_walks_all_planes_for_nv12: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let width = 32u32;
    let height = 16u32;
    let descriptor = match register_nv12_or_skip(
        &fixture,
        2,
        width,
        height,
        "cpu_readable_walks_all_planes_for_nv12",
    ) {
        Some(d) => d,
        None => return,
    };

    {
        let mut guard = fixture
            .ctx
            .acquire_write(&descriptor)
            .expect("acquire_write");
        for byte in guard.view_mut().plane_mut(0).bytes_mut().iter_mut() {
            *byte = Y_BYTE;
        }
        for chunk in guard
            .view_mut()
            .plane_mut(1)
            .bytes_mut()
            .chunks_exact_mut(2)
        {
            chunk.copy_from_slice(&UV_BYTES);
        }
    }

    let guard = fixture.ctx.acquire_read(&descriptor).expect("acquire_read");
    let view = guard.view();
    let dyn_view: &dyn CpuReadable = view;

    assert_eq!(
        dyn_view.plane_count(),
        2,
        "NV12 surface must expose 2 planes through &dyn CpuReadable"
    );

    let mut total_bytes_seen = 0usize;
    for plane_index in 0..dyn_view.plane_count() {
        let bytes = dyn_view.plane_bytes(plane_index);
        total_bytes_seen += bytes.len();
        match plane_index {
            0 => {
                assert_eq!(bytes.len() as u32, width * height, "Y plane size");
                for (i, &b) in bytes.iter().enumerate() {
                    assert_eq!(b, Y_BYTE, "Y plane byte {i}: {b:02x}");
                }
            }
            1 => {
                assert_eq!(
                    bytes.len() as u32,
                    (width / 2) * (height / 2) * 2,
                    "UV plane size"
                );
                for (i, chunk) in bytes.chunks_exact(2).enumerate() {
                    assert_eq!(chunk, &UV_BYTES, "UV pair {i}: {chunk:02x?}");
                }
            }
            other => panic!("unexpected plane index {other}"),
        }
    }

    let expected_total = (width * height) + (width / 2) * (height / 2) * 2;
    assert_eq!(
        total_bytes_seen as u32, expected_total,
        "trait-generic walker must visit every plane of an NV12 surface"
    );

    assert_eq!(
        dyn_view.read_bytes(),
        dyn_view.plane_bytes(0),
        "read_bytes() must keep returning plane 0 for back-compat"
    );
}
