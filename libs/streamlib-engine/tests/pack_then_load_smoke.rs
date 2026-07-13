// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end gate for the `streamlib pkg build` →
//! `Runner::add_module_with(_, Strategy::Slpkg)`
//! chain: pack a real workspace package into a source-only `.slpkg`
//! (the distribution shape — no prebuilt cdylib), hand the bundle
//! back to a fresh `Runner`, and assert the loaded artifacts
//! (processors for Rust-impl packages, schemas for the canonical
//! schemas-only package) land in the runtime registries.
//!
//! The schemas-only leg is the file-based gate that runs in CI: it
//! packs + loads a `.slpkg` with no cdylib and no cargo build, so it
//! needs no by-version SDK resolution. The Rust-impl leg exercises the
//! FULL consumer story — the loader builds the bundled source on this
//! host via the build orchestrator, resolving the SDK crate chain by
//! version — but is `#[ignore]`d until an out-of-tree by-version SDK
//! source exists again (the custom cargo registry was removed in #1322;
//! see the leg's `#[ignore]` reason). Rust-cdylib load coverage in the
//! meantime lives in `load_project_dylib_*` (in-workspace fixture,
//! dlopen).
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
//! cache. This test therefore writes to the *real* network / core
//! cache entries (at their current package versions) on the host
//! running the test. The
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
use streamlib::sdk::RunnerAutoBuild as _;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::runtime::{Runner, Strategy};
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

/// Run `streamlib pkg build --output <slpkg>` inside `pkg_dir` — the
/// current CLI pack verb (source-only `.slpkg`, run inside the package).
fn pkg_build_slpkg(cli: &Path, pkg_dir: &Path, slpkg: &Path) {
    let status = Command::new(cli)
        .args(["pkg", "build", "--output"])
        .arg(slpkg)
        .current_dir(pkg_dir)
        .status()
        .expect("invoking `streamlib pkg build`");
    assert!(
        status.success(),
        "`streamlib pkg build` in {} must succeed",
        pkg_dir.display()
    );
    assert!(
        slpkg.exists(),
        "pkg build must have written {}",
        slpkg.display()
    );
}

#[test]
#[serial]
#[ignore = "out-of-tree Rust `.slpkg` build needs the SDK by version, which the \
            custom cargo registry used to serve (removed in #1322); SDK publish to \
            real registries is deferred (#1323) and this leg's rework onto \
            `streamlib link --engine` is tracked by #1338. Rust-cdylib load coverage \
            meanwhile lives in the `load_project_dylib_*` dlopen integration tests, \
            which build the fixture in-workspace and exercise the full \
            STREAMLIB_PLUGIN → setup/process/teardown roundtrip."]
fn pack_then_load_rust_package_registers_processors() {
    // Rust-impl gate: `pkg build` packs `@tatolab/network` (small dep
    // graph, no GPU / audio hardware) into a source-only `.slpkg`
    // straight from its package dir — packages/* are standalone
    // workspaces, NOT members of the repo workspace, so there is no
    // `cargo -p streamlib-network` to lean on. Loading the source-only
    // box makes the orchestrator build it on this host, resolving
    // streamlib-plugin-sdk & friends by version — the real consumer
    // story. Both exported processors must land in `PROCESSOR_REGISTRY`
    // after `Runner::add_module_with`.
    //
    // IGNORED (see the `#[ignore]` reason above): with the custom cargo
    // registry gone and SDK publishing deferred, the out-of-tree build
    // has no by-version SDK source. Re-enable when #1323 (real-registry
    // publish) or #1338 (`streamlib link --engine` rework) lands.
    let cli = build_streamlib_cli();

    let tmp = tempfile::tempdir().unwrap();
    let pkg_src = workspace_root().join("packages").join("network");
    let slpkg = tmp.path().join("network.slpkg");
    pkg_build_slpkg(&cli, &pkg_src, &slpkg);

    // A source-only Rust slpkg builds at load — the Runner needs the
    // polyglot build orchestrator wired (`Runner::new()` is
    // orchestrator-free by design and would fail with
    // BuildRequiredButNoOrchestrator).
    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "network"),
            Strategy::Slpkg {
                path: slpkg.clone(),
            },
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
    let slpkg = tmp.path().join("core.slpkg");
    pkg_build_slpkg(&cli, &pkg_src, &slpkg);

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "core"),
            Strategy::Slpkg {
                path: slpkg.clone(),
            },
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
            Strategy::Slpkg {
                path: slpkg.clone(),
            },
        )
        .expect_err("add_module_with SlpkgArchive against a foreign-triple-only slpkg must error");
    let msg = format!("{err}");
    let host = host_target_triple();
    assert!(
        msg.contains(host) || msg.contains(foreign_triple) || msg.to_lowercase().contains("triple"),
        "error must surface the triple mismatch (host: {host}, foreign: {foreign_triple}), got: {msg}"
    );
}
