// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Static-file registry tree emission — the daemon-free read side.
//!
//! A registry's read side is just static files: a cargo sparse index +
//! tarballs, a PEP-503 pypi-simple tree, an npm packument + tgz, and the
//! existing `.slpkg` generic store. This module renders those layouts into a
//! plain on-disk directory that is tokenless to read and browsable as an HTTP
//! directory index — served identically whether it is a CI fixture, a local
//! publish-and-read folder, or a cloud object store.
//!
//! Per-ecosystem read transport (each ecosystem's native anonymous read path):
//! - **`.slpkg` generic** → `file://` (the existing [`streamlib_idents::RegistryClient`]
//!   transport — reused here, not rebuilt).
//! - **pypi-simple** → `file://` (uv/pip consume a PEP-503 `simple/` tree over
//!   `file://` natively).
//! - **cargo sparse** → a dumb static HTTP mount (`sparse+http://…`): the cargo
//!   sparse protocol is HTTP-only by spec, and a static file server is not a
//!   registry daemon.
//! - **npm** → the same static HTTP mount (packument JSON + `.tgz`; the
//!   `dist.tarball` URL points at the mount).
//!
//! ## Atomicity
//!
//! A `file://` consumer must never observe a half-written tree. The tree is
//! built in a staging directory and moved into the served path only after the
//! [`ReleaseManifest`] lands — the whole release flips at once. See
//! [`publish_staged_tree`].

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use streamlib_idents::{
    render_catalog_index_ndjson, schema_jtd_file_name, CatalogIndexLine, PackageRef, RegistryClient,
    RegistryConfig, ReleaseManifest, ReleaseManifestMember, SemVer, CATALOG_INDEX_PATH,
};

use crate::catalog::{build_package_catalog, build_sibling_versions};

/// Which ecosystems an emit run produces. Each maps to its native anonymous
/// read transport (see the module docs).
#[derive(Debug, Clone)]
pub struct EmitEcosystems {
    /// Emit the vulkanalia fork cargo tree (the daemon-free bootstrap the CI
    /// resolve needs). Delegates to `scripts/registry/emit-static-fork.sh`.
    pub cargo_fork: bool,
    /// Package + emit the workspace release-closure crates into the cargo tree.
    pub cargo_closure: bool,
    /// Emit the PEP-503 pypi-simple tree (Python SDK sdist).
    pub pypi: bool,
    /// Emit the npm packument + tgz (Deno SDK).
    pub npm: bool,
    /// Emit the `.slpkg` generic store (packages) + the release manifest.
    pub slpkg: bool,
}

impl Default for EmitEcosystems {
    fn default() -> Self {
        Self {
            cargo_fork: true,
            cargo_closure: false,
            pypi: true,
            npm: true,
            slpkg: true,
        }
    }
}

/// Options for [`emit_static_registry`].
#[derive(Debug, Clone)]
pub struct EmitOptions {
    pub workspace_root: PathBuf,
    /// Final served tree root. Built in a staging sibling and flipped in
    /// atomically once complete.
    pub out: PathBuf,
    /// Absolute base URL the cargo + npm mounts are served at (sparse/npm are
    /// HTTP-only; baked into `config.json` + packuments). `file://` ecosystems
    /// (slpkg, pypi) ignore it.
    pub base_url: String,
    /// `-dev.N` suffix for the emitted versions.
    pub dev: Option<u32>,
    pub ecosystems: EmitEcosystems,
}

/// Canonical crates.io index URL — the `registry` value a cargo sparse index
/// line uses to say "fetch this dependency from crates.io, not this registry".
pub const CRATES_IO_INDEX: &str = "https://github.com/rust-lang/crates.io-index";

/// The cargo sparse-index path for a crate `name` (RFC 2141 layout): 1/2/3-char
/// names get short prefixes; 4+ char names shard on the first two pairs.
///
/// `serde` → `se/rd/serde`; `abc` → `3/a/abc`; `ab` → `2/ab`; `a` → `1/a`.
pub fn cargo_index_path(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    match n.chars().count() {
        1 => format!("1/{n}"),
        2 => format!("2/{n}"),
        3 => format!("3/{}/{n}", &n[0..1]),
        _ => format!("{}/{}/{n}", &n[0..2], &n[2..4]),
    }
}

/// Render the cargo sparse-index `config.json` for a tree served at `base_url`.
/// The templated `dl` yields clean, browsable `.crate` filenames.
pub fn render_cargo_config_json(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!(
        "{{\"dl\":\"{base}/cargo/crates/{{crate}}/{{crate}}-{{version}}.crate\",\"api\":\"{base}/cargo\"}}\n"
    )
}

/// sha256 hex digest of the bytes at `path` — the `cksum` field of a cargo
/// index line, and the integrity a `file://` consumer verifies.
pub fn sha256_hex(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Render a PEP-503 "simple" project index page. `files` is `(filename,
/// sha256hex)` for each artifact; the tree serves each at `../../<filename>`
/// relative to `simple/<project>/index.html`, matching pip/uv's file layout.
pub fn render_pypi_simple_project(project: &str, files: &[(String, String)]) -> String {
    let mut body = String::new();
    body.push_str("<!DOCTYPE html>\n<html>\n<head>\n");
    body.push_str(&format!("<title>Links for {project}</title>\n"));
    body.push_str("</head>\n<body>\n");
    body.push_str(&format!("<h1>Links for {project}</h1>\n"));
    for (filename, sha) in files {
        body.push_str(&format!(
            "<a href=\"../../packages/{filename}#sha256={sha}\">{filename}</a><br/>\n"
        ));
    }
    body.push_str("</body>\n</html>\n");
    body
}

/// Render the top-level PEP-503 simple root index listing every project.
pub fn render_pypi_simple_root(projects: &[String]) -> String {
    let mut body = String::new();
    body.push_str("<!DOCTYPE html>\n<html>\n<head>\n<title>Simple index</title>\n</head>\n<body>\n");
    for p in projects {
        // PEP 503 normalized project name for the link path.
        let norm = p.to_ascii_lowercase().replace(['_', '.'], "-");
        body.push_str(&format!("<a href=\"{norm}/\">{p}</a><br/>\n"));
    }
    body.push_str("</body>\n</html>\n");
    body
}

/// One published version in an npm packument.
#[derive(Debug, Clone)]
pub struct NpmVersion {
    pub version: String,
    pub tarball_filename: String,
    pub shasum_sha1_hex: String,
    pub integrity_sha512_b64: String,
}

/// Render an npm packument (the `GET /<name>` document) for `name` at
/// `base_url`, listing `versions`. `dist.tarball` points at the static mount.
pub fn render_npm_packument(name: &str, base_url: &str, versions: &[NpmVersion]) -> String {
    let base = base_url.trim_end_matches('/');
    let mut versions_obj = serde_json::Map::new();
    let mut latest = String::new();
    for v in versions {
        latest = v.version.clone();
        // Tarballs live at a sibling path that cannot collide with the
        // packument FILE at `npm/<scope>/<name>` (see `emit_npm`).
        let tarball = format!("{base}/npm/tarballs/{}", v.tarball_filename);
        let entry = serde_json::json!({
            "name": name,
            "version": v.version,
            "dist": {
                "tarball": tarball,
                "shasum": v.shasum_sha1_hex,
                "integrity": format!("sha512-{}", v.integrity_sha512_b64),
            },
        });
        versions_obj.insert(v.version.clone(), entry);
    }
    let doc = serde_json::json!({
        "name": name,
        "dist-tags": { "latest": latest },
        "versions": versions_obj,
    });
    serde_json::to_string(&doc).expect("packument serializes")
}

/// Atomically publish a fully-built `staging` tree into the served `served`
/// path so a concurrent `file://` reader observes either the OLD complete tree
/// or the NEW complete tree — never a partial one.
///
/// The caller builds `staging` completely (every ecosystem + the
/// [`ReleaseManifest`] written LAST) before calling this. `staging` and
/// `served` must live on the same filesystem (same parent) so the flip is a
/// single rename.
///
/// - `served` absent → one atomic `rename(staging → served)` (gapless: the
///   tree appears in a single instant).
/// - `served` present → a single gapless swap. On Linux this is
///   `renameat2(RENAME_EXCHANGE)`, which atomically exchanges `staging` and
///   `served` in one syscall — there is no instant at which `served` is
///   absent or partial. The old tree ends up at `staging` and is removed
///   afterward. On other platforms (this tooling is Linux-only in practice)
///   it degrades to rename-aside, which has a sub-instant window where
///   `served` is momentarily absent (never partial — the tree is always
///   whole).
pub fn publish_staged_tree(staging: &Path, served: &Path) -> Result<()> {
    if !staging.is_dir() {
        anyhow::bail!("staging tree {} does not exist", staging.display());
    }
    if let Some(parent) = served.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if !served.exists() {
        return std::fs::rename(staging, served)
            .with_context(|| format!("rename {} → {}", staging.display(), served.display()));
    }
    // `served` exists → gapless replace where the platform supports it.
    #[cfg(target_os = "linux")]
    {
        match exchange_paths(staging, served) {
            Ok(()) => {
                // After the exchange, `staging` holds the old tree.
                std::fs::remove_dir_all(staging).ok();
                return Ok(());
            }
            // Filesystem / kernel without RENAME_EXCHANGE support (e.g. some
            // network / FUSE mounts, pre-3.15 kernels) → degrade to the
            // rename-aside path below (sub-instant absence window, never a
            // partial tree).
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS)) => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("atomically swap {} ⇄ {}", staging.display(), served.display())
                });
            }
        }
    }
    rename_aside_replace(staging, served)
}

/// Replace `served` with `staging` via rename-aside: old tree renamed away,
/// staging renamed in, old removed. Never exposes a partial tree; has a
/// sub-instant window where `served` is absent (the RENAME_EXCHANGE-less
/// fallback).
fn rename_aside_replace(staging: &Path, served: &Path) -> Result<()> {
    let backup = sibling_temp(served, "old");
    std::fs::rename(served, &backup)
        .with_context(|| format!("rename {} → {}", served.display(), backup.display()))?;
    match std::fs::rename(staging, served) {
        Ok(()) => {
            std::fs::remove_dir_all(&backup).ok();
            Ok(())
        }
        Err(e) => {
            std::fs::rename(&backup, served).ok();
            Err(e).with_context(|| {
                format!("rename {} → {}", staging.display(), served.display())
            })
        }
    }
}

/// Atomically exchange two paths via `renameat2(RENAME_EXCHANGE)` (Linux).
/// Both paths must exist; on success `a` and `b` have traded inodes in a
/// single, uninterruptible operation.
#[cfg(target_os = "linux")]
fn exchange_paths(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: both CStrings are valid NUL-terminated paths held for the call;
    // AT_FDCWD resolves the relative/absolute paths against the cwd.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// A uniquely-named sibling path next to `path` (same parent → same
/// filesystem), tagged `tag`, for staging/backup dirs.
pub fn sibling_temp(path: &Path, tag: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tree".to_string());
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    parent.join(format!(".{name}.{tag}.{nonce}"))
}

/// The registry org the tree is emitted under (`STREAMLIB_REGISTRY_ORG`, default `tatolab`).
fn registry_org() -> String {
    std::env::var("STREAMLIB_REGISTRY_ORG")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tatolab".to_string())
}

fn workspace_version(workspace_root: &Path) -> Result<String> {
    let path = workspace_root.join("Cargo.toml");
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let doc: toml::Value =
        toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("[workspace.package].version missing from workspace Cargo.toml")
}

fn target_version(base: &str, dev: Option<u32>) -> String {
    match dev {
        Some(n) => format!("{base}-dev.{n}"),
        None => base.to_string(),
    }
}

/// Emit a complete static registry tree for the current workspace release into
/// [`EmitOptions::out`], via a staging dir that is flipped in atomically once
/// the [`ReleaseManifest`] lands (see [`publish_staged_tree`]). The whole tree
/// is browsable as a plain HTTP directory index.
pub fn emit_static_registry(opts: &EmitOptions) -> Result<()> {
    let base = workspace_version(&opts.workspace_root)?;
    let target = target_version(&base, opts.dev);
    let org = registry_org();

    build_and_flip(&opts.out, |staging| {
        if opts.ecosystems.cargo_fork {
            emit_cargo_fork(opts, staging)?;
        }
        if opts.ecosystems.cargo_closure {
            emit_cargo_closure(opts, staging)?;
        }
        if opts.ecosystems.pypi {
            emit_pypi(opts, staging, &target)?;
        }
        if opts.ecosystems.npm {
            emit_npm(opts, staging, &target)?;
        }
        if opts.ecosystems.slpkg {
            emit_slpkg_and_manifest(opts, staging, &target, &org)?;
        }
        Ok(())
    })?;
    tracing::info!(out = %opts.out.display(), release = %target, "static registry emitted");
    Ok(())
}

/// The staged-swap seam: run `build` against a fresh staging sibling of `out`,
/// then flip staging into `out` atomically ([`publish_staged_tree`]). On build
/// error the staging dir is removed and `out` is untouched — a crashed /
/// failed emit never perturbs the served tree.
pub fn build_and_flip(out: &Path, build: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let staging = sibling_temp(out, "staging");
    // Best-effort clean of any prior staging remnant, then a fresh dir.
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;
    match build(&staging) {
        Ok(()) => publish_staged_tree(&staging, out),
        Err(e) => {
            std::fs::remove_dir_all(&staging).ok();
            Err(e)
        }
    }
}

/// Delegate the vulkanalia-fork cargo tree to the standalone shell emitter
/// (the daemon-free bootstrap that must not require the workspace to build).
fn emit_cargo_fork(opts: &EmitOptions, staging: &Path) -> Result<()> {
    let script = opts
        .workspace_root
        .join("scripts/registry/emit-static-fork.sh");
    let status = Command::new("bash")
        .arg(&script)
        .arg(staging)
        .arg("--base-url")
        .arg(&opts.base_url)
        .status()
        .with_context(|| format!("run {}", script.display()))?;
    if !status.success() {
        anyhow::bail!("emit-static-fork.sh failed");
    }
    Ok(())
}

/// Package each workspace release-closure crate with `cargo package` and render
/// its `.crate` + sparse-index line into the staging cargo tree.
/// The `.crate` artifact filename `cargo package` produces for a crate:
/// always `{name}-{manifest_version}.crate`, where the version is the crate's
/// actual `Cargo.toml` version. Never re-derive the version (e.g. by stamping
/// a `-dev.N` the manifests don't carry) — `cargo package` embeds the manifest
/// version, so index/filename/manifest must all follow it. A `--dev` closure
/// emit expects the workspace manifests already bumped (the publish scripts'
/// bump/restore convention).
pub fn crate_artifact_filename(name: &str, manifest_version: &str) -> String {
    format!("{name}-{manifest_version}.crate")
}

/// Kills a spawned child process on drop — the ephemeral staging server's
/// lifetime guard inside [`emit_cargo_closure`].
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Package each workspace release-closure crate with `cargo package` and
/// render its `.crate` + sparse-index line into the staging cargo tree.
///
/// `cargo package` validates every `registry = "tatolab"` dep against the live
/// index, so the closure is packaged in topo order against an EPHEMERAL
/// static server on the staging tree itself: each crate resolves its
/// already-emitted siblings (and the fork, emitted before this) from the
/// growing staging index. The staging `config.json` points at the ephemeral
/// server during packaging and is stamped with the final base URL afterward
/// — the served tree never observes the ephemeral URL.
fn emit_cargo_closure(opts: &EmitOptions, staging: &Path) -> Result<()> {
    let closure = crate::compute_release_closure(&opts.workspace_root)?;
    let cargo_dir = staging.join("cargo");
    std::fs::create_dir_all(cargo_dir.join("crates"))?;
    let render = opts
        .workspace_root
        .join("scripts/registry/render_cargo_index_line.py");

    // Ephemeral staging server: pick a free port, serve the staging dir.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .context("bind ephemeral port for the staging index server")?;
        listener.local_addr()?.port()
    };
    let ephemeral_base = format!("http://127.0.0.1:{port}");
    std::fs::write(cargo_dir.join("config.json"), render_cargo_config_json(&ephemeral_base))?;
    let server = KillOnDrop(
        Command::new("python3")
            .args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1", "--directory"])
            .arg(staging)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawn the ephemeral staging index server (python3)")?,
    );
    // Wait for it to accept connections.
    let mut up = false;
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::ensure!(up, "ephemeral staging index server did not come up on {ephemeral_base}");
    let staging_index = format!("sparse+{ephemeral_base}/cargo/");

    for c in &closure.crates {
        // The packaged artifact carries the crate's ACTUAL manifest version
        // (see `crate_artifact_filename`); a dev emit bumps manifests first.
        let version = c.version.clone();
        let crate_file = opts
            .workspace_root
            .join("target/package")
            .join(crate_artifact_filename(&c.name, &version));
        // Reuse a previously-packaged `.crate` only after structural
        // verification (a truncated leftover from an aborted emit is
        // discarded + repackaged, never trusted). A verified reuse skips
        // `cargo package` — and with it that crate's live registry-dep
        // validation — but the emitted index line is rendered from the
        // reused tarball's own manifest, so the tree stays correct; a
        // per-version tarball is treated as immutable for reuse.
        let provenance = crate::crate_tarball::obtain_crate_tarball(
            &crate_file,
            &c.name,
            &version,
            || {
                let out = Command::new("cargo")
                    .args(["package", "--no-verify", "--allow-dirty", "-p", &c.name])
                    .env("CARGO_REGISTRIES_TATOLAB_INDEX", &staging_index)
                    .current_dir(&opts.workspace_root)
                    .output()
                    .with_context(|| format!("cargo package -p {}", c.name))?;
                if !out.status.success() {
                    anyhow::bail!(
                        "cargo package -p {} failed: {}",
                        c.name,
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                Ok(())
            },
        )?;
        tracing::info!(
            crate_name = %c.name,
            version = %version,
            ?provenance,
            "static-registry cargo-closure crate tarball obtained"
        );
        // Normalize the tarball to a byte-stable, source-only form (strip the
        // git-HEAD-derived `.cargo_vcs_info.json`, re-gzip with a fixed header)
        // and refuse a source change under an already-published version.
        // `opts.out` still holds the PREVIOUS complete served tree during this
        // staged emit (the flip runs after the closure returns), so the prior
        // `.crate` is the immutability reference; `cksum` is the normalized
        // tarball's checksum for the sparse-index line.
        let served_crate = opts
            .out
            .join("cargo")
            .join("crates")
            .join(&c.name)
            .join(crate_artifact_filename(&c.name, &version));
        let cksum = crate::crate_tarball::finalize_crate_tarball(
            &crate_file,
            &c.name,
            &version,
            served_crate.exists().then_some(served_crate.as_path()),
        )?;
        let dest = cargo_dir.join("crates").join(&c.name);
        std::fs::create_dir_all(&dest)?;
        std::fs::copy(&crate_file, dest.join(crate_artifact_filename(&c.name, &version)))?;

        let idx = cargo_dir.join(cargo_index_path(&c.name));
        if let Some(parent) = idx.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = Command::new("python3")
            .arg(&render)
            .env("NAME", &c.name)
            .env("VERSION", &version)
            .env("CKSUM", &cksum)
            .env("CRATE", &crate_file)
            .output()
            .with_context(|| format!("render index line for {}", c.name))?;
        if !line.status.success() {
            anyhow::bail!(
                "index-line render failed for {}: {}",
                c.name,
                String::from_utf8_lossy(&line.stderr).trim()
            );
        }
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&idx)
            .with_context(|| format!("open index {}", idx.display()))?;
        f.write_all(&line.stdout)?;
    }

    // Packaging done: stamp the FINAL base URL into config.json.
    drop(server);
    std::fs::write(cargo_dir.join("config.json"), render_cargo_config_json(&opts.base_url))?;
    Ok(())
}

/// Build the Python SDK sdist (uv) and render a PEP-503 simple tree.
fn emit_pypi(opts: &EmitOptions, staging: &Path, target: &str) -> Result<()> {
    let project = opts.workspace_root.join("libs/streamlib-python");
    let pypi = staging.join("pypi");
    let packages = pypi.join("packages");
    std::fs::create_dir_all(&packages)?;
    // PEP 440 spells the dev train `<base>.devN`.
    let py_version = match opts.dev {
        Some(n) => format!("{}.dev{n}", target.split("-dev.").next().unwrap_or(target)),
        None => target.to_string(),
    };
    let out = Command::new("uv")
        .args(["build", "--sdist", "--out-dir"])
        .arg(&packages)
        .current_dir(&project)
        .output()
        .context("uv build --sdist (is `uv` installed?)")?;
    if !out.status.success() {
        anyhow::bail!("uv build --sdist failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    // Enumerate the produced sdist(s) and render the simple index.
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&packages)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("gz") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let sha = sha256_hex(&path)?;
            files.push((name, sha));
        }
    }
    files.sort();
    let simple = pypi.join("simple").join("streamlib");
    std::fs::create_dir_all(&simple)?;
    std::fs::write(simple.join("index.html"), render_pypi_simple_project("streamlib", &files))?;
    std::fs::write(
        pypi.join("simple").join("index.html"),
        render_pypi_simple_root(&["streamlib".to_string()]),
    )?;
    tracing::info!(version = %py_version, "emitted pypi-simple tree");
    Ok(())
}

/// Pack the Deno SDK (deno pack) and render an npm packument.
fn emit_npm(opts: &EmitOptions, staging: &Path, target: &str) -> Result<()> {
    let project = opts.workspace_root.join("libs/streamlib-deno");
    let name = "@tatolab/streamlib-deno";
    // Layout that actually serves statically: npm clients GET
    // `<base>/npm/@scope%2fname` (the packument), which a static server
    // decodes to the path `npm/@scope/name` — so the packument must be a
    // FILE at exactly that path. Tarballs live under the non-conflicting
    // sibling `npm/tarballs/` (`dist.tarball` URLs match — see
    // `render_npm_packument`).
    let tarballs_dir = staging.join("npm").join("tarballs");
    std::fs::create_dir_all(&tarballs_dir)?;
    let tgz = tarballs_dir.join(format!("streamlib-deno-{target}.tgz"));
    // Regenerate the escalate wire vocabulary so the artifact is current.
    let setup = Command::new("deno")
        .args(["task", "setup"])
        .current_dir(&project)
        .status()
        .context("deno task setup (is `deno` installed?)")?;
    if !setup.success() {
        anyhow::bail!("deno task setup (codegen) failed");
    }
    let out = Command::new("deno")
        .args(["pack", "--set-version", target, "--allow-dirty", "-o"])
        .arg(&tgz)
        .current_dir(&project)
        .output()
        .context("deno pack (needs Deno >= 2.8)")?;
    if !out.status.success() {
        anyhow::bail!("deno pack failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let tarball_filename = tgz.file_name().unwrap().to_string_lossy().into_owned();
    // npm integrity fields — shasum is sha1, integrity is base64(sha512).
    let bytes = std::fs::read(&tgz)?;
    let shasum = {
        use sha1::Digest as _;
        format!("{:x}", sha1::Sha1::digest(&bytes))
    };
    let integrity = {
        use sha2::Digest as _;
        let d = sha2::Sha512::digest(&bytes);
        base64_encode(&d)
    };
    let packument = render_npm_packument(
        name,
        &opts.base_url,
        &[NpmVersion {
            version: target.to_string(),
            tarball_filename,
            shasum_sha1_hex: shasum,
            integrity_sha512_b64: integrity,
        }],
    );
    let packument_path = staging.join("npm").join(name);
    if let Some(parent) = packument_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&packument_path, packument)?;
    Ok(())
}

/// Assemble each workspace package into a `.slpkg`, write it into the `file://`
/// generic store, and write the [`ReleaseManifest`] LAST (the atomicity flip
/// within the staging tree; the whole staging tree then flips atomically).
fn emit_slpkg_and_manifest(
    opts: &EmitOptions,
    staging: &Path,
    target: &str,
    org: &str,
) -> Result<()> {
    let slpkg_dir = staging.join("slpkg");
    std::fs::create_dir_all(&slpkg_dir)?;
    // The registry client is rooted at the tree root and writes under `slpkg/`.
    let config = RegistryConfig {
        base_url: format!("file://{}", staging.display()),
    };

    let packages_dir = opts.workspace_root.join("packages");
    let mut package_members: Vec<ReleaseManifestMember> = Vec::new();
    // Registry-wide processor index accumulated across every package — the
    // node-palette aggregate, written LAST (after the release manifest).
    let mut catalog_index: Vec<CatalogIndexLine> = Vec::new();
    if packages_dir.is_dir() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&packages_dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();

        // Resolution universe for external schema refs: every package being
        // published, with its version + manifest. Built up front so an
        // importer's catalog resolves a dep's version regardless of emit
        // order (alphabetical `entries` doesn't respect dependency order).
        let siblings = build_sibling_versions(&entries)
            .context("building the catalog resolution universe from packages/")?;

        for pkg_dir in &entries {
            let yaml = pkg_dir.join("streamlib.yaml");
            if !yaml.is_file() {
                continue;
            }
            let (pkg_ref, version, bytes) = assemble_slpkg_bytes(pkg_dir)?;
            let semver: SemVer = version
                .parse()
                .with_context(|| format!("package {} version `{version}` is not semver", pkg_ref))?;
            RegistryClient::new(&config)
                .upload_slpkg(&pkg_ref, semver, &bytes)
                .map_err(|e| anyhow::anyhow!("upload {}: {e}", pkg_ref))?;
            package_members
                .push(ReleaseManifestMember::new(pkg_ref.to_string(), version.clone()));

            // Publish-time catalog: per-package `<name>.catalog.json` + the
            // JTDs this package owns, written into the same version dir as the
            // `.slpkg`. Accumulate the per-processor index lines for the
            // registry-wide aggregate.
            let artifacts = build_package_catalog(pkg_dir, &siblings)
                .with_context(|| format!("building catalog for {pkg_ref}"))?;
            write_package_catalog(&slpkg_dir, &artifacts)
                .with_context(|| format!("writing catalog for {pkg_ref}"))?;
            catalog_index.extend(artifacts.index_lines);
        }
    }

    // Crate closure members at their ACTUAL manifest versions — the same
    // correct-by-construction rule as `crate_artifact_filename`: the manifest
    // records what the tree really holds; a dev emit bumps manifests first.
    let closure = crate::compute_release_closure(&opts.workspace_root)?;
    let crate_members: Vec<ReleaseManifestMember> = closure
        .crates
        .iter()
        .map(|c| ReleaseManifestMember::new(c.name.clone(), c.version.clone()))
        .collect();

    let mut manifest = ReleaseManifest::new(target.to_string(), crate_members);
    if opts.ecosystems.pypi {
        manifest.python = Some(target.to_string());
    }
    if opts.ecosystems.npm {
        manifest.deno = Some(target.to_string());
    }
    manifest.packages = package_members;

    // Written LAST — the completion marker.
    RegistryClient::new(&config)
        .upload_release_manifest(org, &manifest)
        .map_err(|e| anyhow::anyhow!("upload release manifest: {e}"))?;

    // Registry-wide catalog aggregate, written AFTER the per-package catalog
    // files and the release manifest, at a STAGING-relative path so it rides
    // the same atomic flip (`build_and_flip`) — a `file://` reader never
    // observes the aggregate without the release it describes. Ordering it
    // last keeps the HTTP-direct-write intuition (index after members)
    // consistent. The CI static-registry emit smoke asserts the aggregate +
    // a per-package catalog exist in the emitted tree, locking this
    // staging-relativity through the real emit path.
    let index_path = staging.join(CATALOG_INDEX_PATH);
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&index_path, render_catalog_index_ndjson(&catalog_index))
        .with_context(|| format!("write {}", index_path.display()))?;
    Ok(())
}

/// Write one package's catalog artifacts under the tree's `slpkg/` root:
/// `<name>.catalog.json` beside the `.slpkg` in the FULL-version dir
/// (`slpkg/<name>/<version>/`, matching per-package catalog fetch by exact
/// published version), and each owned schema's JTD under the **release-core**
/// version dir (`slpkg/<name>/<release-core>/schemas/`) — schema idents are
/// release-core by invariant, so the reader derives the JTD path from the
/// ident's projected version. A `-dev.N` publisher whose JTDs sat under the
/// full prerelease dir would be silently unfetchable.
pub fn write_package_catalog(
    slpkg_dir: &Path,
    artifacts: &crate::catalog::PackageCatalogArtifacts,
) -> Result<()> {
    let name = artifacts.catalog.package.name.as_str();
    let version = artifacts.catalog.version.to_string();
    let ver_dir = slpkg_dir.join(name).join(&version);
    std::fs::create_dir_all(&ver_dir).with_context(|| format!("create {}", ver_dir.display()))?;

    let catalog_path = ver_dir.join(streamlib_idents::package_catalog_file_name(name));
    let catalog_json = serde_json::to_vec_pretty(&artifacts.catalog)
        .context("serialize package catalog JSON")?;
    std::fs::write(&catalog_path, catalog_json)
        .with_context(|| format!("write {}", catalog_path.display()))?;

    if !artifacts.schema_jtd.is_empty() {
        // Release-core dir — the version basis of every SchemaIdent.
        let jtd_ver_dir = slpkg_dir
            .join(name)
            .join(artifacts.catalog.version.release_core().to_string());
        let schemas_dir = jtd_ver_dir.join("schemas");
        std::fs::create_dir_all(&schemas_dir)
            .with_context(|| format!("create {}", schemas_dir.display()))?;
        for jtd in &artifacts.schema_jtd {
            let path = schemas_dir.join(schema_jtd_file_name(&jtd.type_name));
            let bytes = serde_json::to_vec_pretty(&jtd.json)
                .with_context(|| format!("serialize JTD for {}", jtd.type_name))?;
            std::fs::write(&path, bytes)
                .with_context(|| format!("write {}", path.display()))?;
        }
    }
    Ok(())
}

/// Assemble a package dir into `.slpkg` bytes, returning its
/// [`PackageRef`], version, and the bytes.
fn assemble_slpkg_bytes(pkg_dir: &Path) -> Result<(PackageRef, String, Vec<u8>)> {
    let tmp = tempfile::Builder::new()
        .prefix("slpkg-emit-")
        .suffix(".slpkg")
        .tempfile()
        .context("create temp .slpkg")?;
    let outcome = crate::assemble_artifact(
        pkg_dir,
        &crate::AssembleTarget::Slpkg(tmp.path().to_path_buf()),
        &crate::AssembleOptions {
            no_build: false,
            profile: crate::CargoProfile::Release,
            path_deps: crate::PathDepPolicy::RejectPathPatches,
        },
        &(),
    )?;
    let bytes = std::fs::read(tmp.path()).context("read assembled .slpkg")?;
    // `AssembleOutcome.package_name` is the bare name; the org comes from the
    // manifest's `package:` block.
    #[derive(serde::Deserialize)]
    struct Pkg {
        org: String,
    }
    #[derive(serde::Deserialize)]
    struct Yaml {
        package: Pkg,
    }
    let yaml_body = std::fs::read_to_string(pkg_dir.join("streamlib.yaml"))
        .with_context(|| format!("read {}/streamlib.yaml", pkg_dir.display()))?;
    let parsed: Yaml = serde_yaml::from_str(&yaml_body)
        .with_context(|| format!("parse {}/streamlib.yaml", pkg_dir.display()))?;
    let pkg_ref = PackageRef::new(
        streamlib_idents::Org::new(&parsed.package.org).map_err(|e| anyhow::anyhow!("{e}"))?,
        streamlib_idents::Package::new(&outcome.package_name)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    Ok((pkg_ref, outcome.package_version, bytes))
}

/// Minimal standard base64 (no external dep) for the npm `integrity` field.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn index_path_matches_cargo_sparse_grammar() {
        assert_eq!(cargo_index_path("a"), "1/a");
        assert_eq!(cargo_index_path("ab"), "2/ab");
        assert_eq!(cargo_index_path("abc"), "3/a/abc");
        assert_eq!(cargo_index_path("serde"), "se/rd/serde");
        assert_eq!(cargo_index_path("vulkanalia"), "vu/lk/vulkanalia");
        assert_eq!(cargo_index_path("vulkanalia-sys"), "vu/lk/vulkanalia-sys");
        // Uppercase is lowercased for the path (cargo index is lowercase).
        assert_eq!(cargo_index_path("Serde"), "se/rd/serde");
    }

    #[test]
    fn config_json_carries_templated_dl_and_base_url() {
        let cfg = render_cargo_config_json("http://127.0.0.1:8799");
        assert!(cfg.contains("\"dl\":\"http://127.0.0.1:8799/cargo/crates/{crate}/{crate}-{version}.crate\""));
        assert!(cfg.contains("\"api\":\"http://127.0.0.1:8799/cargo\""));
        assert!(cfg.ends_with('\n'));
        // Trailing slash on the base is normalized away.
        let cfg2 = render_cargo_config_json("http://127.0.0.1:8799/");
        assert_eq!(cfg, cfg2);
    }

    #[test]
    fn pypi_simple_project_lists_files_with_hash_fragment() {
        let html = render_pypi_simple_project(
            "streamlib",
            &[("streamlib-0.5.1.tar.gz".into(), "abc123".into())],
        );
        assert!(html.contains("Links for streamlib"));
        assert!(html.contains("streamlib-0.5.1.tar.gz#sha256=abc123"));
    }

    #[test]
    fn npm_packument_points_tarball_at_mount_and_sets_latest() {
        let doc = render_npm_packument(
            "@tatolab/streamlib-deno",
            "http://127.0.0.1:8799",
            &[NpmVersion {
                version: "0.5.1".into(),
                tarball_filename: "streamlib-deno-0.5.1.tgz".into(),
                shasum_sha1_hex: "deadbeef".into(),
                integrity_sha512_b64: "AAAA".into(),
            }],
        );
        let v: serde_json::Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(v["dist-tags"]["latest"], "0.5.1");
        assert_eq!(
            v["versions"]["0.5.1"]["dist"]["tarball"],
            "http://127.0.0.1:8799/npm/tarballs/streamlib-deno-0.5.1.tgz"
        );
        assert_eq!(v["versions"]["0.5.1"]["dist"]["integrity"], "sha512-AAAA");
    }

    /// The npm layout must actually serve statically: the packument is a FILE
    /// at `npm/@scope/name` (what a client's `GET <base>/npm/@scope%2fname`
    /// decodes to on a static server), and tarballs live at a sibling path
    /// that can NEVER collide with it. The historical bug: writing the
    /// packument at `npm/@scope/name/index.json` makes `npm/@scope/name` a
    /// DIRECTORY — a static server answers the packument GET with an HTML
    /// listing and resolution breaks.
    #[test]
    fn npm_static_layout_packument_is_a_file_and_tarballs_dont_collide() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path();
        let name = "@tatolab/streamlib-deno";

        // Mirror emit_npm's layout without the deno toolchain.
        let tarballs_dir = staging.join("npm").join("tarballs");
        std::fs::create_dir_all(&tarballs_dir).unwrap();
        std::fs::write(tarballs_dir.join("streamlib-deno-0.5.1.tgz"), b"tgz").unwrap();
        let packument_path = staging.join("npm").join(name);
        std::fs::create_dir_all(packument_path.parent().unwrap()).unwrap();
        let doc = render_npm_packument(
            name,
            "http://127.0.0.1:8799",
            &[NpmVersion {
                version: "0.5.1".into(),
                tarball_filename: "streamlib-deno-0.5.1.tgz".into(),
                shasum_sha1_hex: "deadbeef".into(),
                integrity_sha512_b64: "AAAA".into(),
            }],
        );
        std::fs::write(&packument_path, &doc).unwrap();

        // The packument path IS a file (not a directory) …
        assert!(packument_path.is_file(), "packument must be a plain file");
        // … and the tarball URL path maps to an existing file that does not
        // collide with the packument path.
        let v: serde_json::Value = serde_json::from_str(&doc).unwrap();
        let tarball_url = v["versions"]["0.5.1"]["dist"]["tarball"].as_str().unwrap();
        let rel = tarball_url.strip_prefix("http://127.0.0.1:8799/").unwrap();
        let tarball_path = staging.join(rel);
        assert!(tarball_path.is_file(), "dist.tarball must map to a real file");
        assert!(
            !tarball_path.starts_with(&packument_path),
            "tarball path must not nest under the packument path"
        );
    }

    /// Locks the correct-by-construction version rule for the closure emit:
    /// the artifact filename always follows the crate's ACTUAL manifest
    /// version — a dev emit bumps manifests first, never re-derives.
    /// Mentally revert emit_cargo_closure to stamp `target` when
    /// `opts.dev.is_some()`: `cargo package` still writes
    /// `{name}-{manifest_version}.crate` and the emit dies at the ensure!.
    #[test]
    fn crate_artifact_filename_follows_manifest_version() {
        assert_eq!(
            crate_artifact_filename("streamlib-macros", "0.5.1"),
            "streamlib-macros-0.5.1.crate"
        );
        assert_eq!(
            crate_artifact_filename("streamlib-macros", "0.5.1-dev.3"),
            "streamlib-macros-0.5.1-dev.3.crate"
        );
    }

    #[test]
    fn publish_staged_tree_fresh_target_is_atomic_rename() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join(".staging");
        let served = root.path().join("served");
        std::fs::create_dir_all(staging.join("cargo")).unwrap();
        std::fs::write(staging.join("marker"), b"v2").unwrap();

        publish_staged_tree(&staging, &served).unwrap();
        assert!(!staging.exists(), "staging consumed by the rename");
        assert_eq!(std::fs::read_to_string(served.join("marker")).unwrap(), "v2");
        assert!(served.join("cargo").is_dir());
    }

    /// The set of *complete* release versions under a served tree's channel —
    /// what a `file://` [`ReleaseManifest`] consumer's `list_release_versions`
    /// observes. A release subdir counts only when its `manifest.json` (the
    /// completion marker, written LAST) is present.
    fn complete_releases(base: &Path) -> Vec<String> {
        let dir = base.join("slpkg/streamlib-release");
        let mut vs: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("manifest.json").is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        vs.sort();
        vs
    }

    /// The mid-publish window guarantee, exercised through the REAL seam
    /// (`build_and_flip`, the exact path `emit_static_registry` runs) with a
    /// CONCURRENT reader: while a new release is built, a reader of the
    /// served tree only ever observes a complete release set — the old
    /// `{0.5.0}` or the new `{0.5.1}` — never a partial or mixed one.
    ///
    /// Reader protocol (the file:// consumer shape): list the release
    /// channel, then read each listed version's manifest. A manifest read
    /// that fails is tolerated ONLY when a re-list shows the version gone
    /// (the atomic flip raced between the two syscalls); a version that
    /// stays listed without its manifest is a partial tree → panic.
    /// Mentally revert `build_and_flip`/`publish_staged_tree` to a
    /// copy-into-served loop: the half-copied `0.5.1` dir is listed without
    /// its manifest (manifest is written last) and stays listed → panic.
    #[test]
    #[cfg(target_os = "linux")] // relies on the gapless RENAME_EXCHANGE flip
    fn window_concurrent_reader_never_observes_partial_release() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("served");

        let write_release = |staging: &Path, ver: &str, slow: bool| -> Result<()> {
            std::fs::create_dir_all(staging.join("cargo/crates"))?;
            for i in 0..30 {
                std::fs::write(
                    staging.join(format!("cargo/crates/payload-{i}.bin")),
                    vec![0u8; 1024],
                )?;
                if slow {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            // Release manifest written LAST — the completion marker.
            let rel = staging.join("slpkg/streamlib-release").join(ver);
            std::fs::create_dir_all(&rel)?;
            std::fs::write(
                rel.join("manifest.json"),
                format!("{{\"release_version\":\"{ver}\"}}"),
            )?;
            Ok(())
        };

        // Old complete release, published through the real seam.
        build_and_flip(&out, |staging| write_release(staging, "0.5.0", false)).unwrap();

        let list = |base: &Path| -> Option<Vec<String>> {
            let rd = std::fs::read_dir(base.join("slpkg/streamlib-release")).ok()?;
            let mut vs: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            vs.sort();
            Some(vs)
        };

        let stop = Arc::new(AtomicBool::new(false));
        let out_r = out.clone();
        let stop_r = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut saw_old = false;
            let mut saw_new = false;
            while !stop_r.load(Ordering::Relaxed) {
                // With RENAME_EXCHANGE the served path never vanishes.
                let listed = list(&out_r).expect("release channel must never vanish");
                match listed.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                    ["0.5.0"] | ["0.5.1"] => {}
                    ref other => panic!("non-atomic release set observed: {other:?}"),
                }
                for v in &listed {
                    let manifest =
                        out_r.join("slpkg/streamlib-release").join(v).join("manifest.json");
                    match std::fs::read_to_string(&manifest) {
                        Ok(body) => {
                            // Never a torn/partial file: staged writes are
                            // invisible until the flip.
                            let parsed: serde_json::Value =
                                serde_json::from_str(&body).expect("manifest must be whole JSON");
                            assert_eq!(parsed["release_version"], v.as_str());
                            if v == "0.5.0" {
                                saw_old = true;
                            } else {
                                saw_new = true;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Tolerable ONLY if the flip raced between list
                            // and read — the version must now be gone.
                            let relisted = list(&out_r).expect("channel must never vanish");
                            assert!(
                                !relisted.contains(v),
                                "version {v} listed without its manifest and still \
                                 listed on re-list — PARTIAL tree exposed"
                            );
                        }
                        Err(e) => panic!("manifest read failed: {e}"),
                    }
                }
            }
            (saw_old, saw_new)
        });

        // New release built + flipped through the real seam, slowly, while
        // the reader spins.
        build_and_flip(&out, |staging| write_release(staging, "0.5.1", true)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        stop.store(true, Ordering::Relaxed);
        let (saw_old, saw_new) = reader.join().unwrap();

        assert!(saw_old, "reader must have observed the old release during staging");
        assert!(saw_new, "reader must have observed the flipped-in new release");
        assert_eq!(complete_releases(&out), vec!["0.5.1".to_string()]);
    }

    /// A build that CRASHES (errors) mid-emit must leave the served tree
    /// untouched and clean up its staging dir — the failed-publish half of
    /// the window guarantee.
    #[test]
    fn crashed_build_leaves_served_tree_untouched() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("served");
        std::fs::create_dir_all(out.join("slpkg/streamlib-release/0.5.0")).unwrap();
        std::fs::write(
            out.join("slpkg/streamlib-release/0.5.0/manifest.json"),
            b"{\"release_version\":\"0.5.0\"}",
        )
        .unwrap();

        let err = build_and_flip(&out, |staging| {
            // Write half a release, then die before the manifest.
            std::fs::create_dir_all(staging.join("slpkg/streamlib-release/0.5.1"))?;
            std::fs::write(staging.join("slpkg/streamlib-release/0.5.1/partial.bin"), b"x")?;
            anyhow::bail!("simulated mid-emit crash")
        })
        .unwrap_err();
        assert!(err.to_string().contains("simulated mid-emit crash"));

        // Served tree: exactly the old complete release, nothing else.
        assert_eq!(complete_releases(&out), vec!["0.5.0".to_string()]);
        assert!(!out.join("slpkg/streamlib-release/0.5.1").exists());
        // No staging remnant left beside the served tree.
        let remnants: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".staging."))
            .collect();
        assert!(remnants.is_empty(), "staging remnant left behind: {remnants:?}");
    }

    /// Locks the gapless replace primitive: `publish_staged_tree` onto an
    /// EXISTING served tree swaps in the new one and drops the old, with no
    /// intermediate rename-aside gap (Linux `renameat2(RENAME_EXCHANGE)`).
    #[test]
    fn publish_staged_tree_replaces_existing_atomically() {
        let root = tempfile::tempdir().unwrap();
        let served = root.path().join("served");
        std::fs::create_dir_all(&served).unwrap();
        std::fs::write(served.join("MARKER"), b"old").unwrap();
        std::fs::write(served.join("only-in-old"), b"1").unwrap();

        let staging = sibling_temp(&served, "staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("MARKER"), b"new").unwrap();
        std::fs::write(staging.join("only-in-new"), b"1").unwrap();

        publish_staged_tree(&staging, &served).unwrap();

        assert_eq!(std::fs::read_to_string(served.join("MARKER")).unwrap(), "new");
        assert!(served.join("only-in-new").is_file());
        assert!(!served.join("only-in-old").exists(), "old tree fully replaced");
        assert!(!staging.exists(), "staging (old tree after swap) removed");
    }
}
