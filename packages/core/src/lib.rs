// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test-only host for `@tatolab/core` schema codegen. The wire
//! vocabulary itself ships via `streamlib.yaml` + `schemas/`; this
//! Rust crate exists solely to make codegen output available to
//! integration tests under `tests/`. `streamlib pack` ignores it.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}
