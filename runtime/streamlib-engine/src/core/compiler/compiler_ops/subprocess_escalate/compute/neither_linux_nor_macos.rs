// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::escalate_op_unavailable_on_this_platform;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestRegisterComputeKernel, EscalateRequestRunComputeKernel,
    EscalateRequestRunComputeKernelBatch,
};
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_register_compute_kernel(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRegisterComputeKernel,
) -> EscalateResponse {
    escalate_op_unavailable_on_this_platform(request_id, "register_compute_kernel")
}

pub(in super::super) fn handle_run_compute_kernel(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRunComputeKernel,
) -> EscalateResponse {
    escalate_op_unavailable_on_this_platform(request_id, "run_compute_kernel")
}

pub(in super::super) fn handle_run_compute_kernel_batch(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRunComputeKernelBatch,
) -> EscalateResponse {
    escalate_op_unavailable_on_this_platform(request_id, "run_compute_kernel_batch")
}
