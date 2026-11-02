// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Compiles the codec's compute shaders to SPIR-V with `glslc` into `OUT_DIR`
//! for `include_bytes!`, on Linux, the one target Vulkan Video builds for.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    if target_os != "linux" {
        return;
    }
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    for (shader_source, spirv_file_name) in [
        ("shaders/rgb_to_nv12.comp", "rgb_to_nv12.spv"),
        ("shaders/nv12_to_rgb.comp", "nv12_to_rgb.spv"),
    ] {
        println!("cargo:rerun-if-changed={shader_source}");
        let status = std::process::Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg("-g")
            .arg("-O")
            .arg(shader_source)
            .arg("-o")
            .arg(out_dir.join(spirv_file_name))
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");
        assert!(status.success(), "glslc failed to compile {shader_source}");
    }
}
