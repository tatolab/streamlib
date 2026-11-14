// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use super::super::escalate_op_unavailable_on_this_platform;
use super::super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestConvertSurfaceInto;
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_convert_surface_into(
    _sandbox: &GpuContextLimitedAccess,
    _registry: &EscalateHandleRegistry,
    request_id: String,
    _request: EscalateRequestConvertSurfaceInto,
) -> EscalateResponse {
    escalate_op_unavailable_on_this_platform(request_id, "convert_surface_into")
}
