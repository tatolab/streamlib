// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseErr;
use crate::core::context::GpuContextLimitedAccess;

pub(in super::super) fn handle_acquire_image(
    _sandbox: &GpuContextLimitedAccess,
    _registry: &EscalateHandleRegistry,
    rid: String,
    _request: EscalateRequestAcquireImage,
) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id: rid,
        message: "acquire_image is only available on Linux (DMA-BUF render-target path)"
            .to_string(),
    })
}
