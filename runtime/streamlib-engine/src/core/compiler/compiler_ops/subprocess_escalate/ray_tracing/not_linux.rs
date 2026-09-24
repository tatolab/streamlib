// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::escalate_op_only_available_on_linux;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestRegisterAccelerationStructureBlas,
    EscalateRequestRegisterAccelerationStructureTlas, EscalateRequestRegisterRayTracingKernel,
    EscalateRequestRunRayTracingKernel,
};
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_register_acceleration_structure_blas(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRegisterAccelerationStructureBlas,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "register_acceleration_structure_blas")
}

pub(in super::super) fn handle_register_acceleration_structure_tlas(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRegisterAccelerationStructureTlas,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "register_acceleration_structure_tlas")
}

pub(in super::super) fn handle_register_ray_tracing_kernel(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRegisterRayTracingKernel,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "register_ray_tracing_kernel")
}

pub(in super::super) fn handle_run_ray_tracing_kernel(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRunRayTracingKernel,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "run_ray_tracing_kernel")
}
