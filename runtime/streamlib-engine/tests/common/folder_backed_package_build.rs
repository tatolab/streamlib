// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Building an in-tree folder-backed package from an integration test.
//!
//! A package's `[lib] path` points at a generated crate root, and cargo resolves
//! that path at target resolution — before any build script runs — so it has to
//! exist before cargo is invoked. Off the monorepo the build orchestrator's
//! pre-cargo staging step writes it; in-tree there is no orchestrator in the
//! loop, so a test that shells out to `cargo build -p <pkg>` writes it first,
//! through the same generator (`cargo xtask generate-crate-roots` is the CI
//! entry point to it).

use std::path::Path;

/// Write every in-tree folder-backed package's crate root, then
/// `cargo build -p <cargo_package_name>`. Panics with the cargo failure, which
/// is what the caller wants: a fixture cdylib that did not build cannot be
/// dlopen'd, and a silent skip would turn a load test green for the wrong
/// reason.
pub fn build_folder_backed_package(cargo_package_name: &str) {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    for package_dir in
        folder_backed_package_dirs(workspace_root).expect("discovering folder-backed packages")
    {
        let request =
            streamlib_processor_extract::crate_root::RustCrateRootGenerationRequest::for_package_dir(
                &package_dir,
            )
            .unwrap_or_else(|e| panic!("reading {}: {e}", package_dir.display()));
        streamlib_processor_extract::crate_root::write_generated_rust_crate_root(&request)
            .unwrap_or_else(|e| panic!("generating crate root for {}: {e}", package_dir.display()));
    }

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", cargo_package_name])
        .status()
        .expect("invoking cargo build");
    assert!(
        status.success(),
        "cargo build -p {cargo_package_name} must succeed"
    );
}

/// Every workspace directory whose `Cargo.toml` points `[lib] path` at the
/// generated crate root.
fn folder_backed_package_dirs(
    workspace_root: &Path,
) -> std::io::Result<Vec<std::path::PathBuf>> {
    let expected = format!(
        "{}/{}",
        streamlib_processor_extract::crate_root::GENERATED_CRATE_ROOT_DIR_NAME,
        streamlib_processor_extract::crate_root::GENERATED_CRATE_ROOT_FILE_NAME
    );
    let mut out = Vec::new();
    for parent in ["packages", "examples"] {
        collect_manifests(&workspace_root.join(parent), &expected, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn collect_manifests(
    dir: &Path,
    expected_lib_path: &str,
    out: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest = path.join("Cargo.toml");
        if manifest.is_file()
            && std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|body| body.parse::<toml::Value>().ok())
                .and_then(|manifest| {
                    manifest
                        .get("lib")
                        .and_then(|lib| lib.get("path"))
                        .and_then(|p| p.as_str())
                        .map(|p| p.to_string())
                })
                .is_some_and(|declared| declared == expected_lib_path)
        {
            out.push(path.clone());
        }
        collect_manifests(&path, expected_lib_path, out)?;
    }
    Ok(())
}
