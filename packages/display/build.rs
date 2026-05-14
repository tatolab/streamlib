// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Build script: compiles the display-blit vertex + fragment shaders to
//! SPIR-V via `glslc` on Linux. The artifacts land in `OUT_DIR` and the
//! display processor `include_bytes!`'s them at compile time.

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
        ("src/shaders/display_blit.vert", "display_blit.vert.spv", "vertex"),
        ("src/shaders/display_blit.frag", "display_blit.frag.spv", "fragment"),
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");

    for (src, dst, stage) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg(format!("-fshader-stage={stage}"))
            .arg("-O")
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");

        assert!(status.success(), "glslc failed to compile {}", src);
    }
}
