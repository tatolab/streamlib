// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::escalate_op_only_available_on_linux;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestCopyDeviceExportStagingBackToSurface, EscalateRequestOpenCpuReadbackStaging,
    EscalateRequestOpenDeviceExportStaging, EscalateRequestRefillDeviceExportStaging,
    EscalateRequestRunCpuReadbackCopy,
};
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_run_cpu_readback_copy(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRunCpuReadbackCopy,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "run_cpu_readback_copy")
}

pub(in super::super) fn handle_open_cpu_readback_staging(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestOpenCpuReadbackStaging,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "open_cpu_readback_staging")
}

pub(in super::super) fn handle_open_device_export_staging(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestOpenDeviceExportStaging,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "open_device_export_staging")
}

pub(in super::super) fn handle_refill_device_export_staging(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRefillDeviceExportStaging,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "refill_device_export_staging")
}

pub(in super::super) fn handle_copy_device_export_staging_back_to_surface(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestCopyDeviceExportStagingBackToSurface,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "copy_device_export_staging_back_to_surface")
}
