// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::surface_bound_kernel_binding::publish_bound_surface_layouts_to_surface_share;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestCopySurfaceToSurface;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;

/// Copy one surface into another on a helper's behalf, answering once the
/// copy has retired, and publish the layout the destination settled in so a
/// cross-process reader's checkout names it.
pub(in super::super) fn handle_copy_surface_to_surface(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    request: EscalateRequestCopySurfaceToSurface,
) -> EscalateResponse {
    let EscalateRequestCopySurfaceToSurface {
        request_id: _,
        source_surface_id,
        destination_surface_id,
    } = request;
    match sandbox.copy_surface_to_surface(&source_surface_id, &destination_surface_id) {
        Ok(settled_destination_layouts) => {
            publish_bound_surface_layouts_to_surface_share(
                sandbox.surface_store(),
                &settled_destination_layouts,
            );
            EscalateResponse::Ok(EscalateResponseOk {
                request_id,
                handle_id: destination_surface_id,
                ..Default::default()
            })
        }
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!("copy_surface_to_surface failed: {failure}"),
        }),
    }
}
