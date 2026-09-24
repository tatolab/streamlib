// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::escalate_op_only_available_on_linux;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestRegisterGraphicsKernel, EscalateRequestRunGraphicsDraw,
};
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_register_graphics_kernel(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRegisterGraphicsKernel,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "register_graphics_kernel")
}

pub(in super::super) fn handle_run_graphics_draw(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestRunGraphicsDraw,
) -> EscalateResponse {
    escalate_op_only_available_on_linux(request_id, "run_graphics_draw")
}
