// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Compile the example's Vulkan shaders to SPIR-V via `glslc`.
//!
//! These shaders + their kernel wrappers (`blending_compositor.rs`,
//! `crt_film_grain.rs`) are sandboxed scenario content for this demo,
//! NOT engine primitives — they previously lived in
//! `libs/streamlib/src/vulkan/rhi/` and were relocated in #487 because
//! they encode application-specific effect logic (cyberpunk N54 News
//! PiP chrome, 80s Blade Runner CRT post-process) that doesn't belong
//! in the engine. The wrappers migrate into RDG passes when #631 ships
//! and this build.rs (along with the example's `vulkanalia` dep and
//! the boundary-check allowlist exception) goes away.
//!
//! `main.rs` embeds the resulting SPIR-V via
//! `include_bytes!(concat!(env!("OUT_DIR"), …))`.

fn main() {
    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let shaders: &[(&str, &str, &str)] = &[
        (
            "src/shaders/blending_compositor.vert",
            "blending_compositor.vert.spv",
            "vertex",
        ),
        (
            "src/shaders/blending_compositor.frag",
            "blending_compositor.frag.spv",
            "fragment",
        ),
        (
            "src/shaders/crt_film_grain.vert",
            "crt_film_grain.vert.spv",
            "vertex",
        ),
        (
            "src/shaders/crt_film_grain.frag",
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
