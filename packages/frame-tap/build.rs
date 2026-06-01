// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Codegen for the frame-tap package: generates the typed config + the
//! imported `@tatolab/core` wire types (VideoFrame) consumed by the processor.

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();
}
