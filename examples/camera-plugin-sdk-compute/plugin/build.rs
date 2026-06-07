// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Codegen + Vulkan compute-shader compilation for the engine-free
//! grayscale-compute plugin.
//!
//! `grayscale_compute.rs` embeds the resulting SPIR-V via
//! `include_bytes!(concat!(env!("OUT_DIR"), "/grayscale.comp.spv"))` and
//! hands it to `GpuContextFullAccess::create_compute_kernel` — no raw
//! `HostVulkanDevice`, so the plugin stays cdylib-safe as a
//! separately-built `.slpkg`.

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();
    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let shaders: &[(&str, &str, &str)] = &[("shaders/grayscale.comp", "grayscale.comp.spv", "compute")];

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
