// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Compiles the fixture shaders to SPIR-V through the pinned `shaderc` crate.
//!
//! Not the `glslc` binary, for the reason the engine's build script gives: the
//! pinned crate is the compiler whose version the pipeline-cache key assumes,
//! and a binary found on `PATH` is not. A fixture compiled by a different
//! compiler from the engine it exercises is the one blob in the tree that
//! could disagree with what it is testing.

fn main() {
    #[cfg(target_os = "linux")]
    {
        compile_cpu_ref_doubler();
        compile_graphics_kernel_smoke();
        compile_ray_tracing_kernel_smoke();
    }
}

/// Optimization strips every `OpName`, and a binding with no reflected name
/// cannot be dispatched against by name. The engine's own `build.rs` applies
/// this uniformly for that reason; a fixture compiled without it would be the
/// one blob in the tree whose bindings cannot be bound.
///
/// `set_generate_debug_info` is the crate's spelling of `glslc -g`.
#[cfg(target_os = "linux")]
fn optimized_options_keeping_binding_names() -> shaderc::CompileOptions<'static> {
    let mut options =
        shaderc::CompileOptions::new().expect("failed to create shaderc compile options");
    options.set_optimization_level(shaderc::OptimizationLevel::Performance);
    options.set_generate_debug_info();
    options
}

/// Compile one fixture shader and stage it where `include_bytes!` expects it.
#[cfg(target_os = "linux")]
fn compile_fixture_shader(
    source_path: &str,
    shader_kind: shaderc::ShaderKind,
    destination_file_name: &str,
    options: &shaderc::CompileOptions<'_>,
) {
    use std::path::{Path, PathBuf};

    println!("cargo:rerun-if-changed={source_path}");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let destination_path: PathBuf = Path::new(&out_dir).join(destination_file_name);

    let compiler = shaderc::Compiler::new().expect("failed to create the shaderc compiler");
    let source_text = std::fs::read_to_string(source_path)
        .unwrap_or_else(|e| panic!("failed to read {source_path}: {e}"));
    let compiled = compiler
        .compile_into_spirv(
            &source_text,
            shader_kind,
            source_path,
            "main",
            Some(options),
        )
        .unwrap_or_else(|e| panic!("failed to compile {source_path}: {e}"));

    std::fs::write(&destination_path, compiled.as_binary_u8())
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", destination_path.display()));
}

#[cfg(target_os = "linux")]
fn compile_cpu_ref_doubler() {
    compile_fixture_shader(
        "shaders/cpu_ref_doubler.comp",
        shaderc::ShaderKind::Compute,
        "cpu_ref_doubler.spv",
        &optimized_options_keeping_binding_names(),
    );
}

#[cfg(target_os = "linux")]
fn compile_graphics_kernel_smoke() {
    let options = optimized_options_keeping_binding_names();
    for (source_path, shader_kind, destination_file_name) in [
        (
            "shaders/graphics_kernel_smoke.vert",
            shaderc::ShaderKind::Vertex,
            "graphics_kernel_smoke_vert.spv",
        ),
        (
            "shaders/graphics_kernel_smoke.frag",
            shaderc::ShaderKind::Fragment,
            "graphics_kernel_smoke_frag.spv",
        ),
    ] {
        compile_fixture_shader(source_path, shader_kind, destination_file_name, &options);
    }
}

#[cfg(target_os = "linux")]
fn compile_ray_tracing_kernel_smoke() {
    // RT shaders need Vulkan 1.2 + SPIR-V 1.4 minimum for the
    // SPV_KHR_ray_tracing opcodes — the default target is Vulkan 1.0 /
    // SPIR-V 1.0, which silently drops the GL_EXT_ray_tracing bindings.
    let mut options = optimized_options_keeping_binding_names();
    options.set_target_env(
        shaderc::TargetEnv::Vulkan,
        shaderc::EnvVersion::Vulkan1_2 as u32,
    );
    options.set_target_spirv(shaderc::SpirvVersion::V1_4);

    for (source_path, shader_kind, destination_file_name) in [
        (
            "shaders/ray_tracing_kernel_smoke.rgen",
            shaderc::ShaderKind::RayGeneration,
            "ray_tracing_kernel_smoke_rgen.spv",
        ),
        (
            "shaders/ray_tracing_kernel_smoke.rmiss",
            shaderc::ShaderKind::Miss,
            "ray_tracing_kernel_smoke_rmiss.spv",
        ),
        (
            "shaders/ray_tracing_kernel_smoke.rchit",
            shaderc::ShaderKind::ClosestHit,
            "ray_tracing_kernel_smoke_rchit.spv",
        ),
    ] {
        compile_fixture_shader(source_path, shader_kind, destination_file_name, &options);
    }
}
