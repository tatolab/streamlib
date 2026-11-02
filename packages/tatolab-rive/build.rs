// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Builds rive-runtime at a pinned revision with rive-runtime's own build
//! script, in one premake workspace with this wheel's C++ bridge, and links
//! the static libraries it produces.
//!
//! A checkout named by `TATOLAB_RIVE_RUNTIME_DIRECTORY` is used as it is;
//! otherwise the pinned revision is fetched into `OUT_DIR`. The build needs
//! `git`, `ninja`, `clang`, `make`, `python3`, `glslangValidator` and
//! `spirv-opt` on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

const RIVE_RUNTIME_REPOSITORY: &str = "https://github.com/rive-app/rive-runtime";
const RIVE_RUNTIME_REVISION: &str = "788f82c2a11d16fa7a4af865ff383a87e976c4ab";
const RIVE_RUNTIME_DIRECTORY_ENVIRONMENT_VARIABLE: &str = "TATOLAB_RIVE_RUNTIME_DIRECTORY";

/// `build_rive.sh` arguments: a release build of the runtime with text and
/// layout, the Vulkan renderer only, position-independent for a cdylib and
/// without LTO so the system linker reads the objects.
const RIVE_BUILD_ARGUMENTS: [&str; 8] = [
    "ninja",
    "release",
    "--with_vulkan",
    "--no_gl",
    "--with_rive_text",
    "--with_rive_layout",
    "--with-pic",
    "--no-lto",
];

const BRIDGE_SOURCES: [&str; 3] = [
    "native/premake5.lua",
    "native/tatolab_rive_external_renderer_bridge.cpp",
    "native/tatolab_rive_external_renderer_bridge.h",
];

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    if target_os != "linux" {
        return;
    }
    println!("cargo:rerun-if-env-changed={RIVE_RUNTIME_DIRECTORY_ENVIRONMENT_VARIABLE}");
    for bridge_source in BRIDGE_SOURCES {
        println!("cargo:rerun-if-changed={bridge_source}");
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rive_runtime_directory = match std::env::var_os(RIVE_RUNTIME_DIRECTORY_ENVIRONMENT_VARIABLE)
    {
        Some(named_checkout) => PathBuf::from(named_checkout),
        None => fetch_the_pinned_rive_runtime_revision(&out_dir.join("rive-runtime")),
    };

    let bridge_workspace = out_dir.join("rive-bridge-workspace");
    std::fs::create_dir_all(&bridge_workspace).expect("the bridge workspace is created");
    let manifest_directory =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    for bridge_source in BRIDGE_SOURCES {
        let source = manifest_directory.join(bridge_source);
        let file_name = source.file_name().expect("each bridge source names a file");
        std::fs::copy(&source, bridge_workspace.join(file_name))
            .unwrap_or_else(|copy_failure| panic!("copying {}: {copy_failure}", source.display()));
    }

    let relative_build_output = "out/release";
    let status = Command::new(rive_runtime_directory.join("build/build_rive.sh"))
        .args(RIVE_BUILD_ARGUMENTS)
        .current_dir(&bridge_workspace)
        .env(
            RIVE_RUNTIME_DIRECTORY_ENVIRONMENT_VARIABLE,
            &rive_runtime_directory,
        )
        .env("RIVE_OUT", relative_build_output)
        .status()
        .expect("rive-runtime's build/build_rive.sh runs");
    assert!(status.success(), "rive-runtime's build failed: {status}");

    link_every_static_library_in(&bridge_workspace.join(relative_build_output));
}

fn fetch_the_pinned_rive_runtime_revision(checkout: &Path) -> PathBuf {
    let revision_marker = checkout.join(".tatolab-rive-runtime-revision");
    if std::fs::read_to_string(&revision_marker).ok().as_deref() == Some(RIVE_RUNTIME_REVISION) {
        return checkout.to_path_buf();
    }
    let _ = std::fs::remove_dir_all(checkout);
    std::fs::create_dir_all(checkout).expect("the rive-runtime checkout directory is created");
    for git_arguments in [
        vec!["init", "--quiet"],
        vec!["remote", "add", "origin", RIVE_RUNTIME_REPOSITORY],
        vec![
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "origin",
            RIVE_RUNTIME_REVISION,
        ],
        vec!["checkout", "--quiet", "FETCH_HEAD"],
    ] {
        let status = Command::new("git")
            .args(&git_arguments)
            .current_dir(checkout)
            .status()
            .expect("git runs");
        assert!(
            status.success(),
            "git {} failed in {}: {status}",
            git_arguments.join(" "),
            checkout.display()
        );
    }
    std::fs::write(&revision_marker, RIVE_RUNTIME_REVISION)
        .expect("the revision marker is written");
    checkout.to_path_buf()
}

/// Link every `lib*.a` the build wrote, as one group so their order does not
/// matter, then the C++ standard library they need.
fn link_every_static_library_in(build_output: &Path) {
    let mut static_libraries: Vec<PathBuf> = std::fs::read_dir(build_output)
        .unwrap_or_else(|read_failure| panic!("reading {}: {read_failure}", build_output.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "a")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("lib"))
        })
        .collect();
    static_libraries.sort();
    assert!(
        static_libraries
            .iter()
            .any(|path| path.ends_with("libtatolab_rive_external_renderer_bridge.a")),
        "rive-runtime's build wrote no bridge library into {}",
        build_output.display()
    );
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    for static_library in &static_libraries {
        println!("cargo:rustc-link-arg={}", static_library.display());
    }
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    println!("cargo:rustc-link-lib=dylib=stdc++");
}
