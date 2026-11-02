// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::super::surface_bound_kernel_binding::publish_bound_surface_layouts_to_surface_share;
use crate::core::color::H273ColorVui;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestColorInfoAxisNames, EscalateRequestConvertSurfaceInto,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;

/// Convert one surface into another on a helper's behalf through the helper's
/// own conversion session, answering once the conversion has retired, and
/// publish the layout the destination settled in.
pub(in super::super) fn handle_convert_surface_into(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    request_id: String,
    request: EscalateRequestConvertSurfaceInto,
) -> EscalateResponse {
    let EscalateRequestConvertSurfaceInto {
        request_id: _,
        source_surface_id,
        destination_surface_id,
        source_color,
    } = request;
    let EscalateRequestColorInfoAxisNames {
        primaries,
        transfer,
        matrix,
        range,
    } = source_color;
    let converted = H273ColorVui::from_color_info_axis_names(
        primaries.as_deref(),
        transfer.as_deref(),
        matrix.as_deref(),
        range.as_deref(),
    )
    .and_then(|source_color| {
        registry
            .surface_color_conversion_session()
            .convert_surface_into(
                sandbox.host_inner(),
                &source_surface_id,
                &destination_surface_id,
                &source_color,
            )
    });
    match converted {
        Ok(settled_destination) => {
            publish_bound_surface_layouts_to_surface_share(
                sandbox.surface_store(),
                std::slice::from_ref(&settled_destination),
            );
            EscalateResponse::Ok(EscalateResponseOk {
                request_id,
                handle_id: destination_surface_id,
                ..Default::default()
            })
        }
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!("convert_surface_into failed: {failure}"),
        }),
    }
}
