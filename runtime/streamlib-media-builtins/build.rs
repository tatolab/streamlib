// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Build script: wherever the Vulkan RHI is compiled, compiles this crate's
//! compute shaders (`src/shaders/*.comp`) to SPIR-V via `glslc` and stages
//! them in `OUT_DIR` for `include_bytes!`.

/// Keeps every `OpName` through `-O`: reflection binds by the shader's own
/// spelling of each binding and refuses a module whose names are gone.
const KEEP_BINDING_NAMES: &str = "-g";

const COMPUTE_SHADERS: &[(&str, &str)] = &[(
    "src/shaders/test_pattern_frame.comp",
    "test_pattern_frame.spv",
)];

fn main() {
    // The target, not the host compiling this script.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    if target_os == "linux" || target_os == "macos" {
        compile_compute_shaders();
    }
}

fn compile_compute_shaders() {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    for (source, spirv_file_name) in COMPUTE_SHADERS {
        println!("cargo:rerun-if-changed={source}");
        let status = std::process::Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg(KEEP_BINDING_NAMES)
            .arg("-O")
            .arg(source)
            .arg("-o")
            .arg(out_dir.join(spirv_file_name))
            .status()
            .expect("failed to run glslc; install the Vulkan SDK or put glslc on PATH");
        assert!(status.success(), "glslc failed to compile {source}");
    }
}
