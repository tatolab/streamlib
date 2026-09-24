// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestCopyDeviceExportStagingBackToSurface, EscalateRequestOpenCpuReadbackStaging,
    EscalateRequestOpenDeviceExportStaging, EscalateRequestRefillDeviceExportStaging,
    EscalateRequestRunCpuReadbackCopy, EscalateRequestRunCpuReadbackCopyDirection,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::{GpuContextLimitedAccess, SurfaceExportStagingResidency};

/// The three wire ops that run one surface-export staging copy.
///
/// Named as ops rather than as a (residency x direction) product because
/// the wire does not spell the two axes alike: device-export gives each
/// direction its own op name, cpu-readback carries direction as a field.
/// Enumerating the ops gives each one its name for free.
#[derive(Clone, Copy)]
pub(super) enum SurfaceExportStagingCopyOp {
    RefillDeviceExportStaging,
    CopyDeviceExportStagingBackToSurface,
    RunCpuReadbackCopy(SurfaceExportStagingCopyDirection),
}

/// Which way one surface-export staging copy runs.
#[derive(Clone, Copy)]
pub(super) enum SurfaceExportStagingCopyDirection {
    /// A refill: the surface's current pixels into the staging, so the
    /// consumer's next read sees this frame.
    SurfaceIntoStaging,
    /// A publish: the consumer's edit back into the surface's own
    /// allocation, so every other holder observes it.
    StagingBackIntoSurface,
}

impl SurfaceExportStagingCopyOp {
    /// The wire op name, for error messages that have to say which
    /// request failed.
    fn escalate_op_name(self) -> &'static str {
        match self {
            Self::RefillDeviceExportStaging => "refill_device_export_staging",
            Self::CopyDeviceExportStagingBackToSurface => {
                "copy_device_export_staging_back_to_surface"
            }
            Self::RunCpuReadbackCopy(_) => "run_cpu_readback_copy",
        }
    }

    fn residency(self) -> SurfaceExportStagingResidency {
        match self {
            Self::RefillDeviceExportStaging | Self::CopyDeviceExportStagingBackToSurface => {
                SurfaceExportStagingResidency::DeviceLocal
            }
            Self::RunCpuReadbackCopy(_) => SurfaceExportStagingResidency::HostVisible,
        }
    }

    fn direction(self) -> SurfaceExportStagingCopyDirection {
        match self {
            Self::RefillDeviceExportStaging => {
                SurfaceExportStagingCopyDirection::SurfaceIntoStaging
            }
            Self::CopyDeviceExportStagingBackToSurface => {
                SurfaceExportStagingCopyDirection::StagingBackIntoSurface
            }
            Self::RunCpuReadbackCopy(direction) => direction,
        }
    }
}

/// The wire op that opens a staging at `residency`.
pub(super) fn escalate_open_op_name(residency: SurfaceExportStagingResidency) -> &'static str {
    match residency {
        SurfaceExportStagingResidency::DeviceLocal => "open_device_export_staging",
        SurfaceExportStagingResidency::HostVisible => "open_cpu_readback_staging",
    }
}

/// Open the device-local export staging for `surface_id` on behalf of a
/// helper process — the residency an external device API (CUDA) imports.
pub(in super::super) fn handle_open_device_export_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    request: EscalateRequestOpenDeviceExportStaging,
) -> EscalateResponse {
    handle_open_surface_export_staging(
        sandbox,
        request_id,
        &request.surface_id,
        SurfaceExportStagingResidency::DeviceLocal,
    )
}

/// Open one residency's export staging for `surface_id` on behalf of a
/// helper process: allocate it if the surface has none at that
/// residency, publish it and its refill timeline to the surface-share
/// service, and answer with everything the child needs to reach the
/// memory — the id to check out, the geometry the staging was sized for,
/// whether a write-back is possible, and the UUID of the GPU that owns
/// it.
///
/// No fd travels on this socket. The staging's OPAQUE_FD and the
/// timeline's fd reach the child through the surface-share check-out it
/// makes with the returned id.
pub(super) fn handle_open_surface_export_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    surface_id: &str,
    residency: SurfaceExportStagingResidency,
) -> EscalateResponse {
    let opened = (|| -> crate::core::error::Result<EscalateResponseOk> {
        let staging = sandbox.surface_export_staging(surface_id, residency)?;
        let (shared_id, pixel_format) = sandbox.share_surface_export_staging(&staging)?;
        Ok(EscalateResponseOk {
            request_id: request_id.clone(),
            handle_id: shared_id,
            width: Some(staging.surface_width()),
            height: Some(staging.surface_height()),
            format: Some(pixel_format.wire_name().to_string()),
            staging_byte_size: Some(staging.staging_byte_size().to_string()),
            bytes_per_row: Some(staging.bytes_per_row().to_string()),
            writable: Some(staging.writable()),
            exporting_device_uuid: Some(
                staging
                    .exporting_device_uuid()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            ),
            ..Default::default()
        })
    })();
    match opened {
        Ok(response) => EscalateResponse::Ok(response),
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!("{} failed: {failure}", escalate_open_op_name(residency)),
        }),
    }
}

/// Run one surface-export staging copy on behalf of a helper process and
/// answer with the timeline value it signalled.
///
/// The child waits for that value on its imported copy of the staging's
/// `refill_done` timeline before touching the memory — the host's own
/// bounded wait orders the submit for callers in this process, but it
/// says nothing to a consumer one process away.
///
/// Always available at either residency: the staging is a `GpuContext`
/// capability, minted on first ask, with no installation step and
/// nothing supplied by the application.
///
/// Every copy blocks: a busy recorder is waited for, never reported.
/// Every refusal — a retired frame id, a read-only export, a geometry
/// change — is an error for every op, so `ok` and `err` are the only two
/// answers a child ever has to have an arm for.
pub(super) fn handle_surface_export_staging_copy(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    surface_id: &str,
    op: SurfaceExportStagingCopyOp,
) -> EscalateResponse {
    let copied = sandbox
        .surface_export_staging(surface_id, op.residency())
        .and_then(|staging| match op.direction() {
            SurfaceExportStagingCopyDirection::SurfaceIntoStaging => {
                sandbox.refill_surface_export_staging(&staging, surface_id)
            }
            SurfaceExportStagingCopyDirection::StagingBackIntoSurface => {
                sandbox.copy_surface_export_staging_back_to_surface(&staging, surface_id)
            }
        });
    match copied {
        Ok(signalled) => EscalateResponse::Ok(EscalateResponseOk {
            request_id,
            handle_id: surface_id.to_string(),
            timeline_value: Some(signalled.to_string()),
            ..Default::default()
        }),
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!("{} failed: {failure}", op.escalate_op_name()),
        }),
    }
}

/// Open the CPU-readable staging for `surface_id` on behalf of a helper
/// process — the readback twin of
/// [`handle_open_device_export_staging`], differing only in residency.
///
/// Without this the copies above would land in a buffer no child can
/// reach: the staging is engine-owned, so nothing else publishes it.
pub(in super::super) fn handle_open_cpu_readback_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    request: EscalateRequestOpenCpuReadbackStaging,
) -> EscalateResponse {
    handle_open_surface_export_staging(
        sandbox,
        request_id,
        &request.surface_id,
        SurfaceExportStagingResidency::HostVisible,
    )
}

/// Run a `run_cpu_readback_copy` in the direction the wire names.
pub(in super::super) fn handle_run_cpu_readback_copy(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    request: EscalateRequestRunCpuReadbackCopy,
) -> EscalateResponse {
    let EscalateRequestRunCpuReadbackCopy {
        request_id: _,
        surface_id,
        direction,
    } = request;
    handle_surface_export_staging_copy(
        sandbox,
        request_id,
        &surface_id,
        SurfaceExportStagingCopyOp::RunCpuReadbackCopy(match direction {
            EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer => {
                SurfaceExportStagingCopyDirection::SurfaceIntoStaging
            }
            EscalateRequestRunCpuReadbackCopyDirection::BufferToImage => {
                SurfaceExportStagingCopyDirection::StagingBackIntoSurface
            }
        }),
    )
}

/// Refill `surface_id`'s device-local export staging from the surface.
pub(in super::super) fn handle_refill_device_export_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    request: EscalateRequestRefillDeviceExportStaging,
) -> EscalateResponse {
    handle_surface_export_staging_copy(
        sandbox,
        request_id,
        &request.surface_id,
        SurfaceExportStagingCopyOp::RefillDeviceExportStaging,
    )
}

/// Copy `surface_id`'s device-local export staging back into the surface.
pub(in super::super) fn handle_copy_device_export_staging_back_to_surface(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    request: EscalateRequestCopyDeviceExportStagingBackToSurface,
) -> EscalateResponse {
    handle_surface_export_staging_copy(
        sandbox,
        request_id,
        &request.surface_id,
        SurfaceExportStagingCopyOp::CopyDeviceExportStagingBackToSurface,
    )
}
