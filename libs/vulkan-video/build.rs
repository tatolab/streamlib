// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build script: compiles GLSL compute shaders to SPIR-V using glslc.

use std::path::Path;
use std::process::Command;

fn main() {
    let shaders = [
        ("shaders/rgb_to_nv12.comp", "rgb_to_nv12.spv"),
        ("shaders/nv12_to_rgb.comp", "nv12_to_rgb.spv"),
    ];

    let out_dir = std::env::var("OUT_DIR").unwrap();

    for (src, dst) in &shaders {
        let src_path = Path::new(src);
        let dst_path = Path::new(&out_dir).join(dst);

        println!("cargo:rerun-if-changed={}", src);

        let status = Command::new("glslc")
            .arg("-fshader-stage=compute")
            .arg("-O")
            .arg(src_path)
            .arg("-o")
            .arg(&dst_path)
            .status()
            .expect("Failed to run glslc. Install the Vulkan SDK or ensure glslc is in PATH.");

        assert!(
            status.success(),
            "glslc failed to compile {}",
            src
        );
    }
}
