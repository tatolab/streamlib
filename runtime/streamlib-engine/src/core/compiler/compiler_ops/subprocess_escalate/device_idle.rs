// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A helper process's wait for the parent's device to go idle.

use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestWaitDeviceIdle;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;

/// Wait until the parent's device is idle, answering once it is.
pub(super) fn handle_wait_device_idle(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    request: EscalateRequestWaitDeviceIdle,
) -> EscalateResponse {
    let EscalateRequestWaitDeviceIdle { request_id: _ } = request;
    match sandbox.escalate(|full| full.wait_device_idle()) {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: String::new(),
            ..Default::default()
        }),
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("wait_device_idle failed: {failure}"),
        }),
    }
}
