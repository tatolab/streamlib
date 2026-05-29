// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println!/eprintln! for `cargo:` directives

//! Builds the JTD config dataclass shim and (on Linux) compiles every
//! shader the fixture's PluginAbiObject round-trip needs via the system
//! `glslc`. Matches the streamlib-engine crate's own build.rs shader
//! pipeline so the fixture compiles in the same toolchain shape
//! consumers see.

fn main() {
    streamlib_jtd_codegen::build_rs::run_for_rust_crate();

    let target = std::env::var("TARGET").expect("TARGET env var set by cargo");
    println!("cargo:rustc-env=STREAMLIB_HOST_TARGET={}", target);
    println!("cargo:rerun-if-env-changed=TARGET");

    #[cfg(target_os = "linux")]
    compile_shaders();
}

#[cfg(target_os = "linux")]
fn compile_shaders() {
    let shaders: &[(&str, &str, &str)] = &[
        ("src/shaders/trivial_compute.comp", "trivial_compute.spv", "compute"),
        ("src/shaders/trivial_vert.vert", "trivial_vert.spv", "vertex"),
        ("src/shaders/trivial_frag.frag", "trivial_frag.spv", "fragment"),
        ("src/shaders/trivial_rgen.rgen", "trivial_rgen.spv", "rgen"),
        ("src/shaders/trivial_rmiss.rmiss", "trivial_rmiss.spv", "rmiss"),
        ("src/shaders/trivial_rchit.rchit", "trivial_rchit.spv", "rchit"),
    ];
    for (src, out, stage) in shaders {
        compile_shader(src, out, stage);
    }
}

#[cfg(target_os = "linux")]
fn compile_shader(src: &str, output_name: &str, stage: &str) {
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"));

    let shader_src = manifest_dir.join(src);
    let shader_out = out_dir.join(output_name);

    println!("cargo:rerun-if-changed={}", shader_src.display());

    // Ray-tracing stages need SPIR-V 1.4 + Vulkan 1.2 target env.
    let mut cmd = Command::new("glslc");
    cmd.args(["-O", &format!("-fshader-stage={stage}")]);
    if matches!(stage, "rgen" | "rmiss" | "rchit") {
        cmd.args(["--target-env=vulkan1.2", "--target-spv=spv1.4"]);
    }
    cmd.args([
        shader_src.to_str().expect("shader path utf-8"),
        "-o",
        shader_out.to_str().expect("output path utf-8"),
    ]);

    let status = cmd
        .status()
        .expect("invoking glslc — install shaderc-tools if missing");
    assert!(
        status.success(),
        "glslc failed to compile {}",
        shader_src.display()
    );
}
