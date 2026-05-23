// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();

    #[cfg(target_os = "linux")]
    compile_cpu_ref_doubler();
}

#[cfg(target_os = "linux")]
fn compile_cpu_ref_doubler() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let src = "shaders/cpu_ref_doubler.comp";
    println!("cargo:rerun-if-changed={}", src);
    let dst: PathBuf = Path::new(&out_dir).join("cpu_ref_doubler.spv");
    let status = Command::new("glslc")
        .arg("-fshader-stage=compute")
        .arg("-O")
        .arg(Path::new(src))
        .arg("-o")
        .arg(&dst)
        .status()
        .expect("Failed to run glslc for cpu_ref_doubler.comp (install Vulkan SDK or ensure glslc is in PATH)");
    assert!(
        status.success(),
        "glslc failed to compile cpu_ref_doubler.comp"
    );
}
