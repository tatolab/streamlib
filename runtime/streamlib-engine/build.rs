// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: links Metal on Apple platforms; on Linux compiles the
//! Vulkan compute, vertex, fragment, and ray-tracing shaders this crate
//! ships (`vulkan/rhi/shaders/*.{comp,vert,frag,rgen,rmiss,rchit}`) to
//! SPIR-V and stages the artifacts in `OUT_DIR` for `include_bytes!` to
//! consume at compile time.
//!
//! Compiled through the `shaderc` crate this crate already links, not the
//! `glslc` binary. `glslc` is a thin CLI over the same libshaderc, so the
//! emitted SPIR-V is the same — but the binary is whichever one happens to
//! be on `PATH`, while the crate is pinned `=0.10.1` precisely because the
//! compiler version is part of the pipeline-cache key. Going through the
//! crate is what makes a shader baked here and one compiled at runtime
//! provably the same compiler, and it drops the Vulkan SDK from the list of
//! things a contributor needs installed to build the engine.

fn main() {
    // Link Metal framework on macOS for MP4 writer
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=Metal");
    }

    #[cfg(target_os = "linux")]
    compile_shaders();
}

/// Optimization strips every `OpName`, and a binding with no reflected name
/// cannot be dispatched against by name. Debug info is what carries the
/// shader's own spelling of each binding through to `rspirv-reflect`.
///
/// Applied uniformly — production shaders included — so "engine-compiled
/// SPIR-V keeps its binding names" is one rule with no exceptions list to
/// maintain. The cost is accepted deliberately: `-g` roughly doubles each
/// blob (it embeds the GLSL source, not just names), and changing the bytes
/// changes each driver pipeline-cache filename once, so the first run after
/// an upgrade recompiles pipelines cold.
///
/// `set_generate_debug_info` is the crate's spelling of `glslc -g`.
#[cfg(target_os = "linux")]
fn keep_binding_names(options: &mut shaderc::CompileOptions<'_>) {
    options.set_generate_debug_info();
}

/// The `-fshader-stage=` value each source is compiled as.
#[cfg(target_os = "linux")]
fn shader_kind_for_stage(stage: &str) -> shaderc::ShaderKind {
    match stage {
        "compute" => shaderc::ShaderKind::Compute,
        "vertex" => shaderc::ShaderKind::Vertex,
        "fragment" => shaderc::ShaderKind::Fragment,
        "rgen" => shaderc::ShaderKind::RayGeneration,
        "rmiss" => shaderc::ShaderKind::Miss,
        "rchit" => shaderc::ShaderKind::ClosestHit,
        other => panic!("no ShaderKind mapped for shader stage `{other}`"),
    }
}

/// Compile one GLSL source to SPIR-V and write it where `include_bytes!`
/// expects it.
///
/// Errors carry shaderc's own diagnostic, which names the file and line —
/// a `panic!` with that text reads the same as the `glslc` stderr it
/// replaces.
#[cfg(target_os = "linux")]
fn compile_shader_to_spirv(
    compiler: &shaderc::Compiler,
    options: &shaderc::CompileOptions<'_>,
    source_path: &std::path::Path,
    stage: &str,
    destination_path: &std::path::Path,
) {
    let source_text = std::fs::read_to_string(source_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", source_path.display()));

    let compiled = compiler
        .compile_into_spirv(
            &source_text,
            shader_kind_for_stage(stage),
            &source_path.to_string_lossy(),
            "main",
            Some(options),
        )
        .unwrap_or_else(|e| panic!("failed to compile {}: {e}", source_path.display()));

    std::fs::write(destination_path, compiled.as_binary_u8())
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", destination_path.display()));
}

/// Base options shared by every shader: optimized, binding names kept, and
/// `#include` resolved against the shader directory (the `glslc -I` this
/// replaces).
#[cfg(target_os = "linux")]
fn base_compile_options(shader_include_dir: &'static str) -> shaderc::CompileOptions<'static> {
    let mut options =
        shaderc::CompileOptions::new().expect("failed to create shaderc compile options");
    options.set_optimization_level(shaderc::OptimizationLevel::Performance);
    keep_binding_names(&mut options);
    options.set_include_callback(move |requested, _include_type, _requesting, _depth| {
        let resolved = std::path::Path::new(shader_include_dir).join(requested);
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| format!("failed to read include {}: {e}", resolved.display()))?;
        Ok(shaderc::ResolvedInclude {
            resolved_name: resolved.to_string_lossy().into_owned(),
            content,
        })
    });
    options
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};

    // Per-stage shader sources. Each entry produces one SPIR-V module
    // consumed via `include_bytes!(concat!(env!("OUT_DIR"), …))`.
    // Add new kernels (compute, vertex, fragment) here.
    let shaders: &[(&str, &str, &str)] = &[
        // Trivial pipelines built+dropped at device init to force the
        // driver's shader-compiler init in a controlled state — see
        // HostVulkanDevice::prewarm_pipeline_compiler /
        // prewarm_graphics_pipeline.
        (
            "src/vulkan/rhi/shaders/prewarm.comp",
            "prewarm.spv",
            "compute",
        ),
        (
            "src/vulkan/rhi/shaders/prewarm.vert",
            "prewarm.vert.spv",
            "vertex",
        ),
        (
            "src/vulkan/rhi/shaders/prewarm.frag",
            "prewarm.frag.spv",
            "fragment",
        ),
        (
            "src/vulkan/rhi/shaders/color_convert_nv12_buffer_to_rgba.comp",
            "color_convert_nv12_buffer_to_rgba.spv",
            "compute",
        ),
        (
            "src/vulkan/rhi/shaders/color_convert_yuyv_buffer_to_rgba.comp",
            "color_convert_yuyv_buffer_to_rgba.spv",
            "compute",
        ),
        (
            "src/vulkan/rhi/shaders/tone_curve.comp",
            "tone_curve.spv",
            "compute",
        ),
        (
            "src/vulkan/rhi/shaders/display_blit.vert",
            "display_blit.vert.spv",
            "vertex",
        ),
        (
            "src/vulkan/rhi/shaders/display_blit.frag",
            "display_blit.frag.spv",
            "fragment",
        ),
        // Vulkan Video codec layer (`vulkan/video/`) — RGB↔NV12
        // compute conversion used by SimpleEncoder::encode_image and
        // SimpleDecoder's RGBA output mode.
        (
            "src/vulkan/video/shaders/rgb_to_nv12.comp",
            "rgb_to_nv12.spv",
            "compute",
        ),
        (
            "src/vulkan/video/shaders/nv12_to_rgb.comp",
            "nv12_to_rgb.spv",
            "compute",
        ),
        // The read-one-write-another conformance shader for named N-binding
        // compute dispatch.
        (
            "src/vulkan/rhi/shaders/test_read_one_write_another.comp",
            "test_read_one_write_another.spv",
            "compute",
        ),
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    // `-I` for the converter common header. New compute shaders share
    // closed-form transfer / matrix math via `#include`.
    let shader_include_dir = "src/vulkan/rhi/shaders";
    println!(
        "cargo:rerun-if-changed={}/color_convert_common.glsl",
        shader_include_dir
    );

    let compiler = shaderc::Compiler::new().expect("failed to create the shaderc compiler");
    let options = base_compile_options(shader_include_dir);

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        compile_shader_to_spirv(&compiler, &options, src_path, stage, &dst_path);
    }

    // Standalone test shader for the SampledImage binding kind.
    {
        let test_sampled_image_src = "src/vulkan/rhi/shaders/test_sampled_image.comp";
        println!("cargo:rerun-if-changed={}", test_sampled_image_src);
        let dst_path: PathBuf = Path::new(&out_dir).join("test_sampled_image.spv");
        compile_shader_to_spirv(
            &compiler,
            &options,
            Path::new(test_sampled_image_src),
            "compute",
            &dst_path,
        );
    }

    // Parameterized test shaders: one .comp source compiled multiple times with
    // different `-DINPUT_COUNT=N` defines, producing one SPIR-V variant per
    // value. Used by parameterized descriptor-management tests.
    let test_blend_src = "src/vulkan/rhi/shaders/test_blend.comp";
    println!("cargo:rerun-if-changed={}", test_blend_src);
    for &n in &[1u32, 2, 4, 8] {
        let dst_path: PathBuf = Path::new(&out_dir).join(format!("test_blend_{n}.spv"));
        // One `CompileOptions` per variant: a macro definition is set on the
        // options object, so a shared one would accumulate every INPUT_COUNT.
        let mut variant_options = base_compile_options(shader_include_dir);
        variant_options.add_macro_definition("INPUT_COUNT", Some(&n.to_string()));
        compile_shader_to_spirv(
            &compiler,
            &variant_options,
            Path::new(test_blend_src),
            "compute",
            &dst_path,
        );
    }

    // Ray-tracing shaders. Need Vulkan 1.2 + SPIR-V 1.4 minimum for the
    // `SPV_KHR_ray_tracing` opcodes; `glslc`'s default target is
    // Vulkan 1.0 / SPIR-V 1.0 which silently drops `GL_EXT_ray_tracing`.
    let rt_shaders: &[(&str, &str, &str)] = &[
        (
            "src/vulkan/rhi/shaders/raytracing_test.rgen",
            "raytracing_test.rgen.spv",
            "rgen",
        ),
        (
            "src/vulkan/rhi/shaders/raytracing_test.rmiss",
            "raytracing_test.rmiss.spv",
            "rmiss",
        ),
        (
            "src/vulkan/rhi/shaders/raytracing_test.rchit",
            "raytracing_test.rchit.spv",
            "rchit",
        ),
        (
            "src/vulkan/rhi/shaders/raytracing_showcase.rgen",
            "raytracing_showcase.rgen.spv",
            "rgen",
        ),
        (
            "src/vulkan/rhi/shaders/raytracing_showcase.rmiss",
            "raytracing_showcase.rmiss.spv",
            "rmiss",
        ),
        (
            "src/vulkan/rhi/shaders/raytracing_showcase.rchit",
            "raytracing_showcase.rchit.spv",
            "rchit",
        ),
    ];

    let mut ray_tracing_options = base_compile_options(shader_include_dir);
    ray_tracing_options.set_target_env(
        shaderc::TargetEnv::Vulkan,
        shaderc::EnvVersion::Vulkan1_2 as u32,
    );
    ray_tracing_options.set_target_spirv(shaderc::SpirvVersion::V1_4);

    for (src, dst, stage) in rt_shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        compile_shader_to_spirv(&compiler, &ray_tracing_options, src_path, stage, &dst_path);
    }
}
