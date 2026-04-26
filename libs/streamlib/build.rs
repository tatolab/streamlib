// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Build script: links Metal on Apple platforms; on Linux compiles the
//! Vulkan compute shaders this crate ships (`vulkan/rhi/shaders/*.comp`) to
//! SPIR-V via `glslc` and stages the artifacts in `OUT_DIR` for
//! `include_bytes!` to consume at compile time.

fn main() {
    // Link Metal framework on macOS for MP4 writer
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=Metal");
    }

    #[cfg(target_os = "linux")]
    compile_compute_shaders();
}

#[cfg(target_os = "linux")]
fn compile_compute_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // Single-variant compute shaders: each entry produces one SPIR-V module.
    // The RHI consumes them via `include_bytes!(concat!(env!("OUT_DIR"), …))`.
    // Add new compute kernels here.
    let shaders: &[(&str, &str)] = &[
        ("src/vulkan/rhi/shaders/nv12_to_bgra.comp", "nv12_to_bgra.spv"),
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");

    for (src, dst) in shaders {
        let src_path = Path::new(src);
        let dst_path: PathBuf = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg("-O")
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");

        assert!(status.success(), "glslc failed to compile {}", src);
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
}
