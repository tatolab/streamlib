// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Codegen + Vulkan shader compilation for the camera-python-display
//! effects package.
//!
//! The two graphics-kernel wrappers (`blending_compositor.rs`,
//! `crt_film_grain.rs`) and the sandboxed tone-mapper (`tone_mapper.rs`)
//! are sandboxed scenario content for the camera-python-display demo.
//! Each rides the engine-free plugin SDK's cdylib-safe FullAccess /
//! Limited primitives (`create_graphics_kernel` / `create_compute_kernel`
//! / `create_command_recorder` / `offscreen_render`), so this crate links
//! ONLY `streamlib-plugin-sdk` — no `streamlib` facade, no `vulkanalia`
//! dep, no boundary allowlist exception.
//!
//! `lib.rs` embeds the resulting SPIR-V via
//! `include_bytes!(concat!(env!("OUT_DIR"), …))`.

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();
    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // `(source, out_name, glslc_stage)`. The tone-mapper compute shader
    // `#include`s `color_convert_common.glsl` (also copied example-local),
    // resolved via the `-I shaders` include dir below.
    let shaders: &[(&str, &str, &str)] = &[
        (
            "shaders/blending_compositor.vert",
            "blending_compositor.vert.spv",
            "vertex",
        ),
        (
            "shaders/blending_compositor.frag",
            "blending_compositor.frag.spv",
            "fragment",
        ),
        (
            "shaders/crt_film_grain.vert",
            "crt_film_grain.vert.spv",
            "vertex",
        ),
        (
            "shaders/crt_film_grain.frag",
            "crt_film_grain.frag.spv",
            "fragment",
        ),
        ("shaders/tone_curve.comp", "tone_curve.comp.spv", "compute"),
    ];

    // Include dir for `#include "color_convert_common.glsl"` in the
    // tone-mapper compute shader. Harmless for the graphics stages (they
    // include nothing). Rerun the build when the shared header changes.
    let shader_include_dir = "shaders";
    println!(
        "cargo:rerun-if-changed={}/color_convert_common.glsl",
        shader_include_dir
    );

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let glslc = std::env::var("GLSLC").unwrap_or_else(|_| "glslc".to_string());
        let status = Command::new(&glslc)
            .arg(format!("-fshader-stage={stage}"))
            .arg("-O")
            .arg("-I")
            .arg(shader_include_dir)
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .unwrap_or_else(|e| {
                panic!(
                    "Failed to invoke `{}` to compile {}: {}. Install shaderc-tools / vulkan-tools.",
                    glslc, src, e
                );
            });
        assert!(
            status.success(),
            "{} compilation failed (exit: {:?})",
            src,
            status.code()
        );
    }
}
