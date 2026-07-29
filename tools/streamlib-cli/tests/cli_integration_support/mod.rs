// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared fixtures for the `streamlib` CLI integration binaries.
//!
//! Each file under `tests/` is its own crate, so without a shared module every
//! one re-declares the binary path, the spawn helper, and a `foo` package
//! fixture — and a manifest-schema change has to be chased through each copy
//! independently.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// The `streamlib` binary the integration tests drive.
pub const STREAMLIB_BINARY_PATH: &str = env!("CARGO_BIN_EXE_streamlib");

/// Run the `streamlib` binary with `args` and capture its output.
pub fn run_streamlib_binary(args: &[&str]) -> std::process::Output {
    Command::new(STREAMLIB_BINARY_PATH)
        .args(args)
        .output()
        .expect("spawn streamlib binary")
}

/// Write a `@tatolab/foo` package source tree at `dir` declaring one owned
/// schema and no processors — the smallest publishable package, and the one
/// that assembles without a toolchain build.
pub fn write_schema_only_foo_package_source(dir: &Path, description: &str) {
    write_foo_package_source(dir, description, None);
}

/// Write a `@tatolab/foo` package source tree at `dir` declaring one owned
/// schema and one manual Python processor.
///
/// The processor entry carries no `version:` key: versioning is a
/// package-level property, and the manifest schema rejects a per-processor one.
pub fn write_single_processor_foo_package_source(dir: &Path, description: &str) {
    write_foo_package_source(
        dir,
        description,
        Some(
            "processors:\n  - name: Foo\n    description: does foo\n    \
             runtime: python\n    execution: manual\n    entrypoint: \"foo:Foo\"\n    \
             inputs: []\n    outputs: []\n",
        ),
    );
}

fn write_foo_package_source(dir: &Path, description: &str, processors_block: Option<&str>) {
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    std::fs::write(
        dir.join("streamlib.yaml"),
        format!(
            // Quoted: this is the one fixture entry point for four binaries,
            // and an unquoted description carrying `:` or `#` would parse as
            // something other than what the caller passed.
            "package:\n  org: tatolab\n  name: foo\n  version: 1.1.0\n  \
             description: \"{description}\"\nschemas:\n  FooFrame:\n    \
             file: schemas/foo_frame.yaml\n{}",
            processors_block.unwrap_or_default()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("schemas/foo_frame.yaml"),
        "metadata:\n  type: FooFrame\n  description: \"A demo frame\"\nproperties:\n  \
         width:\n    type: uint32\n  height:\n    type: uint32\n",
    )
    .unwrap();
}
