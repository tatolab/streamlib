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
    PackageRef, RegistryClient, RegistryConfig, ReleaseManifest, ReleaseManifestMember, SemVer,
};

/// Which ecosystems an emit run produces. Each maps to its native anonymous
/// read transport (see the module docs).
#[derive(Debug, Clone)]
pub struct EmitEcosystems {
    /// Emit the vulkanalia fork cargo tree (the daemon-free bootstrap the CI
    /// resolve needs). Delegates to `scripts/gitea/emit-static-fork.sh`.
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

/// One dependency entry in a cargo sparse-index line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CargoIndexDep {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    /// `"normal" | "build" | "dev"`.
    pub kind: String,
    /// The index URL the dep resolves from. Omitted means "this registry" (a
    /// same-registry dep, e.g. a fork sibling) — cargo treats an absent key as
    /// the current registry, matching Gitea's index output; [`CRATES_IO_INDEX`]
    /// means crates.io.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// The published crate name when the dep is locally renamed via `package`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

/// One version line of a cargo sparse index (serialized as a single NDJSON row).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CargoIndexLine {
    pub name: String,
    pub vers: String,
    pub deps: Vec<CargoIndexDep>,
    pub cksum: String,
    pub features: std::collections::BTreeMap<String, Vec<String>>,
    pub yanked: bool,
}

impl CargoIndexLine {
    /// Serialize to the exact NDJSON byte shape cargo expects (compact JSON,
    /// one object, trailing newline appended by the caller when writing a file).
    pub fn to_ndjson(&self) -> String {
        serde_json::to_string(self).expect("CargoIndexLine serializes")
    }
}

/// A bare `X.Y.Z` cargo version req means `^X.Y.Z`; operators/ranges pass
/// through unchanged.
pub fn normalize_cargo_req(version: &str) -> String {
    let v = version.trim();
    if v.is_empty() {
        return "*".to_string();
    }
    let first = v.chars().next().unwrap();
    if v == "*" || v.contains(',') || matches!(first, '^' | '~' | '=' | '<' | '>' | '*') {
        v.to_string()
    } else {
        format!("^{v}")
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
        let tarball = format!("{base}/npm/{}/-/{}", name, v.tarball_filename);
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
    // `served` exists → gapless replace.
    #[cfg(target_os = "linux")]
    {
        exchange_paths(staging, served).with_context(|| {
            format!("atomically swap {} ⇄ {}", staging.display(), served.display())
        })?;
        // After the exchange, `staging` holds the old tree.
        std::fs::remove_dir_all(staging).ok();
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
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

/// The registry org the tree is emitted under (`GITEA_ORG`, default `tatolab`).
fn registry_org() -> String {
    std::env::var("GITEA_ORG")
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

    let staging = sibling_temp(&opts.out, "staging");
    // Best-effort clean of any prior staging remnant, then a fresh dir.
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;

    let result = (|| -> Result<()> {
        if opts.ecosystems.cargo_fork {
            emit_cargo_fork(opts, &staging)?;
        }
        if opts.ecosystems.cargo_closure {
            emit_cargo_closure(opts, &staging, &target)?;
        }
        if opts.ecosystems.pypi {
            emit_pypi(opts, &staging, &target)?;
        }
        if opts.ecosystems.npm {
            emit_npm(opts, &staging, &target)?;
        }
        if opts.ecosystems.slpkg {
            emit_slpkg_and_manifest(opts, &staging, &target, &org)?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            publish_staged_tree(&staging, &opts.out)?;
            tracing::info!(out = %opts.out.display(), release = %target, "static registry emitted");
            Ok(())
        }
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
        .join("scripts/gitea/emit-static-fork.sh");
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
fn emit_cargo_closure(opts: &EmitOptions, staging: &Path, target: &str) -> Result<()> {
    let closure = crate::compute_release_closure(&opts.workspace_root)?;
    let cargo_dir = staging.join("cargo");
    std::fs::create_dir_all(cargo_dir.join("crates"))?;
    // config.json (fork emit already wrote one; overwrite is identical bytes).
    std::fs::write(cargo_dir.join("config.json"), render_cargo_config_json(&opts.base_url))?;
    let render = opts
        .workspace_root
        .join("scripts/gitea/render_cargo_index_line.py");

    for c in &closure.crates {
        // A --dev publish stamps every closure crate at the target version.
        let version = if opts.dev.is_some() { target.to_string() } else { c.version.clone() };
        let out = Command::new("cargo")
            .args(["package", "--no-verify", "--allow-dirty", "-p", &c.name])
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
        let crate_file = opts
            .workspace_root
            .join("target/package")
            .join(format!("{}-{}.crate", c.name, version));
        anyhow::ensure!(
            crate_file.is_file(),
            "expected packaged crate at {}",
            crate_file.display()
        );
        let dest = cargo_dir.join("crates").join(&c.name);
        std::fs::create_dir_all(&dest)?;
        std::fs::copy(&crate_file, dest.join(format!("{}-{}.crate", c.name, version)))?;

        let cksum = sha256_hex(&crate_file)?;
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
    let npm_dir = staging.join("npm").join(name).join("-");
    std::fs::create_dir_all(&npm_dir)?;
    let tgz = npm_dir.join(format!("streamlib-deno-{target}.tgz"));
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
    std::fs::write(staging.join("npm").join(name).join("index.json"), packument)?;
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
    let config = RegistryConfig {
        base_url: format!("file://{}", slpkg_dir.display()),
        token: None,
    };

    let packages_dir = opts.workspace_root.join("packages");
    let mut package_members: Vec<ReleaseManifestMember> = Vec::new();
    if packages_dir.is_dir() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&packages_dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for pkg_dir in entries {
            let yaml = pkg_dir.join("streamlib.yaml");
            if !yaml.is_file() {
                continue;
            }
            let (pkg_ref, version, bytes) = assemble_slpkg_bytes(&pkg_dir)?;
            let semver: SemVer = version
                .parse()
                .with_context(|| format!("package {} version `{version}` is not semver", pkg_ref))?;
            RegistryClient::new(&config)
                .upload_slpkg(&pkg_ref, semver, &bytes)
                .map_err(|e| anyhow::anyhow!("upload {}: {e}", pkg_ref))?;
            package_members
                .push(ReleaseManifestMember::new(pkg_ref.to_string(), version.clone()));
        }
    }

    // Crate closure members at the target version.
    let closure = crate::compute_release_closure(&opts.workspace_root)?;
    let crate_members: Vec<ReleaseManifestMember> = closure
        .crates
        .iter()
        .map(|c| {
            let v = if opts.dev.is_some() { target.to_string() } else { c.version.clone() };
            ReleaseManifestMember::new(c.name.clone(), v)
        })
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
    // Parse org/name from the package name (`@org/name`).
    let (org, name) = outcome
        .package_name
        .trim_start_matches('@')
        .split_once('/')
        .with_context(|| format!("package name `{}` is not `@org/name`", outcome.package_name))?;
    let pkg_ref = PackageRef::new(
        streamlib_idents::Org::new(org).map_err(|e| anyhow::anyhow!("{e}"))?,
        streamlib_idents::Package::new(name).map_err(|e| anyhow::anyhow!("{e}"))?,
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
    fn req_normalization_adds_caret_only_to_bare_versions() {
        assert_eq!(normalize_cargo_req("0.35.0"), "^0.35.0");
        assert_eq!(normalize_cargo_req("^0.35.0"), "^0.35.0");
        assert_eq!(normalize_cargo_req("=1.0.0"), "=1.0.0");
        assert_eq!(normalize_cargo_req("~1.2"), "~1.2");
        assert_eq!(normalize_cargo_req(">=1, <2"), ">=1, <2");
        assert_eq!(normalize_cargo_req("*"), "*");
        assert_eq!(normalize_cargo_req(""), "*");
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
    fn index_line_ndjson_shape_is_a_single_compact_object() {
        let line = CargoIndexLine {
            name: "vulkanalia".into(),
            vers: "0.35.0".into(),
            deps: vec![
                CargoIndexDep {
                    name: "vulkanalia-sys".into(),
                    req: "^0.35.0".into(),
                    features: vec![],
                    optional: false,
                    default_features: true,
                    target: None,
                    kind: "normal".into(),
                    registry: None, // same registry → null
                    package: None,
                },
                CargoIndexDep {
                    name: "bitflags".into(),
                    req: "^2.0".into(),
                    features: vec![],
                    optional: false,
                    default_features: true,
                    target: None,
                    kind: "normal".into(),
                    registry: Some(CRATES_IO_INDEX.into()),
                    package: None,
                },
            ],
            cksum: "0".repeat(64),
            features: Default::default(),
            yanked: false,
        };
        let ndjson = line.to_ndjson();
        assert!(!ndjson.contains('\n'), "index line must be a single row");
        let back: serde_json::Value = serde_json::from_str(&ndjson).unwrap();
        assert_eq!(back["name"], "vulkanalia");
        assert_eq!(back["deps"][0]["registry"], serde_json::Value::Null);
        assert_eq!(back["deps"][1]["registry"], CRATES_IO_INDEX);
        assert_eq!(back["yanked"], false);
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
            "http://127.0.0.1:8799/npm/@tatolab/streamlib-deno/-/streamlib-deno-0.5.1.tgz"
        );
        assert_eq!(v["versions"]["0.5.1"]["dist"]["integrity"], "sha512-AAAA");
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

    /// The mid-publish window guarantee (#1240): while the NEW release tree is
    /// being built (into a staging dir), the served tree stays the OLD
    /// *complete* release — no partial/mixed state ever appears in the served
    /// path — and the flip to the new release is a single atomic operation.
    ///
    /// Deterministic (single-threaded): after every incremental staging write
    /// we assert the served tree is still exactly `{0.5.0}`, proving staging
    /// never leaks into served. Mentally revert `publish_staged_tree` to a
    /// copy-into-served loop and the pre-flip assertions fail (served would
    /// gain a half-written `0.5.1`); mentally revert the manifest-last ordering
    /// and the "complete" filter would admit an incomplete `0.5.1`.
    #[test]
    fn publish_staged_tree_keeps_served_complete_through_the_window() {
        let root = tempfile::tempdir().unwrap();
        let served = root.path().join("served");
        std::fs::create_dir_all(served.join("slpkg/streamlib-release/0.5.0")).unwrap();
        std::fs::write(
            served.join("slpkg/streamlib-release/0.5.0/manifest.json"),
            b"{\"release_version\":\"0.5.0\"}",
        )
        .unwrap();
        assert_eq!(complete_releases(&served), vec!["0.5.0".to_string()]);

        // Build the NEW release into staging incrementally — the "window."
        let staging = sibling_temp(&served, "staging");
        std::fs::create_dir_all(staging.join("slpkg/streamlib-release/0.5.1")).unwrap();
        std::fs::create_dir_all(staging.join("cargo/crates")).unwrap();
        for i in 0..20 {
            std::fs::write(staging.join(format!("cargo/crates/part-{i}.bin")), b"x").unwrap();
            // Throughout staging, served is untouched and complete.
            assert_eq!(
                complete_releases(&served),
                vec!["0.5.0".to_string()],
                "served tree changed during the staging window (partial exposure)"
            );
        }
        // ReleaseManifest written LAST into staging (the completion marker).
        std::fs::write(
            staging.join("slpkg/streamlib-release/0.5.1/manifest.json"),
            b"{\"release_version\":\"0.5.1\"}",
        )
        .unwrap();
        // Still old until the flip.
        assert_eq!(complete_releases(&served), vec!["0.5.0".to_string()]);

        publish_staged_tree(&staging, &served).unwrap();

        // Post-flip: exactly the new release, staging consumed, cargo tree in.
        assert_eq!(complete_releases(&served), vec!["0.5.1".to_string()]);
        assert!(!staging.exists());
        assert!(served.join("cargo/crates/part-0.bin").is_file());
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
