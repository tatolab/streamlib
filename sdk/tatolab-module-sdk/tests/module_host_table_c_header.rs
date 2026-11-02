// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shipped C header compiles warning-free as C11 and as C++, and lays the
//! table out where the Rust mirror's layout regression tests do.

use std::path::{Path, PathBuf};
use std::process::Command;

fn module_sdk_directory() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn scratch_output_directory(test_name: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test_name);
    std::fs::create_dir_all(&directory).expect("the scratch directory is created");
    directory
}

/// The compiler `$<variable>` names, or `fallback`.
fn compiler_from_environment(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|compiler| !compiler.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn compile_against_the_header(compiler: &str, arguments: &[&str], source_path: &Path) {
    let include_directory = module_sdk_directory().join("include");
    let output = Command::new(compiler)
        .args(arguments)
        .arg("-I")
        .arg(&include_directory)
        .arg(source_path)
        .output()
        .unwrap_or_else(|spawn_error| panic!("`{compiler}` starts: {spawn_error}"));
    assert!(
        output.status.success(),
        "`{compiler}` refused {}:\n{}",
        source_path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_c_header_holds_the_rust_layout_regression_tests_sizes_and_offsets() {
    let object_path =
        scratch_output_directory("c_header_layout").join("module_host_table_c_header_layout.o");
    let object_path = object_path.to_str().expect("the scratch path is UTF-8");
    compile_against_the_header(
        &compiler_from_environment("CC", "cc"),
        &[
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wpedantic",
            "-Werror",
            "-c",
            "-o",
            object_path,
        ],
        &module_sdk_directory().join("tests/module_host_table_c_header_layout.c"),
    );
}

#[test]
fn the_c_header_compiles_as_cpp() {
    let output_directory = scratch_output_directory("c_header_as_cpp");
    let source_path = output_directory.join("module_host_table_c_header_as_cpp.cpp");
    std::fs::write(
        &source_path,
        "#include \"tatolab_module_host_table.h\"\n\
         static_assert(sizeof(ModuleHostTable) == 16 + 14 * sizeof(void *), \"ModuleHostTable size\");\n",
    )
    .expect("the C++ source is written");
    let object_path = output_directory.join("module_host_table_c_header_as_cpp.o");
    compile_against_the_header(
        &compiler_from_environment("CXX", "c++"),
        &[
            "-std=c++17",
            "-Wall",
            "-Wextra",
            "-Wpedantic",
            "-Werror",
            "-c",
            "-o",
            object_path.to_str().expect("the scratch path is UTF-8"),
        ],
        &source_path,
    );
}
