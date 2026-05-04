// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Compile `shaders/triangle.{vert,frag}` to SPIR-V via `glslc`. The
//! example's `main.rs` embeds the resulting `.spv` blobs via
//! `include_bytes!` and ships them to the polyglot processor as hex
//! strings in the processor config.

fn main() {
    #[cfg(target_os = "linux")]
    compile_graphics_shaders();
}

#[cfg(target_os = "linux")]
fn compile_graphics_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let shaders: &[(&str, &str)] = &[
        ("shaders/triangle.vert", "triangle.vert.spv"),
        ("shaders/triangle.frag", "triangle.frag.spv"),
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");

    for (src, dst) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let glslc = std::env::var("GLSLC").unwrap_or_else(|_| "glslc".to_string());
        let status = Command::new(&glslc)
            .arg("-O")
            .arg("--target-env=vulkan1.2")
            .arg("-o")
            .arg(&dst_path)
            .arg(src_path)
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
