// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cuda::tests::image_registration` — exercises the
//! image-flavored registration path (`register_host_image_surface` +
//! `CudaTextureView` / `CudaSurfaceView` + `acquire_texture` /
//! `acquire_surface`) end-to-end through the host RHI's
//! `HostVulkanTexture::new_opaque_fd_export` primitive.
//!
//! The cdylib's CUDA-side mapping (`cudaImportExternalMemory` →
//! `cudaExternalMemoryGetMappedMipmappedArray` →
//! `cudaCreateTextureObject` / `cudaCreateSurfaceObject`) is **out of
//! scope** here — that's the cdylib bring-up work in the polyglot CUDA
//! cdylib image-path issue. This test asserts the adapter's
//! registration shape, the view types' Vulkan-side accessors, the
//! release-on-drop release semantics, and the path-restriction error
//! messages when callers mix buffer and image acquire paths.
//!
//! Test gating:
//! - `target_os = "linux"` — OPAQUE_FD `VkImage` is Linux-only by
//!   construction.
//! - Skips when Vulkan is unavailable OR when
//!   `HostVulkanDevice::opaque_fd_image_pool()` returns `None` (NVIDIA
//!   driver without external-memory; CI Mesa hosts).

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::engine::host_rhi::{
    HostVulkanBuffer, HostVulkanDevice, HostVulkanTexture, HostVulkanTimelineSemaphore,
};
use streamlib::sdk::engine::HostGpuDeviceExt;
use streamlib::sdk::rhi::{TextureDescriptor, TextureFormat as RhiTextureFormat};
use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceSyncState,
    SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_cuda::{
    CudaSurfaceAdapter, HostImageSurfaceRegistration, HostSurfaceRegistration, VulkanLayout,
};
use streamlib_consumer_rhi::TextureFormat as ConsumerTextureFormat;

const W: u32 = 32;
const H: u32 = 32;

fn try_init_gpu() -> Option<Arc<GpuContext>> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("streamlib_adapter_cuda=debug,streamlib=warn")
        .try_init();
    GpuContext::init_for_platform_sync().ok().map(Arc::new)
}

/// Acquire a host device, skip the test if the OPAQUE_FD image pool
/// is missing on this driver.
fn host_device_or_skip(test_name: &str) -> Option<Arc<HostVulkanDevice>> {
    let gpu = try_init_gpu()?;
    let host_device = Arc::clone(gpu.device().vulkan_device());
    if host_device.opaque_fd_image_pool().is_none() {
        println!(
            "{test_name}: skipping — OPAQUE_FD image pool unavailable on this driver"
        );
        return None;
    }
    Some(host_device)
}

fn make_image(
    host_device: &Arc<HostVulkanDevice>,
    format: RhiTextureFormat,
) -> Result<Arc<HostVulkanTexture>, String> {
    let desc = TextureDescriptor::new(W, H, format);
    HostVulkanTexture::new_opaque_fd_export(host_device, &desc)
        .map(Arc::new)
        .map_err(|e| format!("HostVulkanTexture::new_opaque_fd_export({format:?}): {e}"))
}

fn make_timeline(
    host_device: &Arc<HostVulkanDevice>,
) -> Result<Arc<HostVulkanTimelineSemaphore>, String> {
    HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
        .map(Arc::new)
        .map_err(|e| format!("HostVulkanTimelineSemaphore::new_exportable: {e}"))
}

fn make_buffer(host_device: &Arc<HostVulkanDevice>) -> Result<Arc<HostVulkanBuffer>, String> {
    HostVulkanBuffer::new_opaque_fd_export(host_device, (W as u64) * (H as u64) * 4)
        .map(Arc::new)
        .map_err(|e| format!("HostVulkanBuffer::new_opaque_fd_export: {e}"))
}

fn make_surface(id: SurfaceId) -> StreamlibSurface {
    StreamlibSurface::new(
        id,
        W,
        H,
        SurfaceFormat::Bgra8,
        SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    )
}

/// Register an image surface and confirm the registry exposes the
/// texture (not a buffer). Mentally revert `register_host_image_surface`
/// to store in `SurfaceResource::Buffer` and the `surface_texture`
/// lookup fails (returns `None`).
#[test]
fn image_registration_round_trips_through_surface_texture_accessor() {
    let Some(host_device) = host_device_or_skip(
        "image_registration_round_trips_through_surface_texture_accessor",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x4001;
    let texture = make_image(&host_device, RhiTextureFormat::Rgba8Unorm)
        .expect("texture");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_image_surface(
            id,
            HostImageSurfaceRegistration {
                texture: Arc::clone(&texture),
                produce_done: Arc::clone(&produce_done),
                consume_done: Arc::clone(&consume_done),
                initial_layout: VulkanLayout::GENERAL,
            },
        )
        .expect("register");
    assert_eq!(adapter.registered_count(), 1);
    let stored = adapter.surface_texture(id).expect("surface_texture Some");
    assert!(Arc::ptr_eq(&stored, &texture), "texture Arc round-trip");
    let stored_produce_done = adapter
        .surface_produce_done(id)
        .expect("surface_produce_done Some");
    assert!(
        Arc::ptr_eq(&stored_produce_done, &produce_done),
        "produce_done Arc round-trip"
    );
    let stored_consume_done = adapter
        .surface_consume_done(id)
        .expect("surface_consume_done Some");
    assert!(
        Arc::ptr_eq(&stored_consume_done, &consume_done),
        "consume_done Arc round-trip"
    );
    // Buffer accessor must return None for image-flavored registrations.
    assert!(
        adapter.surface_pixel_buffer(id).is_none(),
        "surface_pixel_buffer should be None for image surfaces"
    );
    assert!(adapter.unregister_host_surface(id));
    assert_eq!(adapter.registered_count(), 0);
}

/// Validates each CUDA-mappable format registers cleanly. The host RHI
/// also enforces the same gate at construction; this test asserts the
/// adapter accepts every variant the host can build.
#[test]
fn image_registration_accepts_each_cuda_mappable_format() {
    let Some(host_device) = host_device_or_skip(
        "image_registration_accepts_each_cuda_mappable_format",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    for (i, fmt) in [
        RhiTextureFormat::Rgba8Unorm,
        RhiTextureFormat::Rgba16Float,
        RhiTextureFormat::Rgba32Float,
    ]
    .iter()
    .enumerate()
    {
        let id = 0x5000 + i as SurfaceId;
        let texture = match make_image(&host_device, *fmt) {
            Ok(t) => t,
            Err(e) => {
                println!(
                    "image_registration_accepts_each_cuda_mappable_format: skipping {fmt:?}: {e}"
                );
                continue;
            }
        };
        let produce_done = make_timeline(&host_device).expect("produce_done");
        let consume_done = make_timeline(&host_device).expect("consume_done");
        adapter
            .register_host_image_surface(
                id,
                HostImageSurfaceRegistration {
                    texture,
                    produce_done,
                    consume_done,
                    initial_layout: VulkanLayout::UNDEFINED,
                },
            )
            .unwrap_or_else(|e| panic!("register {fmt:?}: {e:?}"));
    }
}

/// Buffer and image registrations on the same `SurfaceId` collide —
/// the registry is one keyspace regardless of flavor.
#[test]
fn buffer_then_image_registration_returns_surface_already_registered() {
    let Some(host_device) = host_device_or_skip(
        "buffer_then_image_registration_returns_surface_already_registered",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x6001;
    let buffer = make_buffer(&host_device).expect("buffer");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_surface(
            id,
            HostSurfaceRegistration {
                pixel_buffer: buffer,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register buffer");
    let image = make_image(&host_device, RhiTextureFormat::Rgba8Unorm).expect("image");
    let produce_done2 = make_timeline(&host_device).expect("produce_done2");
    let consume_done2 = make_timeline(&host_device).expect("consume_done2");
    let result = adapter.register_host_image_surface(
        id,
        HostImageSurfaceRegistration {
            texture: image,
            produce_done: produce_done2,
            consume_done: consume_done2,
            initial_layout: VulkanLayout::GENERAL,
        },
    );
    match result {
        Err(AdapterError::SurfaceAlreadyRegistered { surface_id }) => {
            assert_eq!(surface_id, id);
        }
        other => panic!("expected SurfaceAlreadyRegistered, got {other:?}"),
    }
}

/// `acquire_texture` on a registered image surface returns a guard
/// whose view exposes the correct `vk::Image`, dimensions, and format.
/// Drop releases the read holder so a subsequent acquire succeeds —
/// mentally revert `Drop for CudaTextureGuard` (skip the
/// `end_read_access` call) and the second acquire fails with a stale
/// holder lingering.
#[test]
fn acquire_texture_returns_view_with_correct_handles_and_releases_on_drop() {
    let Some(host_device) = host_device_or_skip(
        "acquire_texture_returns_view_with_correct_handles_and_releases_on_drop",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x7001;
    let texture = make_image(&host_device, RhiTextureFormat::Rgba16Float).expect("texture");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_image_surface(
            id,
            HostImageSurfaceRegistration {
                texture: Arc::clone(&texture),
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::GENERAL,
            },
        )
        .expect("register");
    let surface = make_surface(id);
    {
        let guard = adapter.acquire_texture(&surface).expect("acquire_texture");
        let view = guard.view();
        let expected_image = streamlib_consumer_rhi::VulkanTextureLike::image(&*texture)
            .expect("texture has VkImage");
        assert_eq!(view.vk_image(), expected_image);
        assert_eq!(view.width(), W);
        assert_eq!(view.height(), H);
        assert_eq!(view.format(), ConsumerTextureFormat::Rgba16Float);
        assert_eq!(guard.surface_id(), id);
    }
    // Guard dropped — the next acquire must succeed because the read
    // holder was released by Drop.
    let _again = adapter.acquire_texture(&surface).expect("re-acquire_texture");
}

/// `acquire_surface` mirrors `acquire_texture` on the writeable side.
#[test]
fn acquire_surface_returns_view_with_correct_handles_and_releases_on_drop() {
    let Some(host_device) = host_device_or_skip(
        "acquire_surface_returns_view_with_correct_handles_and_releases_on_drop",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x7002;
    let texture = make_image(&host_device, RhiTextureFormat::Rgba32Float).expect("texture");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_image_surface(
            id,
            HostImageSurfaceRegistration {
                texture: Arc::clone(&texture),
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::GENERAL,
            },
        )
        .expect("register");
    let surface = make_surface(id);
    {
        let guard = adapter.acquire_surface(&surface).expect("acquire_surface");
        let view = guard.view();
        let expected_image = streamlib_consumer_rhi::VulkanTextureLike::image(&*texture)
            .expect("texture has VkImage");
        assert_eq!(view.vk_image(), expected_image);
        assert_eq!(view.width(), W);
        assert_eq!(view.height(), H);
        assert_eq!(view.format(), ConsumerTextureFormat::Rgba32Float);
        assert_eq!(guard.surface_id(), id);
    }
    let _again = adapter.acquire_surface(&surface).expect("re-acquire_surface");
}

/// Path-restriction: an image-flavored surface rejects `acquire_read`
/// / `acquire_write` (the buffer-path methods) with a typed
/// `BackendRejected` carrying a usage-correction hint. Symmetric to
/// the OpenGL adapter's EXTERNAL_OES-rejects-write hint.
#[test]
fn buffer_path_rejects_image_surface_with_usage_correction_hint() {
    let Some(host_device) = host_device_or_skip(
        "buffer_path_rejects_image_surface_with_usage_correction_hint",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x8001;
    let texture = make_image(&host_device, RhiTextureFormat::Rgba8Unorm).expect("texture");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_image_surface(
            id,
            HostImageSurfaceRegistration {
                texture,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::GENERAL,
            },
        )
        .expect("register");
    let surface = make_surface(id);
    use streamlib_adapter_abi::SurfaceAdapter;
    match adapter.acquire_read(&surface) {
        Err(AdapterError::BackendRejected { reason }) => {
            assert!(
                reason.contains("acquire_texture"),
                "error should suggest acquire_texture; got: {reason}"
            );
        }
        other => panic!("expected BackendRejected, got {other:?}"),
    }
    // `WriteGuard` doesn't impl `Debug`, so we can't `{:?}` the Ok arm;
    // pattern-match explicitly to keep the failure mode informative.
    let write_result = adapter.acquire_write(&surface);
    match write_result {
        Err(AdapterError::BackendRejected { reason }) => {
            assert!(
                reason.contains("acquire_surface"),
                "error should suggest acquire_surface; got: {reason}"
            );
        }
        Err(e) => panic!("expected BackendRejected, got Err({e:?})"),
        Ok(_) => panic!("expected BackendRejected on image surface, got Ok(WriteGuard)"),
    }
}

/// Path-restriction: a buffer-flavored surface rejects
/// `acquire_texture` / `acquire_surface` (the image-path methods)
/// with a typed `BackendRejected` carrying the inverse usage hint.
#[test]
fn image_path_rejects_buffer_surface_with_usage_correction_hint() {
    let Some(host_device) = host_device_or_skip(
        "image_path_rejects_buffer_surface_with_usage_correction_hint",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x8101;
    let buffer = make_buffer(&host_device).expect("buffer");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_surface(
            id,
            HostSurfaceRegistration {
                pixel_buffer: buffer,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::UNDEFINED,
            },
        )
        .expect("register");
    let surface = make_surface(id);
    match adapter.acquire_texture(&surface) {
        Err(AdapterError::BackendRejected { reason }) => {
            assert!(
                reason.contains("acquire_read"),
                "error should suggest acquire_read; got: {reason}"
            );
        }
        other => panic!("expected BackendRejected, got {other:?}"),
    }
    match adapter.acquire_surface(&surface) {
        Err(AdapterError::BackendRejected { reason }) => {
            assert!(
                reason.contains("acquire_write"),
                "error should suggest acquire_write; got: {reason}"
            );
        }
        other => panic!("expected BackendRejected, got {other:?}"),
    }
}

/// `try_acquire_surface` returns `Ok(None)` (rather than blocking) when
/// the surface is already held for read. Mirrors the buffer-path
/// `try_acquire_write` contention behavior.
#[test]
fn try_acquire_surface_returns_none_on_reader_contention() {
    let Some(host_device) = host_device_or_skip(
        "try_acquire_surface_returns_none_on_reader_contention",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x9001;
    let texture = make_image(&host_device, RhiTextureFormat::Rgba8Unorm).expect("texture");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_image_surface(
            id,
            HostImageSurfaceRegistration {
                texture,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::GENERAL,
            },
        )
        .expect("register");
    let surface = make_surface(id);
    let _reader = adapter.acquire_texture(&surface).expect("acquire_texture");
    let result = adapter.try_acquire_surface(&surface);
    match result {
        Ok(None) => {}
        Ok(Some(_)) => panic!("expected Ok(None) on reader contention, got Ok(Some(_))"),
        Err(e) => panic!("expected Ok(None) on reader contention, got Err({e:?})"),
    }
}

/// `try_acquire_texture` returns `Ok(None)` when the surface is held
/// for write.
#[test]
fn try_acquire_texture_returns_none_on_writer_contention() {
    let Some(host_device) = host_device_or_skip(
        "try_acquire_texture_returns_none_on_writer_contention",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::new(Arc::clone(&host_device));
    let id: SurfaceId = 0x9002;
    let texture = make_image(&host_device, RhiTextureFormat::Rgba8Unorm).expect("texture");
    let produce_done = make_timeline(&host_device).expect("produce_done");
    let consume_done = make_timeline(&host_device).expect("consume_done");
    adapter
        .register_host_image_surface(
            id,
            HostImageSurfaceRegistration {
                texture,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout::GENERAL,
            },
        )
        .expect("register");
    let surface = make_surface(id);
    let _writer = adapter.acquire_surface(&surface).expect("acquire_surface");
    match adapter.try_acquire_texture(&surface) {
        Ok(None) => {}
        other => panic!("expected Ok(None) on writer contention, got {other:?}"),
    }
}

/// Acquire methods on an unregistered surface return
/// `AdapterError::SurfaceNotFound`. Symmetric to the buffer path's
/// `SurfaceNotFound` behavior; locks that the image-acquire methods
/// surface the same error variant rather than falling into a generic
/// rejection.
#[test]
fn image_acquire_on_unregistered_surface_returns_surface_not_found() {
    let Some(_host_device) = host_device_or_skip(
        "image_acquire_on_unregistered_surface_returns_surface_not_found",
    ) else {
        return;
    };
    let adapter = CudaSurfaceAdapter::<HostVulkanDevice>::new(_host_device);
    let surface = make_surface(0xdead_b001);
    match adapter.acquire_texture(&surface) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_b001);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
    match adapter.acquire_surface(&surface) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_b001);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}
