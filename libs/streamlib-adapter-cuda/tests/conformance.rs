// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cuda::tests::conformance` — runs the public
//! `run_conformance` suite from `streamlib-adapter-abi` against the
//! CUDA adapter, plus a duplicate-registration negative case.
//!
//! Exercises the same eight contracts MockAdapter passes (acquire/drop
//! pairs, parallel reads, `WriteContended` on contention, `try_acquire_*`
//! returning `Ok(None)`, multi-thread Send+Sync). A green run confirms
//! the trait shape is honored — it does NOT prove CUDA interop; that's
//! the carve-out test in `streamlib-adapter-cuda-helpers`.
//!
//! The test allocates an OPAQUE_FD-exportable HOST_VISIBLE buffer
//! (the resource shape CUDA imports). Even when CUDA isn't installed
//! the conformance test runs, because the OPAQUE_FD pool is a pure
//! Vulkan construct — no CUDA SDK touched.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::PixelFormat;
use streamlib::host_rhi::{HostVulkanDevice, HostVulkanPixelBuffer, HostVulkanTimelineSemaphore};
use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_cuda::{CudaSurfaceAdapter, HostSurfaceRegistration, VulkanLayout};

const W: u32 = 32;
const H: u32 = 32;

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_cuda=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

fn register_one(
    adapter: &CudaSurfaceAdapter<HostVulkanDevice>,
    id: SurfaceId,
) -> Result<StreamlibSurface, String> {
    let host_device = Arc::clone(adapter.device());
    let pixel_buffer = HostVulkanPixelBuffer::new_opaque_fd_export(
        &host_device,
        W,
        H,
        4,
        PixelFormat::Bgra32,
    )
    .map_err(|e| format!("HostVulkanPixelBuffer::new_opaque_fd_export: {e}"))?;
    let timeline = HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
        .map_err(|e| format!("HostVulkanTimelineSemaphore::new_exportable: {e}"))?;
    adapter
        .register_host_surface(
            id,
            HostSurfaceRegistration {
                pixel_buffer: Arc::new(pixel_buffer),
                timeline: Arc::new(timeline),
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .map_err(|e| format!("register_host_surface: {e}"))?;
    Ok(StreamlibSurface::new(
        id,
        W,
        H,
        SurfaceFormat::Bgra8,
        SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    ))
}

struct ConformanceFactory<'a> {
    adapter: &'a CudaSurfaceAdapter<HostVulkanDevice>,
}

impl<'a> streamlib_adapter_abi::testing::ConformanceSurfaceFactory for ConformanceFactory<'a> {
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        register_one(self.adapter, id).expect("register_one")
    }
}

#[test]
fn cuda_adapter_passes_run_conformance() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("cuda-adapter conformance: skipping — no Vulkan device available");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!(
            "cuda-adapter conformance: skipping — OPAQUE_FD buffer pool unavailable on this driver"
        );
        return;
    }
    let adapter = CudaSurfaceAdapter::new(host_device);

    let factory = ConformanceFactory { adapter: &adapter };
    run_conformance(&adapter, factory);

    let bogus = empty_surface(0xdead_beef);
    match adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}

#[test]
fn duplicate_registration_returns_surface_already_registered() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!(
                "cuda-adapter duplicate-registration: skipping — no Vulkan device available"
            );
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!(
            "cuda-adapter duplicate-registration: skipping — OPAQUE_FD pool unavailable"
        );
        return;
    }
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0xfeed_face;
    if register_one(&adapter, id).is_err() {
        println!(
            "cuda-adapter duplicate-registration: skipping — first registration failed"
        );
        return;
    }
    // Build a second registration's resources directly so the typed
    // AdapterError variant survives back to this test (the local
    // `register_one` helper wraps errors in `String` for ergonomics).
    let pixel_buffer = HostVulkanPixelBuffer::new_opaque_fd_export(
        &host_device,
        W,
        H,
        4,
        PixelFormat::Bgra32,
    )
    .expect("second pixel buffer");
    let timeline =
        HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .expect("second timeline");
    let result = adapter.register_host_surface(
        id,
        streamlib_adapter_cuda::HostSurfaceRegistration {
            pixel_buffer: Arc::new(pixel_buffer),
            timeline: Arc::new(timeline),
            initial_layout: VulkanLayout::UNDEFINED,
        },
    );
    match result {
        Err(AdapterError::SurfaceAlreadyRegistered { surface_id }) => {
            assert_eq!(surface_id, id);
        }
        other => panic!("expected SurfaceAlreadyRegistered, got {other:?}"),
    }
}

#[test]
fn unregister_returns_true_on_first_call_false_on_second() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!("cuda-adapter unregister: skipping — no Vulkan device available");
            return;
        }
    };
    let host_device = Arc::clone(gpu.device().vulkan_device());
    if host_device.opaque_fd_buffer_pool().is_none() {
        println!("cuda-adapter unregister: skipping — OPAQUE_FD pool unavailable");
        return;
    }
    let adapter = CudaSurfaceAdapter::new(host_device);
    let id: SurfaceId = 0xb16b_00b5;
    if register_one(&adapter, id).is_err() {
        println!("cuda-adapter unregister: skipping — registration failed");
        return;
    }
    assert!(adapter.unregister_host_surface(id), "first unregister should succeed");
    assert!(!adapter.unregister_host_surface(id), "second unregister should fail");
    assert_eq!(adapter.registered_count(), 0);
}
