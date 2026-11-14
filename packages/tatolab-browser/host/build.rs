// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives
//! On Linux the host finds `libcef.so` beside itself, in the CEF runtime it is
//! staged into; on macOS it loads CEF's framework from its app bundle.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
