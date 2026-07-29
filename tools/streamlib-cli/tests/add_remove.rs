// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end `streamlib add` / `streamlib remove` against the per-app
//! `streamlib_modules/` folder, driving the real `streamlib` binary.
//!
//! This is the local-only integration counterpart to the
//! `streamlib-idents::app_modules` unit tests. CI runs `cargo test --lib`,
//! so this `tests/` binary is a developer-run gate: it locks the CLI wiring
//! (arg parsing, `--dir` anchoring, report printing) end-to-end over a real
//! `.slpkg` assembled by `streamlib-pack`.

mod cli_integration_support;

use cli_integration_support::{run_streamlib_binary, write_single_processor_foo_package_source};

use streamlib_pack::{
    AssembleOptions, AssembleTarget, CargoProfile, PathDepPolicy, assemble_artifact,
};

#[test]
fn add_folder_and_slpkg_then_remove_via_app_modules() {
    let pkg = tempfile::tempdir().unwrap();
    write_single_processor_foo_package_source(pkg.path(), "a demo add package");
    let app_root = tempfile::tempdir().unwrap();
    let app_dir = app_root.path().to_str().unwrap();

    // --- add (folder source) -------------------------------------------
    let out = run_streamlib_binary(&["add", pkg.path().to_str().unwrap(), "--dir", app_dir]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "folder add failed: status={:?}\nstdout={stdout}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Added @tatolab/foo v1.1.0"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("Processors (1):"), "stdout: {stdout}");
    assert!(stdout.contains("Foo — does foo"), "stdout: {stdout}");

    let slot = app_root.path().join("streamlib_modules/@tatolab/foo");
    assert!(
        slot.join("streamlib.yaml").is_file(),
        "modules slot missing"
    );
    let lock = std::fs::read_to_string(app_root.path().join("streamlib.lock")).unwrap();
    assert!(lock.contains("@tatolab/foo"), "lock: {lock}");
    assert!(lock.contains("kind: path"), "lock: {lock}");

    // --- re-add via a real `.slpkg` (built by streamlib-pack) -----------
    let artifacts = tempfile::tempdir().unwrap();
    let slpkg = artifacts.path().join("foo.slpkg");
    assemble_artifact(
        pkg.path(),
        &AssembleTarget::Slpkg(slpkg.clone()),
        &AssembleOptions {
            no_build: false,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
            ignore_in_tree_prebuilt_cdylib: false,
        },
        &(),
    )
    .expect("assemble source .slpkg");

    let out = run_streamlib_binary(&["add", slpkg.to_str().unwrap(), "--dir", app_dir]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "slpkg add failed: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Replaced @tatolab/foo v1.1.0"),
        "re-add must replace: {stdout}"
    );
    let lock = std::fs::read_to_string(app_root.path().join("streamlib.lock")).unwrap();
    assert!(lock.contains("kind: archive"), "lock: {lock}");
    assert!(lock.contains("archive_sha256"), "lock: {lock}");
    assert_eq!(
        lock.matches("@tatolab/foo").count(),
        1,
        "one lock entry after re-add: {lock}"
    );

    // --- remove ---------------------------------------------------------
    let out = run_streamlib_binary(&["remove", "@tatolab/foo", "--dir", app_dir]);
    let rstdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "remove failed: {rstdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        rstdout.contains("Removed @tatolab/foo v1.1.0"),
        "stdout: {rstdout}"
    );
    assert!(!slot.exists(), "modules slot must be gone");
    let lock = std::fs::read_to_string(app_root.path().join("streamlib.lock")).unwrap();
    assert!(!lock.contains("@tatolab/foo"), "still locked: {lock}");

    // Removing an absent package fails loud.
    let out = run_streamlib_binary(&["remove", "@tatolab/foo", "--dir", app_dir]);
    assert!(!out.status.success(), "remove of absent package must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not installed"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn add_folder_with_expect_sha256_warns_on_stderr_but_succeeds() {
    let pkg = tempfile::tempdir().unwrap();
    write_single_processor_foo_package_source(pkg.path(), "a demo add package");
    let app_root = tempfile::tempdir().unwrap();

    let out = run_streamlib_binary(&[
        "add",
        pkg.path().to_str().unwrap(),
        "--dir",
        app_root.path().to_str().unwrap(),
        "--expect-sha256",
        &"ab".repeat(32),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // A folder source has no archive bytes; the sha pin is a no-op and must be
    // called out on stderr rather than silently ignored, but the add succeeds.
    assert!(
        out.status.success(),
        "folder add must still succeed: {stderr}"
    );
    assert!(
        stderr.contains("--expect-sha256 is ignored for a folder source"),
        "expected a stderr warning, got: {stderr}"
    );
    assert!(
        stdout.contains("Added @tatolab/foo v1.1.0"),
        "stdout: {stdout}"
    );
}

#[test]
fn add_version_coordinate_gets_guidance_error() {
    let app_root = tempfile::tempdir().unwrap();
    let out = run_streamlib_binary(&[
        "add",
        "@tatolab/foo",
        "--dir",
        app_root.path().to_str().unwrap(),
    ]);
    assert!(
        !out.status.success(),
        "a version coordinate must be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("version coordinate"),
        "guidance missing: {stderr}"
    );
    // Nothing materialized.
    assert!(!app_root.path().join("streamlib_modules").exists());
    assert!(!app_root.path().join("streamlib.lock").exists());
}

#[test]
fn add_with_expect_sha256_mismatch_fails_with_no_partial_state() {
    let pkg = tempfile::tempdir().unwrap();
    write_single_processor_foo_package_source(pkg.path(), "a demo add package");
    let artifacts = tempfile::tempdir().unwrap();
    let slpkg = artifacts.path().join("foo.slpkg");
    assemble_artifact(
        pkg.path(),
        &AssembleTarget::Slpkg(slpkg.clone()),
        &AssembleOptions {
            no_build: false,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
            ignore_in_tree_prebuilt_cdylib: false,
        },
        &(),
    )
    .expect("assemble source .slpkg");

    let app_root = tempfile::tempdir().unwrap();
    let out = run_streamlib_binary(&[
        "add",
        slpkg.to_str().unwrap(),
        "--dir",
        app_root.path().to_str().unwrap(),
        "--expect-sha256",
        &"00".repeat(32),
    ]);
    assert!(!out.status.success(), "sha mismatch must fail the add");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sha256 mismatch"), "stderr: {stderr}");
    // No package dir, no staging residue, no lockfile.
    let modules = app_root.path().join("streamlib_modules");
    if modules.is_dir() {
        let entries: Vec<_> = std::fs::read_dir(&modules)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.is_empty(), "residue: {entries:?}");
    }
    assert!(!app_root.path().join("streamlib.lock").exists());
}
