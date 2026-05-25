// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

fn main() {
    let target = std::env::var("TARGET").expect("TARGET env var set by cargo for build.rs");
    println!("cargo:rustc-env=STREAMLIB_CARGO_BUILD_HOST_TARGET={}", target);
    println!("cargo:rerun-if-env-changed=TARGET");
}
