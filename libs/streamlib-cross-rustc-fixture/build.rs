// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Builds the JTD config dataclass shim and (on Linux) compiles a
//! single trivial compute shader to SPIR-V via the system `glslc`.
//! Matches the streamlib-engine crate's own build.rs shader pipeline
//! so the fixture compiles in the same toolchain shape consumers see.

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();

    let target = std::env::var("TARGET").expect("TARGET env var set by cargo");
    println!("cargo:rustc-env=STREAMLIB_HOST_TARGET={}", target);
    println!("cargo:rerun-if-env-changed=TARGET");

    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"));

    let shader_src = manifest_dir.join("src/shaders/trivial_compute.comp");
    let shader_out = out_dir.join("trivial_compute.spv");

    println!("cargo:rerun-if-changed={}", shader_src.display());

    let status = Command::new("glslc")
        .args([
            "-O",
            "-fshader-stage=compute",
            shader_src.to_str().expect("shader path utf-8"),
            "-o",
            shader_out.to_str().expect("output path utf-8"),
        ])
        .status()
        .expect("invoking glslc — install shaderc-tools if missing");

    assert!(
        status.success(),
        "glslc failed to compile trivial_compute.comp"
    );
}
