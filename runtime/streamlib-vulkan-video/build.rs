// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Compiles the codec's compute shaders to SPIR-V through the Slang compiler
//! the workspace stages into `OUT_DIR` for `include_bytes!`, on Linux, the one
//! target Vulkan Video builds for.

use std::path::{Path, PathBuf};

use streamlib_slang_shader_compiler::{
    ShaderCompilationTargetStage, compile_a_slang_shader_file,
    open_the_workspace_staged_slang_compiler, workspace_staged_slang_compiler_library,
};

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    if target_os != "linux" {
        return;
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_staged_slang_compiler_library(&workspace_root).display()
    );
    let slang_compiler = open_the_workspace_staged_slang_compiler(&workspace_root)
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    for (shader_source, spirv_file_name) in [
        ("shaders/rgb_to_nv12.slang", "rgb_to_nv12.spv"),
        ("shaders/nv12_to_rgb.slang", "nv12_to_rgb.spv"),
    ] {
        println!("cargo:rerun-if-changed={shader_source}");
        compile_a_slang_shader_file(
            &slang_compiler,
            Path::new(shader_source),
            ShaderCompilationTargetStage::Compute,
            &[],
            &[],
            &out_dir.join(spirv_file_name),
        )
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    }
}
