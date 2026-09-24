// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestCopyDeviceExportStagingBackToSurface, EscalateRequestOpenCpuReadbackStaging,
    EscalateRequestOpenDeviceExportStaging, EscalateRequestRefillDeviceExportStaging,
    EscalateRequestRunCpuReadbackCopy,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseErr;
use crate::core::context::GpuContextLimitedAccess;

/// The refusal every export-staging op answers on macOS, where a helper reads
/// and writes a texture's IOSurface in place and no staging exists to open.
fn export_staging_not_needed_on_macos(request_id: String, op_name: &str) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id,
        message: format!(
            "{op_name} is not needed on macOS: a helper reads and writes a texture's IOSurface in \
             place, so there is no export staging to open, refill or copy back"
        ),
    })
}

pub(in super::super) fn handle_run_cpu_readback_copy(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRunCpuReadbackCopy,
) -> EscalateResponse {
    export_staging_not_needed_on_macos(request_id, "run_cpu_readback_copy")
}

pub(in super::super) fn handle_open_cpu_readback_staging(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestOpenCpuReadbackStaging,
) -> EscalateResponse {
    export_staging_not_needed_on_macos(request_id, "open_cpu_readback_staging")
}

pub(in super::super) fn handle_open_device_export_staging(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestOpenDeviceExportStaging,
) -> EscalateResponse {
    export_staging_not_needed_on_macos(request_id, "open_device_export_staging")
}

pub(in super::super) fn handle_refill_device_export_staging(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRefillDeviceExportStaging,
) -> EscalateResponse {
    export_staging_not_needed_on_macos(request_id, "refill_device_export_staging")
}

pub(in super::super) fn handle_copy_device_export_staging_back_to_surface(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestCopyDeviceExportStagingBackToSurface,
) -> EscalateResponse {
    export_staging_not_needed_on_macos(request_id, "copy_device_export_staging_back_to_surface")
}
