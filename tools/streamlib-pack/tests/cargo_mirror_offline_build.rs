// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end validation of the cargo source-replacement mirror: a package that
//! declares the engine/SDK crates by bare version — with **no** `streamlib
//! link` and **no** `[patch.crates-io]` — resolves and builds entirely offline
//! from the emitted tree, the way the separate-build `.slpkg` validation gate
//! builds a package by version.
//!
//! `#[ignore]` because it runs the full mirror emit: `cargo vendor` of the whole
//! workspace closure (network permitted on the emit machine) plus a `cargo
//! package` of every engine/SDK release-closure crate. Run explicitly:
//!
//! ```text
//! cargo test -p streamlib-pack --test cargo_mirror_offline_build -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_pack::cargo_mirror::{
    CARGO_MIRROR_SUBDIR, SOURCE_REPLACEMENT_CONFIG_FILE, VENDOR_SUBDIR, emit_cargo_mirror,
};
use streamlib_pack::compute_release_closure;

/// The workspace root (two levels up from this crate's manifest dir).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Write a minimal standalone consumer crate at `dir` depending on `dep_line`
/// (e.g. `streamlib = "0.6.0"`). The empty `[workspace]` keeps it out of any
/// ambient workspace; `main.rs` references nothing so metadata/build test only
/// resolution + the dep's own compile.
fn write_consumer(dir: &Path, name: &str, dep_line: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n{dep_line}\n\n[workspace]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    // Guard: the consumer must carry NO link marker and NO patch — it resolves
    // purely by version from the mirror, like a real downstream consumer.
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("[patch"),
        "consumer must not carry a [patch.*] block"
    );
    assert!(
        !dir.join("streamlib.link.json").exists(),
        "consumer must not be linked"
    );
}

/// Install the generated `[source]` replacement config into the consumer's
/// `.cargo/config.toml`.
fn install_source_config(consumer: &Path, mirror_config: &Path) {
    std::fs::create_dir_all(consumer.join(".cargo")).unwrap();
    std::fs::copy(mirror_config, consumer.join(".cargo/config.toml")).unwrap();
}

fn run_cargo(consumer: &Path, args: &[&str]) -> std::process::Output {
    Command::new("cargo")
        .args(args)
        .current_dir(consumer)
        // Isolate from any ambient CARGO_* the harness set.
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("spawn cargo")
}

#[test]
#[ignore = "runs a full cargo vendor + package of the engine/SDK chain; run with --ignored"]
fn link_free_package_resolves_and_builds_offline_from_the_mirror() {
    let ws = workspace_root();
    let closure = compute_release_closure(&ws).expect("compute release closure");
    assert!(
        closure.crates.iter().any(|c| c.name == "streamlib"),
        "the release closure must carry the `streamlib` SDK crate"
    );

    // Emit the mirror. staging == out so the generated config's `directory`
    // (rooted at `out`) points at the real emitted vendor tree.
    let tree = tempfile::tempdir().expect("mirror tree tempdir");
    let tree_root = tree.path();
    emit_cargo_mirror(&ws, tree_root, tree_root, &closure).expect("emit cargo mirror");

    let mirror_config = tree_root
        .join(CARGO_MIRROR_SUBDIR)
        .join(SOURCE_REPLACEMENT_CONFIG_FILE);
    let vendor_dir = tree_root.join(CARGO_MIRROR_SUBDIR).join(VENDOR_SUBDIR);
    assert!(mirror_config.is_file(), "generated [source] config missing");
    assert!(
        vendor_dir
            .join("streamlib")
            .join(".cargo-checksum.json")
            .is_file(),
        "the `streamlib` SDK crate must be injected as a directory-source entry"
    );

    // ---- NEGATIVE CONTROL: `--offline` alone is insufficient ----
    // `streamlib-idents` is not published on any real crates.io, so an offline
    // resolve WITHOUT the [source] replacement cannot find it and MUST fail.
    // (The `streamlib` name itself is squatted on crates.io by an unrelated
    // crate, so it would resolve to the WRONG crate — see the squatter-shadow
    // assertion below — which is why the control uses an unambiguous name.)
    let neg = tempfile::tempdir().unwrap();
    write_consumer(neg.path(), "neg-consumer", "streamlib-idents = \"0.6.0\"");
    let neg_out = run_cargo(neg.path(), &["generate-lockfile", "--offline"]);
    assert!(
        !neg_out.status.success(),
        "negative control MUST fail: offline resolve of `streamlib-idents = 0.6.0` without the \
         [source] replacement cannot find the engine crate.\nstderr:\n{}",
        String::from_utf8_lossy(&neg_out.stderr)
    );

    // ---- RESOLVE the full engine/SDK chain offline via the mirror ----
    let resolver = tempfile::tempdir().unwrap();
    write_consumer(resolver.path(), "resolve-consumer", "streamlib = \"0.6.0\"");
    install_source_config(resolver.path(), &mirror_config);

    let lockgen = run_cargo(resolver.path(), &["generate-lockfile", "--offline"]);
    assert!(
        lockgen.status.success(),
        "generate-lockfile --offline against the mirror must succeed.\nstderr:\n{}",
        String::from_utf8_lossy(&lockgen.stderr)
    );

    let lock = std::fs::read_to_string(resolver.path().join("Cargo.lock")).unwrap();
    let streamlib_block = lock
        .split("[[package]]")
        .find(|b| b.contains("name = \"streamlib\"\n"))
        .expect("consumer lock must list the streamlib crate");
    // The mirror shadows the crates.io squatter: `streamlib = ^0.6.0` resolves
    // to the ENGINE crate at 0.6.0 (the only version in the mirror), not the
    // unrelated crates.io `streamlib` (0.6.4 at time of writing). Proven two
    // ways: the resolved version is exactly ours, and the engine's private
    // `streamlib-engine` dep is pulled in (the squatter has no such dep).
    assert!(
        streamlib_block.contains("version = \"0.6.0\"\n"),
        "mirror must resolve `streamlib` to the ENGINE 0.6.0, not the crates.io squatter:\n{streamlib_block}"
    );
    assert!(
        lock.contains("name = \"streamlib-engine\"\n"),
        "the resolved `streamlib` must be the engine SDK crate (pulls streamlib-engine), \
         proving the mirror shadowed the crates.io squatter"
    );
    // The lock records the CANONICAL crates.io source id (source *replacement*
    // preserves it) so `--locked` stays clean.
    assert!(
        streamlib_block.contains("registry+https://github.com/rust-lang/crates.io-index"),
        "the streamlib lock entry must preserve the canonical crates.io source id:\n{streamlib_block}"
    );

    let metadata = run_cargo(
        resolver.path(),
        &["metadata", "--locked", "--offline", "--format-version", "1"],
    );
    assert!(
        metadata.status.success(),
        "`cargo metadata --locked --offline` must resolve the whole engine/SDK chain from the \
         mirror.\nstderr:\n{}",
        String::from_utf8_lossy(&metadata.stderr)
    );

    // ---- COMPILE a real engine crate offline from the mirror ----
    // Building the full `streamlib` SDK pulls the entire engine (Vulkan / system
    // libs) — out of scope for a resolution test — so compile a pure-Rust leaf
    // engine crate to prove genuine offline compilation of engine sources +
    // their vendored transitive deps.
    let builder = tempfile::tempdir().unwrap();
    write_consumer(
        builder.path(),
        "build-consumer",
        "streamlib-idents = \"0.6.0\"",
    );
    install_source_config(builder.path(), &mirror_config);
    let build_lockgen = run_cargo(builder.path(), &["generate-lockfile", "--offline"]);
    assert!(
        build_lockgen.status.success(),
        "lockgen: {}",
        String::from_utf8_lossy(&build_lockgen.stderr)
    );
    let build = run_cargo(builder.path(), &["build", "--locked", "--offline"]);
    assert!(
        build.status.success(),
        "`cargo build --locked --offline` of `streamlib-idents = 0.6.0` must compile from the \
         mirror.\nstderr:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
}
