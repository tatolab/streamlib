// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/escalate` — polyglot escalate IPC wire types.
//!
//! Peer protocol package alongside `@tatolab/core`: wire vocabulary for the
//! control plane (request/response envelope for subprocess→host RPC).

pub mod _generated_;

pub use _generated_::tatolab__escalate::escalate_request;
pub use _generated_::tatolab__escalate::escalate_response;
pub use _generated_::{EscalateRequest, EscalateResponse};
