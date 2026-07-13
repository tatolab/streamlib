// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Golden tests for the cargo sparse-index renderers — the Python index-line
//! renderer (`scripts/registry/render_cargo_index_line.py`, the single source of
//! truth the shell + xtask emit paths shell out to) and the index-path
//! grammar's two implementations (Rust `cargo_index_path` + bash
//! `cargo-idx-path.sh`).
//!
//! The fixtures under `tests/fixtures/cargo-index/` are CAPTURED, not
//! hand-written: the `*.Cargo.toml` files are the normalized manifests
//! extracted from the real `.crate` tarballs `cargo package` produced for the
//! tatolab/vulkanalia fork, and the `*.golden.ndjson` lines are the index
//! rows emitted in the same end-to-end run — the exact tree a real
//! `cargo generate-lockfile` resolved vulkanalia-vma → vulkanalia →
//! vulkanalia-sys from over a static `python3 -m http.server` mount.

use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_pack::static_registry::cargo_index_path;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cargo-index")
}

/// Build a minimal `.crate`-shaped gzip tar holding the fixture Cargo.toml at
/// `<name>-<version>/Cargo.toml`, run the REAL Python renderer against it,
/// and return the emitted NDJSON line.
fn render_line(name: &str, version: &str, cksum: &str, fixture_toml: &Path) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let pkg_dir = tmp.path().join(format!("{name}-{version}"));
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::copy(fixture_toml, pkg_dir.join("Cargo.toml")).unwrap();
    let crate_file = tmp.path().join(format!("{name}-{version}.crate"));
    let tar = Command::new("tar")
        .arg("-czf")
        .arg(&crate_file)
        .arg("-C")
        .arg(tmp.path())
        .arg(format!("{name}-{version}"))
        .status()
        .expect("tar available");
    assert!(tar.success(), "building the fixture .crate must succeed");

    let out = Command::new("python3")
        .arg(workspace_root().join("scripts/registry/render_cargo_index_line.py"))
        .env("NAME", name)
        .env("VERSION", version)
        .env("CKSUM", cksum)
        .env("CRATE", &crate_file)
        .output()
        .expect("python3 available");
    assert!(
        out.status.success(),
        "renderer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The golden's own `cksum` field — passed back into the renderer so the
/// output is byte-comparable.
fn golden_cksum(golden: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(golden.trim()).unwrap();
    v["cksum"].as_str().unwrap().to_string()
}

#[test]
fn python_renderer_matches_captured_golden_vulkanalia() {
    // vulkanalia is the interesting case: a same-registry fork-sibling dep
    // (vulkanalia-sys), crates.io deps (libloading, raw-window-handle), and
    // target-cfg'd macOS deps.
    let golden = std::fs::read_to_string(fixtures_dir().join("vulkanalia.golden.ndjson")).unwrap();
    let rendered = render_line(
        "vulkanalia",
        "0.35.0",
        &golden_cksum(&golden),
        &fixtures_dir().join("vulkanalia-0.35.0.Cargo.toml"),
    );
    assert_eq!(
        rendered, golden,
        "renderer output drifted from the captured golden"
    );

    // Explicit locks on the load-bearing rules (redundant with byte-equality,
    // spelled out so a future golden re-capture can't silently lose them):
    let v: serde_json::Value = serde_json::from_str(rendered.trim()).unwrap();
    let deps = v["deps"].as_array().unwrap();
    // 1. Fork-sibling dep OMITS the `registry` key (same-registry semantics).
    let sys = deps.iter().find(|d| d["name"] == "vulkanalia-sys").unwrap();
    assert!(
        sys.get("registry").is_none(),
        "fork-sibling dep must omit `registry`: {sys}"
    );
    // 2. crates.io dep names the canonical crates.io index.
    let libloading = deps.iter().find(|d| d["name"] == "libloading").unwrap();
    assert_eq!(
        libloading["registry"], "https://github.com/rust-lang/crates.io-index",
        "crates.io dep must name the crates.io index"
    );
    // 3. Bare manifest version reqs gain the caret (`0.35` → `^0.35`).
    assert_eq!(sys["req"], "^0.35");
}

#[test]
fn python_renderer_matches_captured_golden_vulkanalia_vma() {
    // vulkanalia-vma adds the build-dep kind (cc, bindgen) on top of the
    // fork-sibling (vulkanalia) + crates.io (bitflags) mix.
    let golden =
        std::fs::read_to_string(fixtures_dir().join("vulkanalia-vma.golden.ndjson")).unwrap();
    let rendered = render_line(
        "vulkanalia-vma",
        "0.9.0",
        &golden_cksum(&golden),
        &fixtures_dir().join("vulkanalia-vma-0.9.0.Cargo.toml"),
    );
    assert_eq!(
        rendered, golden,
        "renderer output drifted from the captured golden"
    );

    let v: serde_json::Value = serde_json::from_str(rendered.trim()).unwrap();
    let deps = v["deps"].as_array().unwrap();
    let cc = deps.iter().find(|d| d["name"] == "cc").unwrap();
    assert_eq!(
        cc["kind"], "build",
        "build-dependencies must carry kind=build"
    );
    let vk = deps.iter().find(|d| d["name"] == "vulkanalia").unwrap();
    assert!(
        vk.get("registry").is_none(),
        "fork-sibling dep must omit `registry`"
    );
}

/// The index-path grammar exists in TWO implementations — Rust
/// `cargo_index_path` (xtask closure emit) and bash `cargo_idx_path`
/// (`scripts/registry/cargo-idx-path.sh`, sourced by emit-static-fork.sh). Feed
/// both the same names and require identical output, against the expected
/// paths for every grammar arm (1/2/3-char + sharded 4+, lowercasing).
#[test]
fn index_path_grammar_identical_across_rust_and_bash() {
    let cases = [
        ("a", "1/a"),
        ("ab", "2/ab"),
        ("abc", "3/a/abc"),
        ("serde", "se/rd/serde"),
        ("Serde", "se/rd/serde"), // lowercased
        ("vulkanalia", "vu/lk/vulkanalia"),
        ("vulkanalia-sys", "vu/lk/vulkanalia-sys"),
        ("vulkanalia-vma", "vu/lk/vulkanalia-vma"),
        ("streamlib-plugin-sdk", "st/re/streamlib-plugin-sdk"),
    ];
    let script = workspace_root().join("scripts/registry/cargo-idx-path.sh");
    for (name, expected) in cases {
        assert_eq!(cargo_index_path(name), expected, "rust grammar for {name}");
        let out = Command::new("bash")
            .arg(&script)
            .arg(name)
            .output()
            .expect("bash available");
        assert!(out.status.success(), "cargo-idx-path.sh failed for {name}");
        let bash_path = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert_eq!(bash_path, expected, "bash grammar for {name}");
    }
}
