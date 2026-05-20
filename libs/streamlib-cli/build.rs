// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

fn main() {
    // Propagate the host target triple into the binary's environment so
    // `streamlib pack` can write the cdylib under `lib/<triple>/...` and
    // `Runner::load_project` can resolve by the same triple at load time.
    // `TARGET` is only set inside build.rs; re-emit it as a rustc-env so
    // `env!("STREAMLIB_HOST_TARGET")` works from the crate's own code.
    let target = std::env::var("TARGET").expect("TARGET env var set by cargo for build.rs");
    println!("cargo:rustc-env=STREAMLIB_HOST_TARGET={}", target);
    println!("cargo:rerun-if-env-changed=TARGET");
}
