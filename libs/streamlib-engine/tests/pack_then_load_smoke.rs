// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end gate for the `streamlib pack` →
//! `Runner::add_module_with(_, Strategy::Slpkg)`
//! chain: pack a real workspace package into a `.slpkg`, hand the
//! bundle back to a fresh `Runner`, and assert the loaded artifacts
//! (processors for Rust-impl packages, schemas for the canonical
//! schemas-only package) land in the runtime registries.
//!
//! Mentally-revert lock summary (each maps to a specific test):
//! - Drop *any one* processor from `streamlib-network`'s
//!   `export_plugin!` arg list: dlopen still succeeds and the other
//!   processor still registers, so `add_module_with(SlpkgArchive)`
//!   returns `Ok` — but the listed-name assertion that *both*
//!   `UdpSource` and `UdpSink` appear in `PROCESSOR_REGISTRY` fails.
//!   (Dropping the whole `export_plugin!` macro is caught earlier —
//!   by the `STREAMLIB_PLUGIN missing` error path inside the module
//!   loader — the listed-name assertion locks the omit-one regression
//!   class the symbol-missing error path doesn't cover.)
//! - Drop the `schemas:` walk (`register_package_schemas`) in the
//!   module loader and the schemas-only assertion fails
//!   (`current_schema_definition("@tatolab/core/VideoFrame")`
//!   returns `None`).
//! - Drop the `current_image_layout` / wrong-triple error chain in
//!   the module loader's Rust-runtime branch and the foreign-triple
//!   negative test sees a panic or a non-actionable error message
//!   instead of the triple-naming `Configuration` error it asserts
//!   against.
//!
//! Cache scope: the `SlpkgArchive` strategy extracts every slpkg into
//! the process-global `<STREAMLIB_HOME>/.streamlib/cache/packages/<name>-<version>/`
//! cache. This test therefore writes to the *real* `network-1.0.0`
//! and `core-1.0.0` cache entries on the host running the test. The
//! extract is idempotent (`extract_slpkg_to_cache` clears the dir
//! before re-extracting) and the package names match the real
//! workspace packages, so a fresh extract reproduces the same
//! contents — accepted as low-risk. CI runners are fresh per job; on
//! developer machines the entries are rebuilt every time the test
//! runs against the current workspace tree.
//!
//! Companion to `load_project_dylib_smoke.rs`, which exercises the
//! same dlopen path but feeds `add_module_with(ManifestDirectory)`
//! against a staged directory rather than the full pack → extract →
//! load round-trip.

use std::path::{Path, PathBuf};
use std::process::Command;

use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::{Strategy, Runner};
use streamlib_engine::core::runtime::host_target_triple;
use streamlib_engine::schemas::current_schema_definition;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn host_dylib_filename(crate_lib_name: &str) -> String {
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    format!("lib{}.{}", crate_lib_name, ext)
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_entry = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &dst_entry);
        } else {
            std::fs::copy(entry.path(), &dst_entry).unwrap();
        }
    }
}

fn build_streamlib_cli() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "--bin", "streamlib", "-p", "streamlib-cli"])
        .status()
        .expect("invoking cargo build for streamlib-cli");
    assert!(
        status.success(),
        "cargo build --bin streamlib -p streamlib-cli must succeed"
    );

    let bin = workspace_root()
        .join("target")
        .join("debug")
        .join(if cfg!(windows) {
            "streamlib.exe"
        } else {
            "streamlib"
        });
    assert!(
        bin.exists(),
        "streamlib binary expected at {} after build",
        bin.display()
    );
    bin
}

fn cargo_build_dylib(crate_name: &str) -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", crate_name])
        .status()
        .unwrap_or_else(|e| panic!("invoking cargo build -p {}: {}", crate_name, e));
    assert!(
        status.success(),
        "cargo build -p {} must succeed",
        crate_name
    );

    let lib_name = crate_name.replace('-', "_");
    let dylib = workspace_root()
        .join("target")
        .join("debug")
        .join(host_dylib_filename(&lib_name));
    assert!(
        dylib.exists(),
        "expected cdylib at {} after `cargo build -p {}`",
        dylib.display(),
        crate_name
    );
    dylib
}

/// Stage a workspace package into a tempdir with a prebuilt dylib so
/// `streamlib pack --no-build` can bundle it without forcing a release
/// build inside the test (the workspace's debug-mode artifact already
/// satisfies the per-triple `lib/<triple>/<filename>` contract pack
/// looks for).
fn stage_package_with_prebuilt_dylib(
    pkg_src: &Path,
    staging: &Path,
    crate_lib_name: &str,
    dylib: &Path,
) {
    std::fs::create_dir_all(staging).unwrap();
    std::fs::copy(
        pkg_src.join("streamlib.yaml"),
        staging.join("streamlib.yaml"),
    )
    .unwrap();
    let schemas_src = pkg_src.join("schemas");
    if schemas_src.is_dir() {
        copy_dir_recursive(&schemas_src, &staging.join("schemas"));
    }
    let triple_dir = staging.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(dylib, triple_dir.join(host_dylib_filename(crate_lib_name))).unwrap();
}

#[test]
#[serial]
fn pack_then_load_rust_package_registers_processors() {
    // Rust-impl gate: pack `@tatolab/network` (small dep graph, no GPU
    // / audio hardware, no transitive `@tatolab/core` dep so the
    // pack-then-load chain stands alone without an installed-package
    // cache prime) and assert both exported processors land in
    // `PROCESSOR_REGISTRY` after `Runner::add_module_with`.
    let cli = build_streamlib_cli();
    let dylib = cargo_build_dylib("streamlib-network");

    let tmp = tempfile::tempdir().unwrap();
    let pkg_src = workspace_root().join("packages").join("network");
    let staging = tmp.path().join("network-pkg");
    stage_package_with_prebuilt_dylib(&pkg_src, &staging, "streamlib_network", &dylib);

    let slpkg = tmp.path().join("network.slpkg");
    let status = Command::new(&cli)
        .args([
            "pack",
            staging.to_str().unwrap(),
            "--no-build",
            "-o",
            slpkg.to_str().unwrap(),
        ])
        .status()
        .expect("invoking `streamlib pack`");
    assert!(
        status.success(),
        "`streamlib pack` against staged network package must succeed"
    );
    assert!(slpkg.exists(), "pack must have written {}", slpkg.display());

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "network"),
            Strategy::Slpkg { path: slpkg.clone() },
        )
        .expect("add_module_with SlpkgArchive against a freshly-packed Rust slpkg must succeed");

    let registered: Vec<String> = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .map(|desc| desc.name.r#type.as_str().to_string())
        .collect();
    assert!(
        registered.iter().any(|n| n == "UdpSource"),
        "UdpSource must be registered after add_module_with, got: {:?}",
        registered
    );
    assert!(
        registered.iter().any(|n| n == "UdpSink"),
        "UdpSink must be registered after add_module_with, got: {:?}",
        registered
    );
}

#[test]
#[serial]
fn pack_then_load_schemas_only_package_registers_schemas() {
    // Schemas-only gate: pack `@tatolab/core` (the canonical
    // schemas-only package — zero processors, four wire-stable types)
    // and assert at least one canonical schema lands in the runtime
    // schema registry after `Runner::add_module_with`. Exercises the
    // no-cdylib branch of pack (no `lib/` in the archive, no dlopen
    // at load time) plus the `schemas:` walk inside `add_module_with`.
    let cli = build_streamlib_cli();

    let tmp = tempfile::tempdir().unwrap();
    let pkg_src = workspace_root().join("packages").join("core");
    let staging = tmp.path().join("core-pkg");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::copy(
        pkg_src.join("streamlib.yaml"),
        staging.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_recursive(&pkg_src.join("schemas"), &staging.join("schemas"));

    let slpkg = tmp.path().join("core.slpkg");
    let status = Command::new(&cli)
        .args([
            "pack",
            staging.to_str().unwrap(),
            "--no-build",
            "-o",
            slpkg.to_str().unwrap(),
        ])
        .status()
        .expect("invoking `streamlib pack`");
    assert!(
        status.success(),
        "`streamlib pack` against the schemas-only core package must succeed"
    );
    assert!(slpkg.exists());

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "core"),
            Strategy::Slpkg { path: slpkg.clone() },
        )
        .expect("add_module_with SlpkgArchive against a schemas-only slpkg must succeed");

    assert!(
        current_schema_definition("@tatolab/core/VideoFrame").is_some(),
        "@tatolab/core/VideoFrame must be registered after add_module_with"
    );
    assert!(
        current_schema_definition("@tatolab/core/AudioFrame").is_some(),
        "@tatolab/core/AudioFrame must be registered after add_module_with"
    );
}

#[test]
#[serial]
fn add_module_with_slpkg_archive_missing_file_errors_cleanly() {
    // Negative-path gate: add_module_with against a path that doesn't
    // exist must surface a `Configuration` error naming the missing
    // path rather than panicking, returning silently, or
    // deadlocking. Mirrors the actionable-error contract pack /
    // add_module_with(ManifestDirectory) holds for misconfigured manifests.
    let runtime = Runner::new().unwrap();
    let missing = std::path::PathBuf::from("/nonexistent/path/missing.slpkg");
    let err = runtime
        .add_module_with_blocking(
            // Any ident — the strategy resolver errors during extraction
            // before the ident is checked against the (non-existent)
            // archive's manifest.
            module_ident_any_version!("tatolab", "core"),
            Strategy::Slpkg {
                path: missing.clone(),
            },
        )
        .expect_err("add_module_with SlpkgArchive against a missing .slpkg must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("missing.slpkg") || msg.to_lowercase().contains("no such file"),
        "error must surface the missing path / OS error, got: {msg}"
    );
}

#[test]
#[serial]
fn add_module_with_slpkg_archive_wrong_triple_errors_with_actionable_message() {
    // Negative-path gate: a `.slpkg` whose `lib/` directory carries
    // only a foreign-triple subdir (no `lib/<host_triple>/...` for
    // this machine) must fail with an actionable error naming the
    // available triples and the host's expected triple. Catches the
    // class of regressions where the loader silently picks up the
    // wrong artifact or falls back to a legacy flat `lib/<file>`
    // layout instead of erroring at first miss.
    let tmp = tempfile::tempdir().unwrap();
    let slpkg = tmp.path().join("foreign.slpkg");

    let manifest = r#"
package:
  org: tatolab
  name: foreign-triple-fixture
  version: 0.1.0
processors:
  - name: ForeignTripleStub
    version: 1.0.0
    description: "stub"
    execution: manual
    runtime:
      language: rust
    inputs: []
    outputs: []
"#;
    // Pick a triple that's definitely not the host's. The host triple
    // is `x86_64-unknown-linux-gnu` / `aarch64-apple-darwin` / etc.;
    // an obviously-foreign sentinel triple guarantees the loader's
    // host-match returns empty regardless of which platform runs the
    // test.
    let foreign_triple = if host_target_triple().contains("linux") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let dylib_entry = format!("lib/{}/libforeign_stub.so", foreign_triple);

    let file = std::fs::File::create(&slpkg).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::<()>::default()
        .compression_method(zip::CompressionMethod::Deflated);

    use std::io::Write as _;
    zip.start_file("streamlib.yaml", opts).unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file(&dylib_entry, opts).unwrap();
    zip.write_all(b"not-a-real-dylib").unwrap();
    zip.finish().unwrap();

    let runtime = Runner::new().unwrap();
    let err = runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "foreign-triple-fixture"),
            Strategy::Slpkg { path: slpkg.clone() },
        )
        .expect_err("add_module_with SlpkgArchive against a foreign-triple-only slpkg must error");
    let msg = format!("{err}");
    let host = host_target_triple();
    assert!(
        msg.contains(host) || msg.contains(foreign_triple) || msg.to_lowercase().contains("triple"),
        "error must surface the triple mismatch (host: {host}, foreign: {foreign_triple}), got: {msg}"
    );
}
