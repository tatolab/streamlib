// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The readback ops answer from `GpuContext` with nothing installed:
//! no bridge, no application glue, no runtime-absent case.
//!
//! These run without a real surface, so they assert the shape of the
//! refusal rather than a landed copy — an unresolvable surface is an
//! error naming the surface, never the missing-bridge refusal the
//! deleted seam used to answer to every caller. The copy itself is
//! proven over a real device in
//! `surface_export_staging`'s own GPU-gated tests.

use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::super::{handle_escalate_op, request_id};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestOpenCpuReadbackStaging, EscalateRequestRunCpuReadbackCopy,
    EscalateRequestRunCpuReadbackCopyDirection,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::context::{GpuContext, GpuContextLimitedAccess};
use crate::core::rhi::PixelFormat;
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

fn sandbox_or_skip(test_name: &str) -> Option<GpuContextLimitedAccess> {
    gpu_or_skip(test_name).map(GpuContextLimitedAccess::new)
}

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::init_for_platform_sync() {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            println!("{test_name}: no GPU device — skipping");
            None
        }
    }
}

/// Every readback op names the surface it could not resolve, and
/// none of them mentions an installation step.
#[test]
fn an_unresolvable_surface_is_refused_by_name_and_never_by_a_missing_bridge() {
    let Some(sandbox) =
        sandbox_or_skip("an_unresolvable_surface_is_refused_by_name_and_never_by_a_missing_bridge")
    else {
        return;
    };
    let registry = EscalateHandleRegistry::new();

    let requests = [
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-run".into(),
            surface_id: "no-such-surface".into(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
        }),
        EscalateRequest::OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging {
            request_id: "req-open".into(),
            surface_id: "no-such-surface".into(),
        }),
    ];

    for request in requests {
        let expected_request_id = request_id(&request)
            .expect("every readback op carries a correlation token")
            .to_string();
        let response = handle_escalate_op(
            &sandbox,
            &registry,
            &a_mesh_link_ingress_table_carrying_nothing(),
            request,
        )
        .expect("every readback op produces a response");
        match response {
            EscalateResponse::Err(err) => {
                assert_eq!(err.request_id, expected_request_id);
                assert!(
                    err.message.contains("no-such-surface"),
                    "{expected_request_id}: the refusal must name the surface, got: {}",
                    err.message
                );
                assert!(
                    !err.message.contains("Bridge"),
                    "{expected_request_id}: the capability is always present, so no \
                             refusal may cite a bridge; got: {}",
                    err.message
                );
            }
            other => panic!("{expected_request_id}: expected Err, got {other:?}"),
        }
    }
}

/// The seam carries a real frame's pixels into CPU-readable
/// memory: seed a pool frame, drive `run_cpu_readback_copy`
/// through `handle_escalate_op`, read the staging's mapping.
///
/// This is what pins the op's mappings. Swap the residency to
/// `DeviceLocal` and the mapping is null; swap the direction to
/// `StagingBackIntoSurface` and the staging never receives the
/// frame. Both fail here.
/// GPU-gated: skips when no device is present.
#[test]
fn the_seam_lands_a_frames_pixels_in_cpu_readable_memory() {
    const SEEDED: u8 = 0x3c;
    let Some(gpu) = gpu_or_skip("the_seam_lands_a_frames_pixels_in_cpu_readable_memory") else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
        .expect("acquire a frame to read back");
    let surface_id = pool_id.to_string();
    let plane = pooled_backing.plane_base_address(0);
    assert!(!plane.is_null(), "the pooled backing must be host-mapped");
    unsafe { std::ptr::write_bytes(plane, SEEDED, pooled_backing.plane_size(0) as usize) };

    let sandbox = GpuContextLimitedAccess::new(gpu.clone());
    let registry = EscalateHandleRegistry::new();
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-seam-read".into(),
            surface_id: surface_id.clone(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
        }),
    )
    .expect("run_cpu_readback_copy always produces a response");
    let EscalateResponse::Ok(ok) = response else {
        panic!("expected Ok, got {response:?}");
    };
    assert_eq!(ok.request_id, "req-seam-read");
    assert!(
        ok.timeline_value.is_some(),
        "the child waits on the timeline value this op answers with"
    );

    let staging = gpu
        .surface_export_staging(
            &surface_id,
            crate::core::context::SurfaceExportStagingResidency::HostVisible,
        )
        .expect("the op minted the staging the child would check out");
    let mapped = staging.staging_buffer().mapped_ptr();
    assert!(
        !mapped.is_null(),
        "the op must stage into memory a CPU consumer can map"
    );
    let staged =
        unsafe { std::slice::from_raw_parts(mapped, staging.staging_byte_size() as usize) };
    assert!(
        staged.iter().all(|byte| *byte == SEEDED),
        "the staging must carry the seeded frame; first mismatch at {:?}",
        staged.iter().position(|byte| *byte != SEEDED)
    );
}

/// `buffer_to_image` through the seam refuses to publish a
/// staging nothing has read a frame into. Pins the direction
/// mapping: `image_to_buffer` would have succeeded here.
/// GPU-gated: skips when no device is present.
#[test]
fn the_seam_refuses_to_publish_a_staging_no_frame_was_read_into() {
    let Some(gpu) = gpu_or_skip("the_seam_refuses_to_publish_a_staging_no_frame_was_read_into")
    else {
        return;
    };
    let (pool_id, _pooled_backing) = gpu
        .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
        .expect("acquire a frame");
    let sandbox = GpuContextLimitedAccess::new(gpu.clone());
    let registry = EscalateHandleRegistry::new();

    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-seam-write".into(),
            surface_id: pool_id.to_string(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::BufferToImage,
        }),
    )
    .expect("run_cpu_readback_copy always produces a response");
    match response {
        EscalateResponse::Err(err) => assert!(
            err.message.contains("never been read into"),
            "publishing an unread staging must be refused by name, got: {}",
            err.message
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}

/// The publish direction, driven to success through the seam:
/// read a frame in, edit the mapping, publish it back.
///
/// The only end-to-end coverage the publish path has. Swap the
/// second call's `BufferToImage` to `ImageToBuffer` and the edit
/// is silently discarded and overwritten by the frame — which
/// this catches, and nothing else does.
/// GPU-gated: skips when no device is present.
#[test]
fn the_seam_publishes_a_staged_edit_back_into_the_pooled_backing() {
    const EDIT: u8 = 0x6b;
    let Some(gpu) = gpu_or_skip("the_seam_publishes_a_staged_edit_back_into_the_pooled_backing")
    else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
        .expect("acquire a pool-only frame");
    let surface_id = pool_id.to_string();
    let sandbox = GpuContextLimitedAccess::new(gpu.clone());
    let registry = EscalateHandleRegistry::new();

    // Read the frame in — the write-back's precondition.
    let read = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-read".into(),
            surface_id: surface_id.clone(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
        }),
    )
    .expect("run_cpu_readback_copy always produces a response");
    assert!(
        matches!(read, EscalateResponse::Ok(_)),
        "the read-in must succeed, got {read:?}"
    );

    let staging = gpu
        .surface_export_staging(
            &surface_id,
            crate::core::context::SurfaceExportStagingResidency::HostVisible,
        )
        .expect("the staging the read minted");
    let mapped = staging.staging_buffer().mapped_ptr();
    assert!(!mapped.is_null(), "a host-visible staging must be mapped");
    unsafe { std::ptr::write_bytes(mapped, EDIT, staging.staging_byte_size() as usize) };

    let published = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-publish".into(),
            surface_id: surface_id.clone(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::BufferToImage,
        }),
    )
    .expect("run_cpu_readback_copy always produces a response");
    let EscalateResponse::Ok(ok) = published else {
        panic!("expected Ok, got {published:?}");
    };
    assert!(
        ok.timeline_value.is_some(),
        "a publish answers with the timeline value the child waits on"
    );

    let plane = pooled_backing.plane_base_address(0);
    let backing =
        unsafe { std::slice::from_raw_parts(plane, pooled_backing.plane_size(0) as usize) };
    assert!(
        backing.iter().all(|byte| *byte == EDIT),
        "the edit must reach the pooled backing; first mismatch at {:?}",
        backing.iter().position(|byte| *byte != EDIT)
    );
}

/// The open op names itself, not its device-export twin — the two
/// share one handler and differ only by residency, so a swapped
/// mapping would surface here as the wrong op in the message.
/// GPU-gated: skips when no device is present.
#[test]
fn the_open_op_names_itself_and_not_its_device_export_twin() {
    let Some(gpu) = gpu_or_skip("the_open_op_names_itself_and_not_its_device_export_twin") else {
        return;
    };
    let (pool_id, _pooled_backing) = gpu
        .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
        .expect("acquire a frame");
    let sandbox = GpuContextLimitedAccess::new(gpu.clone());
    let registry = EscalateHandleRegistry::new();

    // No surface-share service in a bare context, so the publish
    // step refuses — which is the step whose op name is under test.
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging {
            request_id: "req-seam-open".into(),
            surface_id: pool_id.to_string(),
        }),
    )
    .expect("open_cpu_readback_staging always produces a response");
    match response {
        EscalateResponse::Err(err) => {
            assert!(
                err.message.starts_with("open_cpu_readback_staging"),
                "the refusal must name this op, got: {}",
                err.message
            );
        }
        other => panic!("expected Err without a surface-share service, got {other:?}"),
    }
}

/// A surface id that is not a decimal `u64` is no longer special.
///
/// The deleted bridge keyed its own registry by `u64` and parsed
/// the id before doing anything else, which would refuse every
/// modern id: a published frame is `<slot>#<generation>`. The
/// engine resolves the id it was given.
#[test]
fn a_non_numeric_surface_id_is_not_a_parse_error() {
    let Some(sandbox) = sandbox_or_skip("a_non_numeric_surface_id_is_not_a_parse_error") else {
        return;
    };
    let registry = EscalateHandleRegistry::new();

    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-frame-id".into(),
            surface_id: "pool-slot-7#3".into(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
        }),
    )
    .expect("run_cpu_readback_copy always produces a response");
    match response {
        EscalateResponse::Err(err) => assert!(
            !err.message.contains("u64"),
            "a per-frame surface id must not be refused as a malformed integer, got: {}",
            err.message
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}
