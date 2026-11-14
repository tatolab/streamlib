// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: wherever the Vulkan RHI is compiled, compiles the Vulkan
//! compute, vertex, fragment, and ray-tracing shaders this crate ships
//! (`vulkan/rhi/shaders/*.slang`) to SPIR-V through the Slang compiler and
//! stages the artifacts in `OUT_DIR` for `include_bytes!` to consume at
//! compile time; on Linux compiles the PipeWire shims against the
//! vendored PipeWire/SPA headers; and stamps the engine's build id, which a
//! helper process checks against its parent's.

#[path = "src/core/engine_build_id_composition.rs"]
mod engine_build_id_composition;

fn main() {
    stamp_the_engine_build_id();

    // `CARGO_CFG_TARGET_OS` rather than `#[cfg(target_os = ...)]`, which in a
    // build script names the host that is compiling it — so a `cfg` gate here
    // built the shaders when cross-compiling to macOS from Linux and not when
    // building natively on a Mac, which is the opposite of what either needs.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");

    // Every target the Vulkan RHI compiles for needs its SPIR-V staged, or
    // the `include_bytes!` sites fail to read them.
    if target_os == "linux" || target_os == "macos" {
        compile_shaders();
    }

    if target_os == "linux" {
        compile_pipewire_shims();
    }
}

/// Hand the compiler `STREAMLIB_ENGINE_BUILD_ID_FROM_BUILD_SCRIPT`.
///
/// The nonce is minted each time this script runs, so the script reruns
/// whenever the code compiled into the engine changes: its own sources and
/// manifest, every crate it links through a path dependency, and the lockfile
/// pinning the rest. Otherwise a rebuilt engine would keep the id of the build
/// it replaced and a stale helper would pass the check. The checked-out commit
/// is deliberately not watched — every commit would rebuild the engine and
/// everything above it — so the sha names the commit checked out when the
/// script last ran. A runtime unit compiles the engine twice, into the lend's
/// `_engine` and into `tatolabd`, so its build hands both compiles one nonce.
fn stamp_the_engine_build_id() {
    use engine_build_id_composition::{
        RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE, compose_engine_build_id,
        git_sha_of_the_checkout_containing, mint_per_build_nonce,
        path_dependency_directories_linked_into, runtime_unit_build_nonce_from,
    };

    let manifest_directory = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"),
    );
    let workspace_manifest_directory = manifest_directory.join("../..");
    let crate_directories_compiled_into_the_engine = std::iter::once(manifest_directory.clone())
        .chain(path_dependency_directories_linked_into(
            &manifest_directory,
            &workspace_manifest_directory,
        ));
    let watched_paths = crate_directories_compiled_into_the_engine
        .flat_map(|crate_directory| {
            ["src", "Cargo.toml", "build.rs"].map(|entry| crate_directory.join(entry))
        })
        .chain(std::iter::once(
            workspace_manifest_directory.join("Cargo.lock"),
        ));
    // Only paths that exist: cargo reruns a script on every build while a
    // watched path is missing.
    for watched_path in watched_paths.filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", watched_path.display());
    }

    println!("cargo:rerun-if-env-changed={RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE}");
    let handed_nonce = std::env::var(RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE).ok();
    let per_build_nonce = match runtime_unit_build_nonce_from(handed_nonce.as_deref()) {
        Ok(Some(runtime_unit_build_nonce)) => runtime_unit_build_nonce,
        Ok(None) => mint_per_build_nonce()
            .expect("could not read /dev/urandom for the engine build id's per-build nonce"),
        Err(refusal) => panic!("{refusal}"),
    };
    let engine_build_id = compose_engine_build_id(
        &std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION not set"),
        git_sha_of_the_checkout_containing(&manifest_directory).as_deref(),
        &per_build_nonce,
    );
    println!("cargo:rustc-env=STREAMLIB_ENGINE_BUILD_ID_FROM_BUILD_SCRIPT={engine_build_id}");
}

/// Compile the header-only half of every PipeWire arm.
///
/// SPA's pod builders and parsers are `static inline` C with no shared object
/// behind them, so `dlopen` cannot reach them and they have to be compiled in.
/// The shims call libpipewire only through pointers Rust filled with `dlsym`,
/// so this adds no `DT_NEEDED` entry — the invariant
/// `sdk/streamlib-python-wheel/tests_against_the_runtime_unit_lend/test_wheel_portability.py` enforces.
fn compile_pipewire_shims() {
    const SHIM_SOURCES: &[&str] = &[
        "src/linux/pipewire_entry_points.c",
        "src/linux/pipewire_audio_shim.c",
        "src/linux/pipewire_video_source_shim.c",
    ];
    const SHIM_HEADERS: &[&str] = &[
        "src/linux/pipewire_entry_points.h",
        "src/linux/pipewire_audio_shim.h",
        "src/linux/pipewire_video_source_shim.h",
    ];
    const VENDORED_HEADERS: &str = "../../vendor/pipewire-headers/include";

    for source in SHIM_SOURCES.iter().chain(SHIM_HEADERS) {
        println!("cargo:rerun-if-changed={source}");
    }
    println!("cargo:rerun-if-changed={VENDORED_HEADERS}");

    let mut build = cc::Build::new();
    // SPA's asserts print and abort on an internal precondition failure. Off
    // when Rust's own assertions are off, matching how
    // `vendor/tatolab-vulkanalia-vma/build.rs` treats VMA's.
    if std::env::var("DEBUG").as_deref() == Ok("false") {
        build.define("NDEBUG", None);
    }
    build
        .files(SHIM_SOURCES)
        .include(VENDORED_HEADERS)
        .include("src/linux")
        .std("c11")
        // SPA's headers reach for `strnlen` and friends behind `_GNU_SOURCE`,
        // which `-std=c11` (rather than `gnu11`) would otherwise hide.
        .define("_GNU_SOURCE", None)
        .warnings(true)
        .compile("streamlib_pipewire_shims");
}

/// Compile every shader the engine ships to SPIR-V through the Slang compiler
/// the workspace stages, with the run-time compiler's settings, into `OUT_DIR`
/// under the name its `include_bytes!` site reads.
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use streamlib_slang_shader_compiler::{
        ShaderCompilationTargetStage as Stage, SlangPreprocessorMacro, compile_a_slang_shader_file,
        open_the_workspace_staged_slang_compiler, workspace_staged_slang_compiler_library,
    };

    const SHADER_DIRECTORY: &str = "src/vulkan/rhi/shaders";
    let shaders: &[(&str, Stage, &str)] = &[
        // Trivial pipelines built+dropped at device init to force the
        // driver's shader-compiler init in a controlled state — see
        // HostVulkanDevice::prewarm_pipeline_compiler /
        // prewarm_graphics_pipeline.
        ("prewarm_compute.slang", Stage::Compute, "prewarm.spv"),
        ("prewarm_vertex.slang", Stage::Vertex, "prewarm.vert.spv"),
        (
            "prewarm_fragment.slang",
            Stage::Fragment,
            "prewarm.frag.spv",
        ),
        (
            "color_convert_nv12_buffer_to_rgba.slang",
            Stage::Compute,
            "color_convert_nv12_buffer_to_rgba.spv",
        ),
        (
            "color_convert_yuyv_buffer_to_rgba.slang",
            Stage::Compute,
            "color_convert_yuyv_buffer_to_rgba.spv",
        ),
        (
            "color_convert_rgba_image_to_yuyv_buffer.slang",
            Stage::Compute,
            "color_convert_rgba_image_to_yuyv_buffer.spv",
        ),
        (
            "color_convert_rgba_image_to_nv12_buffer.slang",
            Stage::Compute,
            "color_convert_rgba_image_to_nv12_buffer.spv",
        ),
        (
            "color_convert_rgba_image_to_rgba_image.slang",
            Stage::Compute,
            "color_convert_rgba_image_to_rgba_image.spv",
        ),
        ("tone_curve.slang", Stage::Compute, "tone_curve.spv"),
        (
            "offscreen_blit_vertex.slang",
            Stage::Vertex,
            "offscreen_blit_vertex.slang.spv",
        ),
        (
            "offscreen_blit_fragment.slang",
            Stage::Fragment,
            "offscreen_blit_fragment.slang.spv",
        ),
        // The read-one-write-another conformance shader for named N-binding
        // compute dispatch.
        (
            "test_read_one_write_another.slang",
            Stage::Compute,
            "test_read_one_write_another.spv",
        ),
        // The SampledImage binding kind's test shader.
        (
            "test_sampled_image.slang",
            Stage::Compute,
            "test_sampled_image.spv",
        ),
        (
            "raytracing_test_ray_generation.slang",
            Stage::RayGeneration,
            "raytracing_test.rgen.spv",
        ),
        (
            "raytracing_test_ray_miss.slang",
            Stage::RayMiss,
            "raytracing_test.rmiss.spv",
        ),
        (
            "raytracing_test_ray_closest_hit.slang",
            Stage::RayClosestHit,
            "raytracing_test.rchit.spv",
        ),
        (
            "raytracing_showcase_ray_generation.slang",
            Stage::RayGeneration,
            "raytracing_showcase.rgen.spv",
        ),
        (
            "raytracing_showcase_ray_miss.slang",
            Stage::RayMiss,
            "raytracing_showcase.rmiss.spv",
        ),
        (
            "raytracing_showcase_ray_closest_hit.slang",
            Stage::RayClosestHit,
            "raytracing_showcase.rchit.spv",
        ),
    ];
    // Imported by the color converters and the tone curve.
    const SHARED_SHADER_MODULES: &[&str] = &["color_convert_common.slang"];

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_staged_slang_compiler_library(&workspace_root).display()
    );
    let slang_compiler = open_the_workspace_staged_slang_compiler(&workspace_root)
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let module_search_directories = [PathBuf::from(SHADER_DIRECTORY)];
    for shared_shader_module in SHARED_SHADER_MODULES {
        println!("cargo:rerun-if-changed={SHADER_DIRECTORY}/{shared_shader_module}");
    }

    let compile = |source_file_name: &str,
                   stage: Stage,
                   preprocessor_macros: &[SlangPreprocessorMacro<'_>],
                   compiled_file_name: &str| {
        let source_path = Path::new(SHADER_DIRECTORY).join(source_file_name);
        println!("cargo:rerun-if-changed={}", source_path.display());
        compile_a_slang_shader_file(
            &slang_compiler,
            &source_path,
            stage,
            &module_search_directories,
            preprocessor_macros,
            &out_dir.join(compiled_file_name),
        )
        .unwrap_or_else(|refusal| panic!("{refusal}"));
    };
    for (source_file_name, stage, compiled_file_name) in shaders {
        compile(source_file_name, *stage, &[], compiled_file_name);
    }

    // One source compiled once per input count, for the parameterized
    // descriptor-management tests.
    for input_count in ["1", "2", "4", "8"] {
        compile(
            "test_blend.slang",
            Stage::Compute,
            &[SlangPreprocessorMacro {
                name: "INPUT_COUNT",
                value: input_count,
            }],
            &format!("test_blend_{input_count}.spv"),
        );
    }
}
