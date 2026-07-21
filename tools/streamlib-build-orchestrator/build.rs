// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Build script: bakes the Deno SDK's OWN published version into
//! `STREAMLIB_DENO_SDK_VERSION` by reading `version` from
//! `sdk/streamlib-deno/deno.json`. The off-link Deno extractor spec
//! (`npm:@tatolab/streamlib-deno@<ver>/extract_processors.ts`) must pin the
//! actually-published SDK — the Deno SDK is on an independent version line from
//! the Rust workspace `CARGO_PKG_VERSION`. `deno.json` is the single source of
//! truth; the same read runs in the engine's build script.

fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo for build.rs");
    let deno_json = std::path::Path::new(&manifest_dir).join("../../sdk/streamlib-deno/deno.json");
    println!("cargo:rerun-if-changed={}", deno_json.display());
    let body = std::fs::read_to_string(&deno_json)
        .unwrap_or_else(|e| panic!("reading {}: {e}", deno_json.display()));
    let manifest: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("parsing {}: {e}", deno_json.display()));
    let version = manifest["version"].as_str().unwrap_or_else(|| {
        panic!(
            "{} has no string `version` field to pin the Deno SDK",
            deno_json.display()
        )
    });
    println!("cargo:rustc-env=STREAMLIB_DENO_SDK_VERSION={version}");
}
