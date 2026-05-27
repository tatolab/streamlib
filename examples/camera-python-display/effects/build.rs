// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Codegen + Vulkan shader compilation for the camera-python-display
//! effects package.
//!
//! The two graphics-kernel wrappers in this crate
//! (`blending_compositor.rs`, `crt_film_grain.rs`) are sandboxed
//! scenario content for the camera-python-display demo. Each wrapper
//! drives its dispatch through the engine RHI's cdylib-safe
//! `VulkanGraphicsKernel::offscreen_render` + `RhiCommandRecorder`
//! surfaces, so this crate stays inside the boundary-check rule —
//! no `vulkanalia` dep, no allowlist exception.
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
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let glslc = std::env::var("GLSLC").unwrap_or_else(|_| "glslc".to_string());
        let status = Command::new(&glslc)
            .arg(format!("-fshader-stage={stage}"))
            .arg("-O")
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
