// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire types for the escalate IPC protocol — the seam every escalate
//! encode and decode goes through.

pub(crate) use crate::_generated_::tatolab__escalate::{escalate_request, escalate_response};
pub(crate) use crate::_generated_::{EscalateRequest, EscalateResponse};

#[cfg(test)]
mod escalate_wire_encoding_tests;
