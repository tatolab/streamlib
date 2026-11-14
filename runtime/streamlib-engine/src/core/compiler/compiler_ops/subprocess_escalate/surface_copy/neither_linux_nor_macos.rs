// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::super::escalate_op_unavailable_on_this_platform;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestCopySurfaceToSurface;
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_copy_surface_to_surface(
    _sandbox: &GpuContextLimitedAccess,
    request_id: String,
    _request: EscalateRequestCopySurfaceToSurface,
) -> EscalateResponse {
    escalate_op_unavailable_on_this_platform(request_id, "copy_surface_to_surface")
}
