// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::disallowed_macros)] // build.rs uses println! for `cargo:` directives

//! Builds rive-runtime at a pinned revision with rive-runtime's own build
//! script, in one premake workspace with this wheel's C++ bridge, and links
//! the static libraries it produces.
//!
//! A checkout named by `TATOLAB_RIVE_RUNTIME_DIRECTORY` is used as it is;
//! otherwise the pinned revision is fetched into `OUT_DIR`. The build needs
//! `git`, `ninja`, `clang`, `make`, `python3`, `glslangValidator`,
//! `spirv-opt`, `nm` and `objcopy` on `PATH`, and libuuid's headers for
//! rive-runtime's premake.

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

const RIVE_RENDERER_STATIC_LIBRARY: &str = "librive_pls_renderer.a";
const RIVE_RENDERER_VMA_SYMBOL_PREFIX: &str = "tatolab_rive_";

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

    let build_output = bridge_workspace.join(relative_build_output);
    give_the_rive_renderer_its_own_vma_symbols(&build_output.join(RIVE_RENDERER_STATIC_LIBRARY));
    link_every_static_library_in(&build_output);
}

/// Rename every VMA symbol in the Rive renderer's archive, definitions and
/// references alike, so its VMA (built with `VMA_DYNAMIC_VULKAN_FUNCTIONS=1`,
/// which rive-runtime's allocator setup relies on) never resolves to the
/// consumer RHI's VMA (built with it off) in this one shared library.
/// Idempotent: a symbol already carrying the prefix is left alone.
fn give_the_rive_renderer_its_own_vma_symbols(renderer_static_library: &Path) {
    let symbol_listing = Command::new("nm")
        .args(["--format=posix", "--no-sort"])
        .arg(renderer_static_library)
        .output()
        .expect("nm runs");
    assert!(
        symbol_listing.status.success(),
        "nm failed on {}: {}",
        renderer_static_library.display(),
        String::from_utf8_lossy(&symbol_listing.stderr)
    );
    let mut vma_symbols: Vec<&str> = std::str::from_utf8(&symbol_listing.stdout)
        .expect("nm lists symbols as UTF-8")
        .lines()
        .filter(|line| !line.ends_with(':'))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|symbol| {
            !symbol.starts_with(RIVE_RENDERER_VMA_SYMBOL_PREFIX)
                && (symbol.contains("vma") || symbol.contains("Vma") || symbol.contains("VMA"))
        })
        .collect();
    vma_symbols.sort_unstable();
    vma_symbols.dedup();
    if vma_symbols.is_empty() {
        return;
    }
    let symbol_renames: String = vma_symbols
        .iter()
        .map(|symbol| format!("{symbol} {RIVE_RENDERER_VMA_SYMBOL_PREFIX}{symbol}\n"))
        .collect();
    let symbol_renames_file = renderer_static_library.with_extension("vma-symbol-renames");
    std::fs::write(&symbol_renames_file, symbol_renames)
        .expect("the VMA symbol renames are written");
    let status = Command::new("objcopy")
        .arg(format!("--redefine-syms={}", symbol_renames_file.display()))
        .arg(renderer_static_library)
        .status()
        .expect("objcopy runs");
    assert!(
        status.success(),
        "objcopy could not rename the VMA symbols of {}: {status}",
        renderer_static_library.display()
    );
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
