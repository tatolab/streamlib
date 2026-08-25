// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: compiles the fused JPEG decode compute shader at
//! `src/shaders/jpeg_decode.comp` to SPIR-V and stages the artifact in
//! `OUT_DIR` for `include_bytes!` to consume at compile time. Linux-only —
//! the GPU kernel is gated behind `target_os = "linux"`.
//!
//! Compiled through the pinned `shaderc` crate rather than the `glslc`
//! binary, for the reason the engine's build script gives: the pinned crate
//! is the compiler whose version the pipeline-cache key assumes, and a
//! binary found on `PATH` is not.

fn main() {
    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};

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

    let compiler = shaderc::Compiler::new().expect("failed to create the shaderc compiler");
    let mut options =
        shaderc::CompileOptions::new().expect("failed to create shaderc compile options");
    options.set_optimization_level(shaderc::OptimizationLevel::Performance);
    // The `glslc -I` this replaces.
    options.set_include_callback(move |requested, _include_type, _requesting, _depth| {
        let resolved = Path::new(shader_include_dir).join(requested);
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| format!("failed to read include {}: {e}", resolved.display()))?;
        Ok(shaderc::ResolvedInclude {
            resolved_name: resolved.to_string_lossy().into_owned(),
            content,
        })
    });

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        assert_eq!(*stage, "compute", "only compute shaders are compiled here");
        let source_text = std::fs::read_to_string(src_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", src_path.display()));
        let compiled = compiler
            .compile_into_spirv(
                &source_text,
                shaderc::ShaderKind::Compute,
                src,
                "main",
                Some(&options),
            )
            .unwrap_or_else(|e| panic!("failed to compile {src}: {e}"));
        std::fs::write(&dst_path, compiled.as_binary_u8())
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", dst_path.display()));
    }
}
