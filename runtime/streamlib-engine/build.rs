// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: links Metal on Apple platforms; on Linux compiles the
//! Vulkan compute, vertex, fragment, and ray-tracing shaders this crate
//! ships (`vulkan/rhi/shaders/*.{comp,vert,frag,rgen,rmiss,rchit}`) to
//! SPIR-V via `glslc` and stages the artifacts in `OUT_DIR` for
//! `include_bytes!` to consume at compile time, and compiles the PipeWire
//! shims against the vendored PipeWire/SPA headers.

fn main() {
    // Link Metal framework on macOS for MP4 writer
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=Metal");
    }

    #[cfg(target_os = "linux")]
    compile_shaders();

    // `CARGO_CFG_TARGET_OS` rather than `#[cfg(target_os = ...)]`, which in a
    // build script names the host that is compiling it. Cross-checking the
    // Apple path from Linux (`cargo check --target aarch64-apple-darwin`) would
    // otherwise hand this file's sources to a cross toolchain that is not there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        compile_pipewire_shims();
    }
}

/// Compile the header-only half of every PipeWire arm.
///
/// SPA's pod builders and parsers are `static inline` C with no shared object
/// behind them, so `dlopen` cannot reach them and they have to be compiled in.
/// The shims call libpipewire only through pointers Rust filled with `dlsym`,
/// so this adds no `DT_NEEDED` entry — the invariant
/// `sdk/streamlib-python-wheel/tests/test_wheel_portability.py` enforces.
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

/// `glslc -O` strips every `OpName`, and a binding with no reflected name
/// cannot be dispatched against by name. Debug info is what carries the
/// shader's own spelling of each binding through to `rspirv-reflect`.
///
/// Applied uniformly — production shaders included — so "engine-compiled
/// SPIR-V keeps its binding names" is one rule with no exceptions list to
/// maintain. The cost is accepted deliberately: `-g` roughly doubles each
/// blob (it embeds the GLSL source, not just names), and changing the bytes
/// changes each driver pipeline-cache filename once, so the first run after
/// an upgrade recompiles pipelines cold.
#[cfg(target_os = "linux")]
const KEEP_BINDING_NAMES: &str = "-g";

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

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
            "src/vulkan/rhi/shaders/color_convert_rgba_image_to_yuyv_buffer.comp",
            "color_convert_rgba_image_to_yuyv_buffer.spv",
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

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg(format!("-fshader-stage={stage}"))
            .arg(KEEP_BINDING_NAMES)
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

    // Standalone test shader for the SampledImage binding kind.
    {
        let test_sampled_image_src = "src/vulkan/rhi/shaders/test_sampled_image.comp";
        println!("cargo:rerun-if-changed={}", test_sampled_image_src);
        let dst_path: PathBuf = Path::new(&out_dir).join("test_sampled_image.spv");
        let status = Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg(KEEP_BINDING_NAMES)
            .arg("-O")
            .arg(Path::new(test_sampled_image_src))
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc for test_sampled_image.comp");
        assert!(
            status.success(),
            "glslc failed to compile test_sampled_image.comp"
        );
    }

    // Parameterized test shaders: one .comp source compiled multiple times with
    // different `-DINPUT_COUNT=N` defines, producing one SPIR-V variant per
    // value. Used by parameterized descriptor-management tests.
    let test_blend_src = "src/vulkan/rhi/shaders/test_blend.comp";
    println!("cargo:rerun-if-changed={}", test_blend_src);
    for &n in &[1u32, 2, 4, 8] {
        let dst_path: PathBuf = Path::new(&out_dir).join(format!("test_blend_{n}.spv"));
        let status = Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg(KEEP_BINDING_NAMES)
            .arg("-O")
            .arg(format!("-DINPUT_COUNT={n}"))
            .arg(Path::new(test_blend_src))
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc for test_blend.comp");
        assert!(
            status.success(),
            "glslc failed to compile test_blend.comp with INPUT_COUNT={n}"
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

    for (src, dst, stage) in rt_shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg(format!("-fshader-stage={stage}"))
            .arg("--target-env=vulkan1.2")
            .arg("--target-spv=spv1.4")
            .arg(KEEP_BINDING_NAMES)
            .arg("-O")
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");

        assert!(status.success(), "glslc failed to compile {}", src);
    }
}
