// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives
//! The extension finds `libcef.so` in the CEF runtime the wheel ships beside it.

fn main() {
    println!("cargo::rustc-link-arg-cdylib=-Wl,-rpath,$ORIGIN/cef");
}
