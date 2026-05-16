// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: runs streamlib's JTD codegen (for `_generated_shim.rs`,
//! even though this package owns no schemas) and on Linux compiles the
//! tone-curve compute shader to SPIR-V via `glslc`.

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();

    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let shaders: &[(&str, &str)] = &[(
        "src/shaders/tone_curve.comp",
        "tone_curve.spv",
    )];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let shader_include_dir = "src/shaders";

    println!(
        "cargo:rerun-if-changed={}/color_common.glsl",
        shader_include_dir
    );

    for (src, dst) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg("-O")
            .arg("-I")
            .arg(shader_include_dir)
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");

        assert!(status.success(), "glslc failed to compile {}", src);
    }
}
