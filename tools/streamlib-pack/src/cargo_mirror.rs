// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cargo **source-replacement mirror** emission — the link-free, fully-offline
//! build side of the static registry.
//!
//! A package a downstream consumer writes declares the engine/SDK crates by
//! bare version (`streamlib = "0.6.0"`) with **no** `streamlib link` and **no**
//! `[patch.crates-io]`. There is no crates registry and no publish step, so
//! that bare version can only resolve if the whole crates.io dependency closure
//! of the engine/SDK chain is present locally. This module emits exactly that:
//! a cargo **directory source** carrying the FULL closure, plus the generated
//! `[source]` replacement that redirects `crates-io` at it. A build against the
//! emitted tree then resolves + compiles the package entirely offline — the way
//! the separate-build `.slpkg` validation gate builds a package by version.
//!
//! ## Why a directory source (not a hand-rolled sparse local-registry)
//!
//! The transitive crates.io closure is hundreds of crates. `cargo vendor`
//! produces the whole closure — unpacked sources plus per-crate
//! `.cargo-checksum.json` — as a cargo directory source, and cargo itself
//! computes every checksum and dependency edge; there are no sparse index lines
//! to hand-render (one subtly-wrong `deps`/`features`/`registry` line among
//! hundreds would break resolution). The only crates `cargo vendor` cannot
//! carry are the engine/SDK crates themselves — they are workspace path members,
//! not registry crates — so this module packages each ([`crate::crate_tarball`],
//! byte-stable) and injects it into the same directory source as an extra entry.
//!
//! ## Source *replacement* keeps `--locked` clean
//!
//! The generated config uses `[source.crates-io] replace-with = <mirror>`, which
//! **preserves** the canonical `registry+https://github.com/rust-lang/crates.io-index`
//! source id in a consumer's `Cargo.lock`. A `CARGO_REGISTRIES_*_INDEX` env
//! override would instead rewrite that source id and break `--locked`.
//!
//! ## Growing directory source, topological packaging
//!
//! An engine crate's packaged manifest declares its siblings by version
//! (`streamlib-engine = "0.6.0"`), a crates.io dep once the path is stripped —
//! so `cargo package` validates it against the (replaced) `crates-io` source.
//! Crates are therefore packaged in the release closure's topological order
//! (leaf-first) and each is injected into the directory source **before** its
//! dependents are packaged, so every sibling resolves from the growing mirror.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

use crate::ReleaseClosure;
use crate::crate_tarball::finalize_crate_tarball;

/// Subdir under the emitted registry tree holding the whole cargo mirror.
pub const CARGO_MIRROR_SUBDIR: &str = "cargo-mirror";
/// Subdir under [`CARGO_MIRROR_SUBDIR`] holding the vendored directory source
/// (both the `cargo vendor` closure and the injected engine/SDK crates).
pub const VENDOR_SUBDIR: &str = "vendor";
/// The generated `[source]` replacement config a consumer points cargo at.
pub const SOURCE_REPLACEMENT_CONFIG_FILE: &str = "config.toml";

/// The `[source.<name>]` the mirror replacement is written under. A distinct,
/// zero-context name so a consumer's own cargo config never collides with it.
const MIRROR_SOURCE_NAME: &str = "streamlib-cargo-mirror";

/// Render the `[source]` replacement stanza that redirects the canonical
/// `crates-io` source at the vendored directory source rooted at `vendor_dir`.
///
/// A consumer writes this into its `.cargo/config.toml`; cargo then resolves
/// every crates.io dep — including the engine/SDK crates injected into the
/// directory source — from the local tree with no network. Source *replacement*
/// (not an index override) preserves the canonical crates.io source id in the
/// consumer's `Cargo.lock`, so a later `--locked` build stays reproducible.
pub fn render_source_replacement_config(vendor_dir: &Path) -> String {
    format!(
        "[source.crates-io]\nreplace-with = \"{name}\"\n\n[source.{name}]\ndirectory = \"{dir}\"\n",
        name = MIRROR_SOURCE_NAME,
        dir = vendor_dir.display(),
    )
}

/// A cargo directory-source `.cargo-checksum.json`: `files` (per-source-file
/// sha256, which cargo verifies against the on-disk sources) + `package` (the
/// `.crate` tarball sha256, recorded verbatim as the crate's `checksum` in a
/// consumer's `Cargo.lock`).
#[derive(serde::Serialize)]
struct CargoDirectorySourceChecksum {
    /// Sorted (`BTreeMap`) so re-serializing identical inputs yields identical
    /// bytes — the byte-stability the whole mirror preserves.
    files: BTreeMap<String, String>,
    package: String,
}

/// Render a directory-source `.cargo-checksum.json` from a `relpath → sha256`
/// file map and the `.crate` tarball's sha256. Keys are emitted sorted, so the
/// output is a pure function of its inputs.
pub fn render_cargo_checksum_json(
    files: &BTreeMap<String, String>,
    package_sha256: &str,
) -> Result<String> {
    serde_json::to_string(&CargoDirectorySourceChecksum {
        files: files.clone(),
        package: package_sha256.to_string(),
    })
    .context("serialize .cargo-checksum.json")
}

/// Unpack the byte-stable `.crate` at `crate_path` into
/// `<vendor_dir>/<name>/` as a cargo **directory-source** entry: strip the
/// `<name>-<version>/` tar prefix, write each file, and emit the
/// `.cargo-checksum.json` (`files` = per-file sha256, `package` =
/// `package_sha256`). A pre-existing entry is cleared first so a re-emit is
/// clean.
///
/// The engine/SDK crates are workspace path members `cargo vendor` cannot carry,
/// so this injects each into the same directory source the vendored crates.io
/// closure lives in. cargo identifies the crate by the `Cargo.toml` inside the
/// entry (not the directory name), and verifies each file against `files` on
/// load — so the unpacked bytes and the recorded hashes must agree exactly,
/// which they do because both come from the one normalized `.crate`.
pub fn inject_crate_into_directory_source(
    crate_path: &Path,
    name: &str,
    version: &str,
    package_sha256: &str,
    vendor_dir: &Path,
) -> Result<()> {
    let dest = vendor_dir.join(name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("clear stale directory-source entry {}", dest.display()))?;
    }
    std::fs::create_dir_all(&dest)
        .with_context(|| format!("create directory-source entry {}", dest.display()))?;

    let bytes = std::fs::read(crate_path)
        .with_context(|| format!("read crate tarball {}", crate_path.display()))?;
    let mut decoded = Vec::new();
    GzDecoder::new(&bytes[..])
        .read_to_end(&mut decoded)
        .with_context(|| format!("gzip-decode crate tarball {}", crate_path.display()))?;

    let prefix = format!("{name}-{version}/");
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut archive = tar::Archive::new(&decoded[..]);
    for entry in archive
        .entries()
        .with_context(|| format!("read crate tar entries {}", crate_path.display()))?
    {
        let mut entry = entry.context("read crate tar entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let entry_path = entry
            .path()
            .context("read crate tar entry path")?
            .into_owned();
        let raw = entry_path.to_string_lossy();
        let rel = raw.strip_prefix(&prefix).unwrap_or(&raw).to_string();

        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .context("read crate tar entry data")?;

        let out_path = dest.join(&rel);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&out_path, &data)
            .with_context(|| format!("write {}", out_path.display()))?;

        let mut hasher = Sha256::new();
        hasher.update(&data);
        files.insert(rel, format!("{:x}", hasher.finalize()));
    }

    let checksum = render_cargo_checksum_json(&files, package_sha256)?;
    std::fs::write(dest.join(".cargo-checksum.json"), checksum)
        .with_context(|| format!("write .cargo-checksum.json in {}", dest.display()))?;
    Ok(())
}

/// The `--config` args that replace the canonical `crates-io` source with the
/// directory source at `vendor_dir` for one `cargo package` invocation (no
/// workspace `.cargo/config.toml` is touched).
fn source_replacement_config_args(vendor_dir: &Path) -> Vec<String> {
    vec![
        format!("source.crates-io.replace-with=\"{MIRROR_SOURCE_NAME}\""),
        format!(
            "source.{MIRROR_SOURCE_NAME}.directory=\"{}\"",
            vendor_dir.display()
        ),
    ]
}

/// Vendor the workspace's full crates.io closure into `vendor_dir` as a cargo
/// directory source. Runs on the emit machine (network permitted); the emitted
/// tree is what a consumer resolves offline.
fn run_cargo_vendor(workspace_root: &Path, vendor_dir: &Path) -> Result<()> {
    if let Some(parent) = vendor_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let output = Command::new("cargo")
        .arg("vendor")
        .arg("--manifest-path")
        .arg(workspace_root.join("Cargo.toml"))
        .arg(vendor_dir)
        .current_dir(workspace_root)
        // The printed `[source]` stanza (stdout) is not consumed — this module
        // renders its own config for the FINAL served path.
        .stdout(std::process::Stdio::null())
        .output()
        .context("run `cargo vendor` for the cargo mirror closure")?;
    if !output.status.success() {
        anyhow::bail!(
            "`cargo vendor` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// `cargo package --no-verify --offline` one release-closure crate against the
/// growing directory source (crates.io replaced by `vendor_dir`, so already
/// injected siblings + the vendored closure resolve with no network). The
/// `.crate` lands in `<workspace>/target/package/`.
fn run_cargo_package(workspace_root: &Path, crate_name: &str, vendor_dir: &Path) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.args(["package", "--no-verify", "--offline", "--allow-dirty", "-p"])
        .arg(crate_name);
    for arg in source_replacement_config_args(vendor_dir) {
        cmd.arg("--config").arg(arg);
    }
    let output = cmd
        .current_dir(workspace_root)
        .output()
        .with_context(|| format!("run `cargo package -p {crate_name}`"))?;
    if !output.status.success() {
        anyhow::bail!(
            "`cargo package -p {crate_name}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Emit the cargo source-replacement mirror into `<staging>/cargo-mirror/`: the
/// vendored crates.io closure + every engine/SDK release-closure crate, plus the
/// generated `[source]` replacement config pointing at the FINAL served vendor
/// dir (under `out`, where the staging tree flips to). Returns nothing; the
/// caller lists the same closure in the release manifest's `crates`.
///
/// `closure` must be in topological (leaf-first) order so each crate's siblings
/// are injected before it is packaged.
pub fn emit_cargo_mirror(
    workspace_root: &Path,
    staging: &Path,
    out: &Path,
    closure: &ReleaseClosure,
) -> Result<()> {
    let mirror_dir = staging.join(CARGO_MIRROR_SUBDIR);
    let vendor_dir = mirror_dir.join(VENDOR_SUBDIR);
    std::fs::create_dir_all(&mirror_dir)
        .with_context(|| format!("create cargo mirror dir {}", mirror_dir.display()))?;

    tracing::info!(
        vendor = %vendor_dir.display(),
        "cargo mirror: vendoring the crates.io closure"
    );
    run_cargo_vendor(workspace_root, &vendor_dir)?;

    for member in &closure.crates {
        let crate_path = workspace_root
            .join("target/package")
            .join(format!("{}-{}.crate", member.name, member.version));
        crate::crate_tarball::obtain_crate_tarball(
            &crate_path,
            &member.name,
            &member.version,
            || run_cargo_package(workspace_root, &member.name, &vendor_dir),
        )?;
        // Byte-stable normalize (strip git-HEAD vcs-info, fixed-header re-gzip)
        // so `package` — the checksum a consumer's lock records — is a pure
        // function of source, independent of the commit the emit ran at. First
        // emit into a fresh staging tree ⇒ no prior `.crate` to guard against.
        let package_sha256 =
            finalize_crate_tarball(&crate_path, &member.name, &member.version, None)?;
        inject_crate_into_directory_source(
            &crate_path,
            &member.name,
            &member.version,
            &package_sha256,
            &vendor_dir,
        )?;
        tracing::info!(
            crate_name = %member.name,
            version = %member.version,
            "cargo mirror: packaged + injected engine/SDK crate"
        );
    }

    // The config's directory must name the FINAL served location (the staging
    // tree flips into `out` atomically after this returns), so a consumer that
    // reads the config post-flip resolves against a path that exists.
    let served_vendor_dir = out.join(CARGO_MIRROR_SUBDIR).join(VENDOR_SUBDIR);
    let config_path = mirror_dir.join(SOURCE_REPLACEMENT_CONFIG_FILE);
    std::fs::write(
        &config_path,
        render_source_replacement_config(&served_vendor_dir),
    )
    .with_context(|| format!("write {}", config_path.display()))?;
    tracing::info!(
        crates = closure.crates.len(),
        config = %config_path.display(),
        "cargo mirror emit complete"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// The `[source]` stanza uses `replace-with` (not an index override) so the
    /// canonical crates.io source id survives into a consumer's `Cargo.lock`,
    /// and names the directory source with an absolute path.
    #[test]
    fn source_replacement_config_replaces_crates_io_with_directory() {
        let cfg = render_source_replacement_config(Path::new("/srv/reg/cargo-mirror/vendor"));
        assert!(cfg.contains("[source.crates-io]"), "{cfg}");
        assert!(
            cfg.contains("replace-with = \"streamlib-cargo-mirror\""),
            "must replace, never override the index: {cfg}"
        );
        assert!(
            cfg.contains("[source.streamlib-cargo-mirror]"),
            "names the replacement source: {cfg}"
        );
        assert!(
            cfg.contains("directory = \"/srv/reg/cargo-mirror/vendor\""),
            "points at the vendored directory source: {cfg}"
        );
        assert!(
            !cfg.to_ascii_uppercase().contains("CARGO_REGISTRIES"),
            "an index env override would break --locked; must not appear: {cfg}"
        );
    }

    /// `.cargo-checksum.json` carries sorted `files` + the `package` digest, and
    /// is byte-stable for identical inputs (BTreeMap ordering).
    #[test]
    fn cargo_checksum_json_is_sorted_and_stable() {
        let mut files = BTreeMap::new();
        files.insert("src/lib.rs".to_string(), "bbbb".to_string());
        files.insert("Cargo.toml".to_string(), "aaaa".to_string());
        let a = render_cargo_checksum_json(&files, "deadbeef").unwrap();
        let b = render_cargo_checksum_json(&files, "deadbeef").unwrap();
        assert_eq!(a, b, "identical inputs must render byte-identically");
        // Cargo.toml sorts before src/lib.rs.
        let cargo_at = a.find("Cargo.toml").unwrap();
        let lib_at = a.find("src/lib.rs").unwrap();
        assert!(cargo_at < lib_at, "files keys must be sorted: {a}");
        assert!(a.contains("\"package\":\"deadbeef\""), "{a}");
    }

    /// Build a minimal `.crate` (gzip-tar with the `<name>-<version>/` prefix)
    /// mirroring `cargo package` layout.
    fn write_fake_crate(path: &Path, name: &str, version: &str, entries: &[(&str, &[u8])]) {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut builder = tar::Builder::new(Vec::new());
        for (rel, data) in entries {
            let full = format!("{name}-{version}/{rel}");
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, &full, *data).unwrap();
        }
        let raw = builder.into_inner().unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut enc = GzEncoder::new(file, Compression::default());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
    }

    /// Injecting a `.crate` produces a directory-source entry cargo accepts:
    /// the `<name>-<version>/` prefix is stripped, every file lands unpacked,
    /// and `.cargo-checksum.json` records the on-disk file hashes + `package`.
    #[test]
    fn inject_unpacks_and_writes_matching_checksums() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        let crate_path = dir.path().join("streamlib-idents-0.6.0.crate");
        let manifest = b"[package]\nname = \"streamlib-idents\"\nversion = \"0.6.0\"\n";
        let lib = b"pub fn ok() {}\n";
        write_fake_crate(
            &crate_path,
            "streamlib-idents",
            "0.6.0",
            &[("Cargo.toml", manifest), ("src/lib.rs", lib)],
        );

        inject_crate_into_directory_source(
            &crate_path,
            "streamlib-idents",
            "0.6.0",
            "abc123",
            &vendor,
        )
        .unwrap();

        let entry = vendor.join("streamlib-idents");
        // Prefix stripped: files live at the entry root, not under a versioned dir.
        assert_eq!(std::fs::read(entry.join("Cargo.toml")).unwrap(), manifest);
        assert_eq!(std::fs::read(entry.join("src/lib.rs")).unwrap(), lib);

        // The recorded per-file hashes match the unpacked bytes exactly (what
        // cargo verifies on load); `package` is passed through verbatim.
        let checksum: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.join(".cargo-checksum.json")).unwrap())
                .unwrap();
        assert_eq!(checksum["package"], "abc123");
        let mut h = Sha256::new();
        h.update(lib);
        let want = format!("{:x}", h.finalize());
        assert_eq!(checksum["files"]["src/lib.rs"], want);
    }

    /// Re-injecting over an existing entry replaces it wholesale (stale files
    /// from a prior emit do not survive to corrupt the checksum map).
    #[test]
    fn inject_replaces_stale_entry() {
        let dir = tempfile::tempdir().unwrap();
        let vendor = dir.path().join("vendor");
        let stale = vendor.join("streamlib-idents");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("leftover.rs"), b"// stale\n").unwrap();

        let crate_path = dir.path().join("c.crate");
        write_fake_crate(
            &crate_path,
            "streamlib-idents",
            "0.6.0",
            &[
                ("Cargo.toml", b"[package]\nname=\"streamlib-idents\"\n"),
                ("src/lib.rs", b"// fresh\n"),
            ],
        );
        inject_crate_into_directory_source(
            &crate_path,
            "streamlib-idents",
            "0.6.0",
            "sha",
            &vendor,
        )
        .unwrap();
        assert!(
            !stale.join("leftover.rs").exists(),
            "a stale file from a prior emit must not survive re-injection"
        );
    }
}
