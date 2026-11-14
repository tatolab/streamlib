// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Compiles the fixtures' shaders to SPIR-V through the Slang compiler the
//! workspace stages into `OUT_DIR` for `include_bytes!`.

use std::path::{Path, PathBuf};

use streamlib_slang_shader_compiler::{
    ShaderCompilationTargetStage as Stage, compile_a_slang_shader_file,
    open_the_workspace_staged_slang_compiler, workspace_staged_slang_compiler_library,
};

fn main() {
    // `CARGO_CFG_TARGET_OS` rather than `#[cfg(target_os = ...)]`, which in a
    // build script names the host compiling it rather than the target these
    // blobs are staged for — the same distinction the engine's own build
    // script draws.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    if target_os != "linux" && target_os != "macos" {
        return;
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_staged_slang_compiler_library(&workspace_root).display()
    );
    let slang_compiler = open_the_workspace_staged_slang_compiler(&workspace_root)
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    for (shader_source, stage, spirv_file_name) in [
        (
            "shaders/cpu_ref_doubler.slang",
            Stage::Compute,
            "cpu_ref_doubler.spv",
        ),
        (
            "shaders/graphics_kernel_smoke_vertex.slang",
            Stage::Vertex,
            "graphics_kernel_smoke_vert.spv",
        ),
        (
            "shaders/graphics_kernel_smoke_fragment.slang",
            Stage::Fragment,
            "graphics_kernel_smoke_frag.spv",
        ),
        (
            "shaders/ray_tracing_kernel_smoke_ray_generation.slang",
            Stage::RayGeneration,
            "ray_tracing_kernel_smoke_rgen.spv",
        ),
        (
            "shaders/ray_tracing_kernel_smoke_ray_miss.slang",
            Stage::RayMiss,
            "ray_tracing_kernel_smoke_rmiss.spv",
        ),
        (
            "shaders/ray_tracing_kernel_smoke_ray_closest_hit.slang",
            Stage::RayClosestHit,
            "ray_tracing_kernel_smoke_rchit.spv",
        ),
    ] {
        println!("cargo:rerun-if-changed={shader_source}");
        compile_a_slang_shader_file(
            &slang_compiler,
            Path::new(shader_source),
            stage,
            &[],
            &[],
            &out_dir.join(spirv_file_name),
        )
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    }
}
