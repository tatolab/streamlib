// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire types for the escalate IPC protocol — the seam every escalate
//! encode and decode goes through.

pub(crate) mod escalate_request;
pub(crate) mod escalate_response;

pub(crate) use escalate_request::EscalateRequest;
pub(crate) use escalate_response::EscalateResponse;

#[cfg(test)]
mod escalate_wire_encoding_tests;
