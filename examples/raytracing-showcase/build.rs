// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)]

//! Compile the showcase ray-tracing shaders to SPIR-V via `glslc`.
//! Vulkan 1.2 / SPIR-V 1.4 are required for `SPV_KHR_ray_tracing`
//! opcodes — the streamlib build script uses the same target settings.

fn main() {
    let shaders: &[(&str, &str, &str)] = &[
        ("shaders/raytracing_showcase.rgen", "raytracing_showcase.rgen.spv", "rgen"),
        ("shaders/raytracing_showcase.rmiss", "raytracing_showcase.rmiss.spv", "rmiss"),
        ("shaders/raytracing_showcase.rchit", "raytracing_showcase.rchit.spv", "rchit"),
    ];

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    for (src, dst, stage) in shaders {
        let src_path = std::path::Path::new(src);
        let dst_path: std::path::PathBuf = std::path::Path::new(&out_dir).join(dst);
        println!("cargo:rerun-if-changed={}", src);
        let status = std::process::Command::new("glslc")
            .arg(format!("-fshader-stage={stage}"))
            .arg("--target-env=vulkan1.2")
            .arg("--target-spv=spv1.4")
            .arg("-O")
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");
        assert!(status.success(), "glslc failed to compile {}", src);
    }
}
