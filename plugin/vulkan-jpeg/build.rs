// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: compiles the fused JPEG decode compute shader at
//! `src/shaders/jpeg_decode.comp` to SPIR-V via `glslc` and stages
//! the artifact in `OUT_DIR` for `include_bytes!` to consume at compile
//! time. Linux-only — the GPU kernel is gated behind `target_os = "linux"`.

fn main() {
    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let shaders: &[(&str, &str, &str)] =
        &[("src/shaders/jpeg_decode.comp", "jpeg_decode.spv", "compute")];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");

    // The JPEG kernel `#include`s `color_convert_common.glsl` (YCbCr → RGB
    // math, transfer closed-forms, `TRANSFER_*` / `FLAG_APPLY_TRANSFER`
    // constants). It is vendored into this crate's own `src/shaders/` rather
    // than referenced across a workspace-relative path into streamlib-engine,
    // so the crate is self-contained and compiles from the registry off-tree
    // (a registry consumer has no sibling engine source tree). It mirrors
    // `streamlib-engine/src/vulkan/rhi/shaders/color_convert_common.glsl` and
    // must stay in sync with it if the color math changes.
    let shader_include_dir = "src/shaders";
    println!(
        "cargo:rerun-if-changed={}/color_convert_common.glsl",
        shader_include_dir
    );

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg(format!("-fshader-stage={stage}"))
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
