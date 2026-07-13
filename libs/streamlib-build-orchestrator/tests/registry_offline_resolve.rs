// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration: a consumer resolves cargo **offline, with no server** via the
//! stanza `streamlib registry use` emits. Reshapes a local sparse tree into a
//! serverless cargo `local-registry`, writes the `[source]` replacement into
//! the consumer's `.cargo/config.toml`, then `cargo generate-lockfile
//! --offline` resolves a `registry = "tatolab"` dep with zero manual config —
//! and the lockfile keeps the canonical source id (the #1245 no-localhost-poison
//! invariant).
//!
//! Self-contained: builds a trivial synthetic crate into a hand-made sparse
//! tree, so it needs neither the vulkanalia fork nor the network. Skips (does
//! not fail) when `cargo` / `bash` / `tar` / `sha256sum` are unavailable so the
//! suite stays green on a minimal box.

use std::path::Path;
use std::process::Command;

use streamlib_build_orchestrator::registry::{self, CargoReplacementSource, UseRegistryOptions};

fn tool_on_path(bin: &str, version_flag: &str) -> bool {
    Command::new(bin)
        .arg(version_flag)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// sha256 hex of a file, via `sha256sum` (avoids pulling a hashing crate into
/// dev-deps just for this test).
fn sha256_hex(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    assert!(out.status.success(), "sha256sum failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// Build a minimal sparse cargo tree under `tree` holding one no-dep crate
/// `probecrate` v0.1.0 — the exact on-disk shape the reshape script consumes:
/// `cargo/config.json`, a sharded index line carrying `cksum`, and a nested
/// `.crate` tarball.
fn build_sparse_probecrate_tree(tree: &Path) {
    let cargo = tree.join("cargo");
    // 1. Stage the crate source and pack it as `<name>-<ver>.crate` (gzip tar
    //    with the conventional `<name>-<ver>/` prefix).
    let staging = tree.join("_staging");
    let pkg = staging.join("probecrate-0.1.0");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("Cargo.toml"),
        "[package]\nname = \"probecrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         description = \"probe\"\nlicense = \"MIT\"\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src").join("lib.rs"), "// probe\n").unwrap();

    let crate_dir = cargo.join("crates").join("probecrate");
    std::fs::create_dir_all(&crate_dir).unwrap();
    let crate_file = crate_dir.join("probecrate-0.1.0.crate");
    let tar_ok = Command::new("tar")
        .arg("czf")
        .arg(&crate_file)
        .arg("-C")
        .arg(&staging)
        .arg("probecrate-0.1.0")
        .status()
        .unwrap()
        .success();
    assert!(tar_ok, "tar pack failed");

    // 2. Sparse index line (name len >= 4 → `<c1c2>/<c3c4>/<name>` shard).
    let cksum = sha256_hex(&crate_file);
    let shard = cargo.join("pr").join("ob");
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("probecrate"),
        format!(
            "{{\"name\":\"probecrate\",\"vers\":\"0.1.0\",\"deps\":[],\"cksum\":\"{cksum}\",\"features\":{{}},\"yanked\":false}}\n"
        ),
    )
    .unwrap();

    // 3. config.json (present in a real tree; the reshape drops it).
    std::fs::write(
        cargo.join("config.json"),
        "{\"dl\":\"http://example.invalid/cargo/crates/{crate}/{crate}-{version}.crate\",\"api\":\"http://example.invalid/cargo\"}\n",
    )
    .unwrap();
}

#[test]
fn consumer_resolves_offline_via_emitted_stanza() {
    for (bin, flag) in [
        ("cargo", "--version"),
        ("bash", "--version"),
        ("tar", "--version"),
        ("sha256sum", "--version"),
    ] {
        if !tool_on_path(bin, flag) {
            eprintln!("skipping: `{bin}` not on PATH");
            return;
        }
    }
    let scripts_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/registry");
    if !scripts_dir.join("emit-cargo-local-registry.sh").is_file() {
        eprintln!(
            "skipping: reshape script not present at {}",
            scripts_dir.display()
        );
        return;
    }

    // A local `file://` registry tree with our synthetic crate.
    let tree = tempfile::tempdir().unwrap();
    build_sparse_probecrate_tree(tree.path());

    // A consumer project that depends on it via the `tatolab` registry.
    let consumer = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(consumer.path().join("src")).unwrap();
    std::fs::write(
        consumer.path().join("Cargo.toml"),
        "[package]\nname = \"offline-consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nprobecrate = { version = \"0.1\", registry = \"tatolab\" }\n\n\
         [workspace]\n",
    )
    .unwrap();
    std::fs::write(
        consumer.path().join("src").join("main.rs"),
        "fn main() {}\n",
    )
    .unwrap();

    // `registry use <tree>` — reshape + write the [source] replacement. Zero
    // manual config after this call.
    let report = registry::use_registry(
        consumer.path(),
        tree.path().to_str().unwrap(),
        &UseRegistryOptions {
            cargo_local_registry_dir: Some(consumer.path().join(".streamlib").join("clr")),
            reshape_scripts_dir: Some(scripts_dir.clone()),
        },
    )
    .expect("use_registry must configure the consumer");
    assert!(
        matches!(
            report.cargo_replacement,
            CargoReplacementSource::LocalRegistry(_)
        ),
        "a local tree must yield a serverless local-registry replacement"
    );

    // Resolve OFFLINE — no server, no network. Reverting the [source]
    // replacement (or emitting a sparse+http one) makes this fail: cargo would
    // try the dead canonical / a non-running localhost mount.
    let out = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(consumer.path())
        .env("CARGO_NET_OFFLINE", "true")
        // Isolate from any ambient cargo/registry env the test runner set.
        .env_remove("CARGO_REGISTRIES_TATOLAB_INDEX")
        .output()
        .expect("run cargo generate-lockfile");
    assert!(
        out.status.success(),
        "offline resolve failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The lockfile resolved probecrate AND kept the CANONICAL source id (the
    // localhost / local path never leaks into Cargo.lock).
    let lock = std::fs::read_to_string(consumer.path().join("Cargo.lock")).unwrap();
    assert!(
        lock.contains("name = \"probecrate\""),
        "probecrate not in lockfile:\n{lock}"
    );
    // A named registry records its canonical index URL directly (no
    // `registry+` prefix, which is crates-io-only). The point is the same:
    // source replacement kept the CANONICAL id, not the local mirror.
    assert!(
        lock.contains("source = \"sparse+https://registry.tatolab.com/cargo/\""),
        "canonical source id not preserved in lockfile:\n{lock}"
    );
    assert!(
        !lock.contains("127.0.0.1"),
        "localhost leaked into lockfile:\n{lock}"
    );
    assert!(
        !lock.contains(".streamlib"),
        "local mirror path leaked into lockfile:\n{lock}"
    );
}
