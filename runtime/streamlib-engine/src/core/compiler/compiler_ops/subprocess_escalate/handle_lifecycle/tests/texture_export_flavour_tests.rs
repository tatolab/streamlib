// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The timeline edges and export flavours a helper's textures and images carry,
//! by what this machine's device can export.

use super::*;

/// A helper's texture and render-target image carry their `produce_done` /
/// `consume_done` edges — in the surface-share registration and in the
/// handle the registry holds — exactly where the device exports a timeline
/// semaphore. The software device exports none, refuses an exportable one at
/// creation, and registers both with no edge; a hardware device that exports
/// one keeps both edges. The image arm runs where the device can make a
/// render-target image at all, which the software device cannot.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_helpers_texture_and_image_carry_timeline_edges_exactly_where_the_device_exports_one() {
    use super::super::super::handle_escalate_op;
    use super::super::RegisteredHandle;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
        EscalateRequestAcquireImage, EscalateRequestAcquireTexture,
    };
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::TextureFormat;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        tracing::warn!("no GPU device — skipping");
        return;
    };
    let device_exports_a_timeline = gpu.gpu_capabilities().supports_semaphore_export;
    let device_name = gpu.gpu_capabilities().device_name;
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    let assert_the_registration_carries_edges_exactly_where_one_crosses =
        |handle_id: &str, kind: &str| {
            let registered = live_service
                .state
                .get_surface_planes(handle_id)
                .unwrap_or_else(|| panic!("the {kind} is registered with the service"));
            assert_eq!(
                (
                    registered.produce_done_fd.is_some(),
                    registered.consume_done_fd.is_some()
                ),
                (device_exports_a_timeline, device_exports_a_timeline),
                "{device_name}: the {kind}'s registered edges follow supports_semaphore_export"
            );
        };

    let acquired_texture = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-texture-edges".to_string(),
            width: 64,
            height: 32,
            format: TextureFormat::Rgba8Unorm.wire_name().to_string(),
            node_output_pool: None,
            usage: vec!["texture_binding".to_string(), "storage_binding".to_string()],
        }),
    );
    let texture_handle_id = match acquired_texture {
        Some(EscalateResponse::Ok(ok)) => ok.handle_id,
        other => panic!("{device_name}: acquire_texture failed: {other:?}"),
    };
    assert_the_registration_carries_edges_exactly_where_one_crosses(&texture_handle_id, "texture");
    match registry.remove_handle(&texture_handle_id) {
        Some(RegisteredHandle::Texture {
            produce_done,
            consume_done,
            ..
        }) => assert_eq!(
            (produce_done.is_some(), consume_done.is_some()),
            (device_exports_a_timeline, device_exports_a_timeline),
            "{device_name}: the held texture's edges follow supports_semaphore_export"
        ),
        _ => panic!("the registry holds the acquired texture"),
    }

    let acquired_image = handle_escalate_op(
        &sandbox,
        &registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquireImage(EscalateRequestAcquireImage {
            request_id: "req-image-edges".to_string(),
            width: 64,
            height: 32,
            format: TextureFormat::Rgba8Unorm.wire_name().to_string(),
        }),
    );
    match acquired_image {
        Some(EscalateResponse::Ok(ok)) => {
            assert_the_registration_carries_edges_exactly_where_one_crosses(&ok.handle_id, "image");
            match registry.remove_handle(&ok.handle_id) {
                Some(RegisteredHandle::Image {
                    produce_done,
                    consume_done,
                    ..
                }) => assert_eq!(
                    (produce_done.is_some(), consume_done.is_some()),
                    (device_exports_a_timeline, device_exports_a_timeline),
                    "{device_name}: the held image's edges follow supports_semaphore_export"
                ),
                _ => panic!("the registry holds the acquired image"),
            }
        }
        Some(EscalateResponse::Err(refusal)) => assert!(
            !sandbox
                .host_inner()
                .gpu_capabilities()
                .supports_dma_buf_export,
            "{device_name} exports DMA-BUF, so its render-target image must acquire: {}",
            refusal.message
        ),
        other => panic!("acquire_image answered nothing usable: {other:?}"),
    }
    live_service.stop();
}

/// A render-target texture a helper acquires crosses on the flavour its
/// device can carry: tiled DMA-BUF where the device exports DMA-BUF, and
/// OPAQUE_FD — stating its colour-attachment usage and the memory type an
/// import must bind — where it does not.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_helpers_render_target_texture_registers_the_flavour_its_device_can_carry() {
    use super::super::super::handle_escalate_op;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireTexture;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::TextureFormat;

    const VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT: u32 = 0x10;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        tracing::warn!("no GPU device — skipping");
        return;
    };
    let device_exports_dma_buf = gpu.gpu_capabilities().supports_dma_buf_export;
    let device_name = gpu.gpu_capabilities().device_name;
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    let handle_id = match handle_escalate_op(
        &sandbox,
        &registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-render-target-texture".to_string(),
            width: 64,
            height: 32,
            format: TextureFormat::Rgba8Unorm.wire_name().to_string(),
            node_output_pool: None,
            usage: vec![
                "render_attachment".to_string(),
                "texture_binding".to_string(),
            ],
        }),
    ) {
        Some(EscalateResponse::Ok(ok)) => ok.handle_id,
        other => panic!("{device_name}: a render-target acquire_texture failed: {other:?}"),
    };
    let registered = live_service
        .state
        .get_surface_planes(&handle_id)
        .expect("the render target is registered with the service");

    if device_exports_dma_buf {
        assert_eq!(registered.handle_type, "dma_buf", "{device_name}");
        assert_ne!(
            registered.drm_format_modifier, 0,
            "{device_name}: a render target on a DMA-BUF device takes the tiled flavour"
        );
        assert_eq!(registered.vk_memory_type_index, None, "{device_name}");
    } else {
        assert_eq!(registered.handle_type, "opaque_fd", "{device_name}");
        assert_ne!(
            registered.vk_image_usage & VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
            0,
            "{device_name}: the registration states the colour-attachment usage the import \
             rebuilds the image with, got {:#x}",
            registered.vk_image_usage
        );
        assert!(
            registered.vk_memory_type_index.is_some(),
            "{device_name}: an OPAQUE_FD registration states the memory type an import binds"
        );
    }

    release_a_handle_over_the_escalate_op(&sandbox, &registry, &handle_id)
        .expect("the render target releases");
    live_service.stop();
}

/// On a device without DMA-BUF, a texture request no OPAQUE_FD image can
/// carry — a multi-plane format — is refused at the acquisition, naming the
/// device, the format and the usage, rather than failing later at an export
/// the device does not have.
#[cfg(target_os = "linux")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_texture_no_opaque_fd_image_can_carry_is_refused_at_the_acquisition_where_dma_buf_is_absent() {
    use super::super::super::handle_escalate_op;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
    use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireTexture;
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use crate::core::rhi::TextureFormat;

    let Ok(gpu) = GpuContext::init_for_platform_sync() else {
        tracing::warn!("no GPU device — skipping");
        return;
    };
    if gpu.gpu_capabilities().supports_dma_buf_export {
        tracing::warn!("the device exports DMA-BUF — the refusal is the software device's");
        return;
    }
    let device_name = gpu.gpu_capabilities().device_name;
    let live_service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let sandbox = GpuContextLimitedAccess::new(gpu);
    let registry = EscalateHandleRegistry::new();

    let refusal = handle_id_or_refusal_of(handle_escalate_op(
        &sandbox,
        &registry,
        &WhatAHelperPlacedNodeReaches::default(),
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-multi-plane-texture".to_string(),
            width: 64,
            height: 32,
            format: TextureFormat::Nv12.wire_name().to_string(),
            node_output_pool: None,
            usage: vec!["texture_binding".to_string()],
        }),
    ))
    .expect_err("a multi-plane texture cannot cross a device without DMA-BUF");
    for named in [
        device_name.as_str(),
        TextureFormat::Nv12.wire_name(),
        "texture_binding",
        "OPAQUE_FD",
    ] {
        assert!(
            refusal.contains(named),
            "the refusal names {named:?}: {refusal}"
        );
    }
    live_service.stop();
}
