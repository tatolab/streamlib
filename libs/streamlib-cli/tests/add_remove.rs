// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end `streamlib add` / `streamlib remove` against a scratch `file://`
//! registry tree, driving the real `streamlib` binary and the real
//! `PolyglotBuildOrchestrator`.
//!
//! This is the local-only integration counterpart to the engine's
//! `core::runtime::add` unit tests (which inject a mock orchestrator). CI runs
//! `cargo test --lib`, so this `tests/` binary is a developer-run gate: it
//! locks the CLI wiring + the real materialize path + the catalog-summary
//! print end-to-end, which the mock-orchestrator unit tests can't.
//!
//! It builds a real source-only schema `.slpkg` (no processors ⇒ the
//! orchestrator's materialize is a cheap re-stage + schema codegen, no
//! Rust/venv build), publishes it into a hand-built tree with a catalog, and
//! shells the `streamlib` binary.

use std::path::Path;
use std::process::Command;

use streamlib_pack::{
    AssembleOptions, AssembleTarget, CargoProfile, PathDepPolicy, assemble_artifact,
};

const BIN: &str = env!("CARGO_BIN_EXE_streamlib");

/// Create a schema-only `foo` package at `dir` (no processors, one owned
/// schema — the smallest publishable package the orchestrator will materialize
/// without a Rust/Python toolchain).
fn write_foo_package(dir: &Path) {
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    std::fs::write(
        dir.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: foo\n  version: 1.1.0\n  \
         description: a demo add package\nschemas:\n  FooFrame:\n    file: schemas/foo_frame.yaml\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("schemas/foo_frame.yaml"),
        "metadata:\n  type: FooFrame\n  description: \"A demo frame\"\nproperties:\n  \
         width:\n    type: uint32\n  height:\n    type: uint32\n",
    )
    .unwrap();
}

/// Assemble `pkg_dir` into a source-only `.slpkg` under the tree, then write the
/// version index + a valid catalog declaring one processor with typed ports.
fn publish_foo_into_tree(pkg_dir: &Path, tree: &Path) {
    let ver_dir = tree.join("slpkg/foo/1.1.0");
    std::fs::create_dir_all(&ver_dir).unwrap();
    assemble_artifact(
        pkg_dir,
        &AssembleTarget::Slpkg(ver_dir.join("foo.slpkg")),
        &AssembleOptions {
            no_build: false,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
        },
        &(),
    )
    .expect("assemble source .slpkg");

    std::fs::write(
        tree.join("slpkg/foo/index.json"),
        "{\"name\":\"foo\",\"vers\":\"1.1.0\"}\n",
    )
    .unwrap();
    // `PackageCatalog.package` is the canonical `@org/name` STRING; a port's
    // concrete schema is the structured `SchemaIdent` map.
    std::fs::write(
        ver_dir.join("foo.catalog.json"),
        r#"{
  "package": "@tatolab/foo",
  "version": "1.1.0",
  "processors": [
    {
      "name": "Foo", "description": "does foo", "runtime": "rust",
      "inputs": [{"name": "video_in", "schema": "any", "read_mode": "skip_to_latest"}],
      "outputs": [{"name": "video_out", "schema": {"org": "tatolab", "package": "foo", "type": "FooFrame", "version": "1.1.0"}}]
    }
  ]
}"#,
    )
    .unwrap();
}

fn run(args: &[&str], registry: &str, home: &Path) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .env("STREAMLIB_REGISTRY_URL", registry)
        .env("STREAMLIB_HOME", home)
        .output()
        .expect("spawn streamlib binary")
}

#[test]
fn add_records_prints_catalog_then_remove_evicts() {
    let tree = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    write_foo_package(pkg.path());
    publish_foo_into_tree(pkg.path(), tree.path());

    let registry = format!("file://{}", tree.path().display());

    // --- add -----------------------------------------------------------
    let out = run(&["add", "@tatolab/foo"], &registry, home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "add failed: status={:?}\nstdout={stdout}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // Catalog-backed discovery summary printed.
    assert!(
        stdout.contains("Added @tatolab/foo v1.1.0"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("Processors (1):"), "stdout: {stdout}");
    assert!(stdout.contains("Foo — does foo"), "stdout: {stdout}");
    assert!(stdout.contains("video_in (any)"), "stdout: {stdout}");
    assert!(
        stdout.contains("video_out (@tatolab/foo/FooFrame@1.1.0)"),
        "stdout: {stdout}"
    );

    // Recorded in packages.yaml + materialized cache slot present.
    let packages_yaml =
        std::fs::read_to_string(home.path().join(".streamlib/packages.yaml")).unwrap();
    assert!(
        packages_yaml.contains("@tatolab/foo"),
        "packages.yaml: {packages_yaml}"
    );
    let slot = home.path().join(".streamlib/cache/packages/foo-1.1.0");
    assert!(
        slot.join("streamlib.yaml").is_file(),
        "cache slot missing manifest"
    );

    // `pkg list` reads packages.yaml offline (no registry) — proves the record
    // is what a later offline consumer resolves against.
    let list = run(&["pkg", "list"], "", home.path());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(list.status.success());
    assert!(list_out.contains("@tatolab/foo"), "pkg list: {list_out}");

    // --- remove --------------------------------------------------------
    let out = run(&["remove", "@tatolab/foo"], &registry, home.path());
    let rstdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "remove failed: {rstdout}");
    assert!(
        rstdout.contains("Removed @tatolab/foo v1.1.0"),
        "stdout: {rstdout}"
    );
    assert!(!slot.exists(), "cache slot must be evicted");
    let packages_yaml =
        std::fs::read_to_string(home.path().join(".streamlib/packages.yaml")).unwrap();
    assert!(
        !packages_yaml.contains("@tatolab/foo"),
        "still recorded: {packages_yaml}"
    );

    // Removing an absent package fails loud.
    let out = run(&["remove", "@tatolab/foo"], &registry, home.path());
    assert!(!out.status.success(), "remove of absent package must fail");
}

#[test]
fn add_unsatisfiable_range_names_available_versions() {
    let tree = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let pkg = tempfile::tempdir().unwrap();
    write_foo_package(pkg.path());
    publish_foo_into_tree(pkg.path(), tree.path());

    let registry = format!("file://{}", tree.path().display());
    let out = run(&["add", "@tatolab/foo@^2.0.0"], &registry, home.path());
    assert!(!out.status.success(), "^2 must not resolve");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The typed RegistryNoMatchingVersion names the available version.
    assert!(
        stderr.contains("1.1.0"),
        "stderr should name available versions: {stderr}"
    );
}
