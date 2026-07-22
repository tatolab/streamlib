// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end `streamlib install` — reproduce a per-app `streamlib_modules/`
//! folder from a committed `streamlib.lock`, driving the real `streamlib`
//! binary.
//!
//! This is the local-only integration counterpart to the
//! `streamlib-idents::app_modules` install unit tests. CI runs `cargo test
//! --lib`, so this `tests/` binary is a developer-run gate: it locks the CLI
//! wiring (`--dir` anchoring, report printing, exit codes) end-to-end over a
//! real `.slpkg` assembled by `streamlib-pack` — the container/CI preinstall
//! story (commit the lock, run `install` at image build).

use std::path::Path;
use std::process::Command;

use streamlib_pack::{
    AssembleOptions, AssembleTarget, CargoProfile, PathDepPolicy, assemble_artifact,
};

const BIN: &str = env!("CARGO_BIN_EXE_streamlib");

/// A schema-only package (no Rust/Python sources — install never builds).
fn write_foo_package(dir: &Path) {
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    std::fs::write(
        dir.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: foo\n  version: 1.1.0\n  \
         description: a demo install package\nschemas:\n  FooFrame:\n    file: schemas/foo_frame.yaml\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("schemas/foo_frame.yaml"),
        "metadata:\n  type: FooFrame\n  description: \"A demo frame\"\nproperties:\n  \
         width:\n    type: uint32\n  height:\n    type: uint32\n",
    )
    .unwrap();
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().expect("spawn streamlib binary")
}

/// The docker/CI story: `add` a portable archive source into a source app,
/// commit its `streamlib.lock`, then reproduce a byte-equivalent
/// `streamlib_modules/` in a CLEAN checkout that carries only the lockfile.
#[test]
fn install_reproduces_modules_in_a_clean_checkout_from_the_lock() {
    let pkg = tempfile::tempdir().unwrap();
    write_foo_package(pkg.path());

    // A real source-only `.slpkg` (a portable, machine-independent source).
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

    // Source app: `add` records the archive source in streamlib.lock.
    let source_app = tempfile::tempdir().unwrap();
    let src_dir = source_app.path().to_str().unwrap();
    let out = run(&["add", slpkg.to_str().unwrap(), "--dir", src_dir]);
    assert!(
        out.status.success(),
        "add failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let source_manifest = std::fs::read(
        source_app
            .path()
            .join("streamlib_modules/@tatolab/foo/streamlib.yaml"),
    )
    .unwrap();

    // Clean checkout: ONLY the lockfile is present.
    let dest_app = tempfile::tempdir().unwrap();
    let dest_dir = dest_app.path().to_str().unwrap();
    std::fs::copy(
        source_app.path().join("streamlib.lock"),
        dest_app.path().join("streamlib.lock"),
    )
    .unwrap();
    assert!(
        !dest_app.path().join("streamlib_modules").exists(),
        "precondition: no modules dir in the clean checkout"
    );

    // Reproduce.
    let out = run(&["install", "--dir", dest_dir]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "install failed: {stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Reproduced 1 package(s)"), "stdout: {stdout}");
    assert!(stdout.contains("@tatolab/foo"), "stdout: {stdout}");
    assert!(stdout.contains("materialized"), "stdout: {stdout}");

    // The reproduced slot is byte-identical to the source app's.
    let dest_slot = dest_app
        .path()
        .join("streamlib_modules/@tatolab/foo/streamlib.yaml");
    assert!(dest_slot.is_file(), "reproduced slot missing manifest");
    assert_eq!(
        std::fs::read(&dest_slot).unwrap(),
        source_manifest,
        "reproduced manifest must be byte-identical"
    );

    // Install is idempotent (second run reproduces the same folder).
    let out = run(&["install", "--dir", dest_dir]);
    assert!(out.status.success(), "second install must succeed");
    assert_eq!(
        std::fs::read(&dest_slot).unwrap(),
        source_manifest,
        "re-install must stay byte-identical"
    );
}

/// The `delete streamlib_modules/, keep streamlib.lock, install` recovery loop.
#[test]
fn install_recovers_deleted_modules_dir_from_the_lock() {
    let pkg = tempfile::tempdir().unwrap();
    write_foo_package(pkg.path());
    let app = tempfile::tempdir().unwrap();
    let dir = app.path().to_str().unwrap();

    // A folder `add` (Path source) into the app.
    let out = run(&["add", pkg.path().to_str().unwrap(), "--dir", dir]);
    assert!(out.status.success(), "add failed");
    let slot = app.path().join("streamlib_modules/@tatolab/foo/streamlib.yaml");
    let before = std::fs::read(&slot).unwrap();

    // Nuke the modules folder, keep the lock.
    std::fs::remove_dir_all(app.path().join("streamlib_modules")).unwrap();
    assert!(app.path().join("streamlib.lock").exists());

    // Install reproduces it.
    let out = run(&["install", "--dir", dir]);
    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(slot.is_file(), "modules dir must be reproduced");
    assert_eq!(std::fs::read(&slot).unwrap(), before);
}

#[test]
fn install_without_a_lockfile_fails_loud() {
    let app = tempfile::tempdir().unwrap();
    let out = run(&["install", "--dir", app.path().to_str().unwrap()]);
    assert!(!out.status.success(), "install with no lock must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no streamlib.lock to install from"),
        "stderr: {stderr}"
    );
}
