// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives
//! The host finds `libcef.so` beside itself, in the CEF runtime it is staged into.

fn main() {
    println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
}
