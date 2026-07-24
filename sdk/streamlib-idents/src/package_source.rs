// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Static-file package source client for schema-package resolution.
//!
//! A package source is just a location a package's bytes live at — there is no
//! central registry to log in to. StreamLib's package source is a static
//! `.slpkg` tree, read tokenlessly over `file://` or a dumb HTTP mount; future
//! sources (a GitHub release tree, a mesh peer, an offline mirror) are modeled
//! the same way — another location, not a hosted service.
//!
//! Schema packages (`@tatolab/escalate` and friends) are distributed as
//! source-only `.slpkg`s in the package source tree's generic store under
//! `slpkg/<name>/<version>/<name>.slpkg`. Resolving a by-version dependency by
//! a semver *range* takes two steps:
//!
//! 1. **List** the available concrete versions of the package, then select the
//!    highest one that satisfies the declared range — cargo/npm/pip semantics.
//! 2. **Download** that version's `.slpkg` from the by-version store.
//!
//! Both steps are **tokenless**. The store has no directory-listing query over
//! plain HTTP, so each publish maintains a cargo-sparse-shaped **version
//! index** as a plain file at `slpkg/<name>/index.json` (NDJSON, one
//! `{"name","vers"}` line per version). Over `file://` the list step
//! enumerates the per-version subdirectories directly; over `http(s)://` it
//! reads that index file — one write path, byte-identically consumable both
//! ways.
//!
//! The `base_url` points at the **tree root** (the directory holding `slpkg/`,
//! `cargo/`, `pypi/`, `npm/`, `catalog/`) — the single package source location a
//! consumer configures. `file://<root>` is the hermetic local-mirror / test /
//! offline transport; `http(s)://…` is a static HTTP mount. Publishing is
//! `file://`-only (an emit writes the tree; a static HTTP mount is read-only).

use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::ident::PackageRef;
use crate::release::ReleaseManifest;
use crate::semver::SemVer;

/// Generic-store "package name" the release manifest is published under.
/// Reserved channel — a `@org/name` package can never collide with it
/// because package names never equal this literal.
pub const RELEASE_MANIFEST_CHANNEL: &str = "streamlib-release";
/// File name of the release manifest inside its per-version directory.
pub const RELEASE_MANIFEST_FILE: &str = "manifest.json";

/// The `slpkg/` subtree the generic store lives under, relative to the tree
/// root the [`PackageSource::base_url`] points at.
const SLPKG_SUBTREE: &str = "slpkg";
/// File name of the per-package version index (NDJSON).
const VERSION_INDEX_FILE: &str = "index.json";

/// Environment variable carrying the package source tree-root URL —
/// `file://<root>` for a local / offline mirror, or `http(s)://…` for a static
/// HTTP mount.
pub const PACKAGE_SOURCE_ENV: &str = "STREAMLIB_PACKAGE_SOURCE";

/// Environment variable carrying the absolute path of an active `streamlib
/// link` checkout. Set by the build orchestrator on the staged package's
/// `cargo build` when a link is active, so a package's `build.rs` schema-dep
/// codegen resolves a dep present in `<checkout>/packages/<name>` from the
/// checkout (the link dev loop) instead of the by-version store.
/// Unset — the dominant case — leaves resolution byte-identical to before.
/// Read by [`ResolverOptions::from_env`], never on the pure resolution path.
///
/// [`ResolverOptions::from_env`]: crate::ResolverOptions::from_env
pub const LINK_CHECKOUT_ENV: &str = "STREAMLIB_LINK_CHECKOUT";

/// A resolved package source: the tree-root location a consumer resolves
/// versioned `.slpkg`s from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSource {
    /// Tree-root URL — `http(s)://host[:port]` for a static HTTP mount, or
    /// `file:///abs/root` for a local mirror.
    pub base_url: String,
}

impl PackageSource {
    /// Build from [`PACKAGE_SOURCE_ENV`], returning `None` when it is unset — so
    /// a dev / path-only build with no package source configured resolves
    /// without touching the network (a version dep then fails loud with
    /// `PackageSourceNotConfigured`). There is no eager default: an unset env is
    /// "no package source", never a silent fallback to a first-party location.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var(PACKAGE_SOURCE_ENV)
            .ok()
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())?;
        Some(Self { base_url })
    }

    /// Build a config for a local on-disk package source tree rooted at `dir` — the
    /// `file://<canonical-abs-dir>` form a `file://` consumer / publisher uses.
    /// `dir` is canonicalized so the derived channel URLs are absolute and
    /// relocation-stable; a non-existent path is a
    /// [`ResolverError::PackageSourceFetchFailed`].
    pub fn for_local_tree(dir: &Path) -> ResolverResult<Self> {
        let canonical = dir
            .canonicalize()
            .map_err(|e| ResolverError::PackageSourceFetchFailed {
                name: "<local tree>".to_string(),
                detail: format!("canonicalize package source tree dir {} : {e}", dir.display()),
            })?;
        Ok(Self {
            base_url: format!("file://{}", canonical.display()),
        })
    }

    /// The on-disk tree root when [`base_url`](Self::base_url) is a `file://`
    /// URL, else `None` (an `http(s)://` mount has no local root). Consumers
    /// locate the per-ecosystem subtrees (`cargo/`, `npm/`, `pypi/`) under it.
    pub fn local_tree_root(&self) -> Option<PathBuf> {
        self.base_url.strip_prefix("file://").map(PathBuf::from)
    }

    /// The pypi PEP-503 `simple/` index URL derived from the single package
    /// source location — the value `uv` reads as `UV_INDEX`. `file://` and `http(s)://`
    /// both work: uv consumes a PEP-503 `simple/` tree over either transport.
    pub fn pypi_simple_index_url(&self) -> String {
        format!("{}/pypi/simple", self.base_url)
    }

    /// The npm registry URL derived from the single package source location — the
    /// value an `.npmrc` `@tatolab:registry=` scope points at. npm/Deno have no
    /// `file://` registry story, so a `file://` tree must first be served over a
    /// static HTTP mount before this is reachable; the string is derived
    /// uniformly here regardless.
    pub fn npm_registry_url(&self) -> String {
        format!("{}/npm/", self.base_url)
    }

    /// The cargo **sparse** index URL derived from the single package source location
    /// (`sparse+<base>/cargo/`). Valid as a `[source]`-replacement target only
    /// for an `http(s)://` mount — cargo's sparse protocol is HTTP-only, so a
    /// `file://` tree is instead consumed via a `local-registry` reshape.
    pub fn cargo_sparse_index_url(&self) -> String {
        format!("sparse+{}/cargo/", self.base_url)
    }
}

/// Client over a single [`PackageSource`].
pub struct PackageSourceClient<'a> {
    config: &'a PackageSource,
}

impl<'a> PackageSourceClient<'a> {
    pub fn new(config: &'a PackageSource) -> Self {
        Self { config }
    }

    fn is_file_scheme(&self) -> bool {
        self.config.base_url.starts_with("file://")
    }

    /// The on-disk tree root a `file://` base URL points at.
    fn file_root(&self) -> PathBuf {
        PathBuf::from(self.config.base_url.trim_start_matches("file://"))
    }

    /// List every concrete version of `pkg_ref` the package source holds.
    pub fn list_versions(&self, pkg_ref: &PackageRef) -> ResolverResult<Vec<SemVer>> {
        if self.is_file_scheme() {
            self.list_versions_file(pkg_ref)
        } else {
            self.list_versions_http(pkg_ref)
        }
    }

    /// Download the `.slpkg` bytes for an exact `version` of `pkg_ref`.
    /// Returns the bytes plus the canonical URL they came from (recorded in
    /// the lockfile as the resolved source).
    pub fn download_slpkg(
        &self,
        pkg_ref: &PackageRef,
        version: SemVer,
    ) -> ResolverResult<(Vec<u8>, String)> {
        let url = self.download_url(pkg_ref, version);
        let bytes = if self.is_file_scheme() {
            let path = self.download_path(pkg_ref, version);
            std::fs::read(&path).map_err(|e| ResolverError::PackageSourceFetchFailed {
                name: pkg_ref.to_string(),
                detail: format!("reading {} : {e}", path.display()),
            })?
        } else {
            http_get_bytes(&url).map_err(|detail| ResolverError::PackageSourceFetchFailed {
                name: pkg_ref.to_string(),
                detail,
            })?
        };
        Ok((bytes, url))
    }

    /// Publish the source-only `.slpkg` `bytes` for `version` of `pkg_ref` into
    /// the generic store, returning the canonical URL they were written to.
    /// Writing is `file://`-only: an emit builds the static tree on disk, and a
    /// static HTTP mount is read-only. The `.slpkg` is written first, then the
    /// per-package `index.json` is refreshed so the read path lists the new
    /// version tokenlessly.
    pub fn upload_slpkg(
        &self,
        pkg_ref: &PackageRef,
        version: SemVer,
        bytes: &[u8],
    ) -> ResolverResult<String> {
        self.ensure_file_scheme("publishing a package")?;
        let url = self.download_url(pkg_ref, version);
        let upload_err = |detail: String| ResolverError::PackageSourceFetchFailed {
            name: pkg_ref.to_string(),
            detail,
        };
        let path = self.download_path(pkg_ref, version);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| upload_err(format!("creating {} : {e}", parent.display())))?;
        }
        std::fs::write(&path, bytes)
            .map_err(|e| upload_err(format!("writing {} : {e}", path.display())))?;
        // Refresh the anonymous version index so the HTTP read path can list
        // versions tokenlessly (`file://` enumerates dirs directly and does not
        // need it, but it is written unconditionally so both transports agree).
        self.write_version_index(pkg_ref, version)?;
        Ok(url)
    }

    /// Refresh the version index for `pkg_ref`: the union of the on-disk
    /// version directories and the `just_published` version, serialized as
    /// cargo-sparse-shaped NDJSON at `slpkg/<name>/index.json`. Enumerating the
    /// on-disk dirs makes every write self-heal — a missing or stale index is
    /// rebuilt correctly on the next publish.
    fn write_version_index(
        &self,
        pkg_ref: &PackageRef,
        just_published: SemVer,
    ) -> ResolverResult<()> {
        let upload_err = |detail: String| ResolverError::PackageSourceFetchFailed {
            name: pkg_ref.to_string(),
            detail,
        };
        let mut versions = self.list_versions_file(pkg_ref)?;
        versions.push(just_published);
        let versions = merge_versions(versions);
        let body = render_index_ndjson(pkg_ref.name.as_str(), &versions);
        let path = self.index_path(pkg_ref);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| upload_err(format!("creating {} : {e}", parent.display())))?;
        }
        std::fs::write(&path, body.as_bytes())
            .map_err(|e| upload_err(format!("writing {} : {e}", path.display())))?;
        Ok(())
    }

    /// Canonical URL of the release manifest for `release_version` — a plain
    /// file under the reserved [`RELEASE_MANIFEST_CHANNEL`].
    fn release_manifest_url(&self, release_version: &str) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.config.base_url,
            SLPKG_SUBTREE,
            RELEASE_MANIFEST_CHANNEL,
            release_version,
            RELEASE_MANIFEST_FILE,
        )
    }

    /// `file://` layout for the release manifest:
    /// `<root>/slpkg/streamlib-release/<V>/manifest.json`.
    fn release_manifest_path(&self, release_version: &str) -> PathBuf {
        self.file_root()
            .join(SLPKG_SUBTREE)
            .join(RELEASE_MANIFEST_CHANNEL)
            .join(release_version)
            .join(RELEASE_MANIFEST_FILE)
    }

    /// [`PackageRef`] for the reserved release-manifest channel under `org`, so
    /// the channel rides the same list/index machinery as any generic package.
    fn release_channel_ref(&self, org: &str) -> ResolverResult<PackageRef> {
        let org = crate::ident::Org::new(org)?;
        let name = crate::ident::Package::new(RELEASE_MANIFEST_CHANNEL)?;
        Ok(PackageRef::new(org, name))
    }

    /// List every release version that has a published release manifest.
    /// `file://` enumerates the release-channel directory; `http(s)://` reads
    /// the channel's version index. An empty list is the pre-atomic-release
    /// back-compat case.
    pub fn list_release_versions(&self, org: &str) -> ResolverResult<Vec<SemVer>> {
        let channel = self.release_channel_ref(org)?;
        self.list_versions(&channel)
    }

    /// Publish the release `manifest` for its `release_version`, returning the
    /// canonical URL it was written to. This is the **atomicity flip** — the
    /// caller runs it *last*, after every crate / SDK / package has landed, so
    /// the manifest's presence marks the release complete. `file://`-only (an
    /// emit builds the tree). `org` is the package source org the release lives under
    /// (e.g. `tatolab`).
    pub fn upload_release_manifest(
        &self,
        org: &str,
        manifest: &ReleaseManifest,
    ) -> ResolverResult<String> {
        self.ensure_file_scheme("publishing a release manifest")?;
        let upload_err = |detail: String| ResolverError::PackageSourceFetchFailed {
            name: format!("{}/{}", RELEASE_MANIFEST_CHANNEL, manifest.release_version),
            detail,
        };
        let release_semver: SemVer = manifest
            .release_version
            .parse()
            .map_err(|e| upload_err(format!("release_version is not a semver: {e}")))?;
        let body = serde_json::to_vec_pretty(manifest)
            .map_err(|e| upload_err(format!("serializing release manifest: {e}")))?;
        let url = self.release_manifest_url(&manifest.release_version);
        let path = self.release_manifest_path(&manifest.release_version);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| upload_err(format!("creating {} : {e}", parent.display())))?;
        }
        std::fs::write(&path, &body)
            .map_err(|e| upload_err(format!("writing {} : {e}", path.display())))?;
        // Refresh the channel's version index so the consumer read path can
        // list release versions tokenlessly.
        let channel = self.release_channel_ref(org)?;
        self.write_version_index(&channel, release_semver)?;
        Ok(url)
    }

    /// Fetch the release manifest for `release_version`. `Ok(None)` when no
    /// manifest is published for that version — the back-compat case for a
    /// pre-atomic-release package source, which the consumer treats as "no
    /// completeness signal, proceed". `Err` only on a real transport / parse
    /// failure.
    pub fn fetch_release_manifest(
        &self,
        _org: &str,
        release_version: &str,
    ) -> ResolverResult<Option<ReleaseManifest>> {
        let fetch_err = |detail: String| ResolverError::PackageSourceFetchFailed {
            name: format!("{}/{}", RELEASE_MANIFEST_CHANNEL, release_version),
            detail,
        };
        let bytes = if self.is_file_scheme() {
            let path = self.release_manifest_path(release_version);
            match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(fetch_err(format!("reading {} : {e}", path.display()))),
            }
        } else {
            let url = self.release_manifest_url(release_version);
            match http_get_optional(&url, None)
                .map_err(|detail| fetch_err(format!("fetching release manifest: {detail}")))?
            {
                Some(b) => b,
                None => return Ok(None),
            }
        };
        let manifest = serde_json::from_slice(&bytes)
            .map_err(|e| fetch_err(format!("parsing release manifest JSON: {e}")))?;
        Ok(Some(manifest))
    }

    /// Canonical `.slpkg` download URL (recorded in the lockfile):
    /// `<base>/slpkg/<name>/<version>/<name>.slpkg`.
    pub(crate) fn download_url(&self, pkg_ref: &PackageRef, version: SemVer) -> String {
        let name = pkg_ref.name.as_str();
        format!(
            "{}/{}/{}/{}/{}.slpkg",
            self.config.base_url, SLPKG_SUBTREE, name, version, name,
        )
    }

    /// `file://` layout: `<root>/slpkg/<name>/<version>/<name>.slpkg`.
    fn download_path(&self, pkg_ref: &PackageRef, version: SemVer) -> PathBuf {
        let name = pkg_ref.name.as_str();
        self.file_root()
            .join(SLPKG_SUBTREE)
            .join(name)
            .join(version.to_string())
            .join(format!("{name}.slpkg"))
    }

    /// Version-index URL — a plain file at `slpkg/<name>/index.json`. The
    /// `index.json` segment is not a semver, so the `.slpkg` version namespace
    /// can never collide with it.
    fn index_url(&self, pkg_ref: &PackageRef) -> String {
        format!(
            "{}/{}/{}/{}",
            self.config.base_url,
            SLPKG_SUBTREE,
            pkg_ref.name.as_str(),
            VERSION_INDEX_FILE,
        )
    }

    /// `file://` layout for the version index: `<root>/slpkg/<name>/index.json`.
    fn index_path(&self, pkg_ref: &PackageRef) -> PathBuf {
        self.file_root()
            .join(SLPKG_SUBTREE)
            .join(pkg_ref.name.as_str())
            .join(VERSION_INDEX_FILE)
    }

    /// List versions by reading the cargo-sparse-shaped version index
    /// (`slpkg/<name>/index.json`) over HTTP. A `404` (no index published yet)
    /// yields an empty list — parity with `file://`'s missing-directory case —
    /// so `select_version` reports `PackageSourceNoMatchingVersion` rather than a
    /// transport error.
    fn list_versions_http(&self, pkg_ref: &PackageRef) -> ResolverResult<Vec<SemVer>> {
        let url = self.index_url(pkg_ref);
        let body = match http_get_optional(&url, None) {
            Ok(Some(body)) => body,
            Ok(None) => return Ok(Vec::new()),
            Err(detail) => {
                return Err(ResolverError::PackageSourceFetchFailed {
                    name: pkg_ref.to_string(),
                    detail: format!("listing versions: {detail}"),
                });
            }
        };
        Ok(parse_index_ndjson(&body, pkg_ref.name.as_str()))
    }

    fn list_versions_file(&self, pkg_ref: &PackageRef) -> ResolverResult<Vec<SemVer>> {
        let dir = self
            .file_root()
            .join(SLPKG_SUBTREE)
            .join(pkg_ref.name.as_str());
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut versions = Vec::new();
        let entries = std::fs::read_dir(&dir).map_err(|e| ResolverError::PackageSourceFetchFailed {
            name: pkg_ref.to_string(),
            detail: format!("reading {} : {e}", dir.display()),
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| ResolverError::PackageSourceFetchFailed {
                name: pkg_ref.to_string(),
                detail: format!("reading {} : {e}", dir.display()),
            })?;
            if !entry.path().is_dir() {
                continue;
            }
            if let Some(v) = entry
                .file_name()
                .to_str()
                .and_then(|s| SemVer::from_dotted(s).ok())
            {
                versions.push(v);
            }
        }
        Ok(versions)
    }

    /// Guard the `file://`-only write paths (publish). A static HTTP mount is
    /// read-only; an emit builds the tree on disk via `file://`.
    fn ensure_file_scheme(&self, action: &str) -> ResolverResult<()> {
        if self.is_file_scheme() {
            Ok(())
        } else {
            Err(ResolverError::PackageSourceFetchFailed {
                name: self.config.base_url.clone(),
                detail: format!(
                    "{action} requires a file:// package source tree (a static HTTP mount is \
                     read-only); got `{}`",
                    self.config.base_url
                ),
            })
        }
    }
}

/// Select the highest version in `available` that satisfies `range`.
pub fn select_version(
    pkg_ref: &PackageRef,
    range: &crate::semver::SemVerRange,
    available: &[SemVer],
) -> ResolverResult<SemVer> {
    available
        .iter()
        .filter(|v| range.matches(**v))
        .max()
        .copied()
        .ok_or_else(|| {
            let mut sorted: Vec<String> = available.iter().map(|v| v.to_string()).collect();
            sorted.sort();
            ResolverError::PackageSourceNoMatchingVersion {
                name: pkg_ref.to_string(),
                range: range.to_string(),
                available: sorted.join(", "),
            }
        })
}

/// One line of the cargo-sparse-shaped version index — `{"name","vers"}`
/// per version. Extra fields a future index might carry (checksum, yanked)
/// are ignored on read, so the shape can grow without breaking older readers.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct VersionIndexLine {
    name: String,
    vers: String,
}

/// Render a sorted version list as NDJSON (one `{"name","vers"}` object per
/// line, trailing newline) — the byte shape published to `index.json`.
fn render_index_ndjson(name: &str, versions: &[SemVer]) -> String {
    let mut out = String::new();
    for v in versions {
        let line = VersionIndexLine {
            name: name.to_string(),
            vers: v.to_string(),
        };
        // Serializing a struct of two owned strings is infallible.
        out.push_str(&serde_json::to_string(&line).expect("serialize version index line"));
        out.push('\n');
    }
    out
}

/// Parse NDJSON index bytes into the semvers whose `name` matches `name`.
/// Blank lines and unparseable lines/versions are skipped, so a partially
/// corrupt index degrades to "fewer versions" rather than a hard failure.
fn parse_index_ndjson(body: &[u8], name: &str) -> Vec<SemVer> {
    String::from_utf8_lossy(body)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<VersionIndexLine>(l).ok())
        .filter(|e| e.name == name)
        .filter_map(|e| SemVer::from_dotted(&e.vers).ok())
        .collect()
}

/// Sort ascending + dedup a version list (the index is canonicalized this way
/// before publish so republishes are byte-stable and reads are ordered).
fn merge_versions(mut versions: Vec<SemVer>) -> Vec<SemVer> {
    versions.sort();
    versions.dedup();
    versions
}

/// Blocking GET that distinguishes a `404` (`Ok(None)`) from a real transport
/// or non-404 status error (`Err`). Used for the optional version index, the
/// optional release manifest, and the catalog client's tree-relative reads.
/// `token` is sent as an `Authorization: token <t>` header only when set (for
/// private mounts that gate reads behind auth); the first-party read path is
/// tokenless and passes `None`.
pub(crate) fn http_get_optional(url: &str, token: Option<&str>) -> Result<Option<Vec<u8>>, String> {
    let mut req = ureq::get(url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("token {t}"));
    }
    match req.call() {
        Ok(response) => {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
                .map_err(|e| format!("reading HTTP response body from {url}: {e}"))?;
            Ok(Some(bytes))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(format!("HTTP request to {url} failed: {e}")),
    }
}

/// Blocking GET of `url`'s raw body (the `.slpkg` download over a static HTTP
/// mount). A non-200 is a hard error — the version was selected from a listed
/// index, so its artifact must exist.
fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request to {url} failed: {e}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|e| format!("reading HTTP response body from {url}: {e}"))?;
    Ok(bytes)
}

/// Persist downloaded `.slpkg` bytes into the resolver cache as a file
/// `extract_slpkg` can read. Content-addressed by the bytes' hash so a
/// re-resolve reuses the artifact, with an atomic temp-then-rename publish.
pub(crate) fn cache_slpkg_bytes(
    pkg_ref: &PackageRef,
    bytes: &[u8],
    cache_dir: &Path,
) -> ResolverResult<PathBuf> {
    let dir = cache_dir.join("package-source");
    std::fs::create_dir_all(&dir).map_err(|e| ResolverError::Io {
        path: dir.clone(),
        source: e,
    })?;
    let hash = crate::lockfile::hash_content(bytes).replace(':', "_");
    let target = dir.join(format!("{}_{hash}.slpkg", pkg_ref.name.as_str()));
    if target.exists() {
        return Ok(target);
    }
    let tmp = dir.join(format!("{}_{hash}.slpkg.partial", pkg_ref.name.as_str()));
    std::fs::write(&tmp, bytes).map_err(|e| ResolverError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, &target).map_err(|e| ResolverError::Io {
        path: target.clone(),
        source: e,
    })?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::{Org, Package};
    use crate::semver::SemVerRange;

    fn pkg_ref(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    fn file_config(root: &std::path::Path) -> PackageSource {
        PackageSource {
            base_url: format!("file://{}", root.display()),
        }
    }

    #[test]
    fn select_version_picks_highest_in_range() {
        let pr = pkg_ref("tatolab", "escalate");
        let avail = vec![
            SemVer::new(1, 0, 0),
            SemVer::new(1, 2, 0),
            SemVer::new(1, 1, 5),
            SemVer::new(2, 0, 0),
        ];
        let range = SemVerRange::from_str("^1.0.0").unwrap();
        assert_eq!(
            select_version(&pr, &range, &avail).unwrap(),
            SemVer::new(1, 2, 0)
        );
    }

    #[test]
    fn select_version_prefers_release_over_prerelease() {
        use crate::semver::PrereleaseKind;
        let pr = pkg_ref("tatolab", "camera");
        let avail = vec![
            SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 9),
            SemVer::new(1, 2, 0),
            SemVer::new_prerelease(1, 2, 1, PrereleaseKind::Dev, 1),
        ];
        let range = SemVerRange::from_str("^1.2.0").unwrap();
        assert_eq!(
            select_version(&pr, &range, &avail).unwrap(),
            SemVer::new(1, 2, 0)
        );
    }

    #[test]
    fn select_version_picks_highest_prerelease_for_prerelease_range() {
        use crate::semver::PrereleaseKind;
        let pr = pkg_ref("tatolab", "camera");
        let avail = vec![
            SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 3),
            SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 9),
            SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Rc, 1),
        ];
        let range = SemVerRange::from_str(">=1.2.0-dev.3").unwrap();
        assert_eq!(
            select_version(&pr, &range, &avail).unwrap(),
            SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Rc, 1)
        );
    }

    #[test]
    fn list_versions_file_parses_prerelease_dir_names() {
        // Directory-name version parsing must accept `-dev.N` / `-rc.N` dirs
        // under `slpkg/<name>/` so a prerelease publish is listable.
        use crate::semver::PrereleaseKind;
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("slpkg").join("camera");
        std::fs::create_dir_all(pkg_dir.join("0.4.33-dev.2")).unwrap();
        std::fs::create_dir_all(pkg_dir.join("0.4.33")).unwrap();
        let cfg = file_config(tmp.path());
        let client = PackageSourceClient::new(&cfg);
        let mut versions = client.list_versions(&pkg_ref("tatolab", "camera")).unwrap();
        versions.sort();
        assert_eq!(
            versions,
            vec![
                SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2),
                SemVer::new(0, 4, 33),
            ]
        );
    }

    #[test]
    fn select_version_errors_when_none_match() {
        let pr = pkg_ref("tatolab", "escalate");
        let avail = vec![SemVer::new(2, 0, 0), SemVer::new(3, 1, 0)];
        let range = SemVerRange::from_str("^1.0.0").unwrap();
        let err = select_version(&pr, &range, &avail).unwrap_err();
        match err {
            ResolverError::PackageSourceNoMatchingVersion {
                range, available, ..
            } => {
                assert_eq!(range, "^1.0.0");
                assert!(available.contains("2.0.0"));
                assert!(available.contains("3.1.0"));
            }
            other => panic!("expected PackageSourceNoMatchingVersion, got {other:?}"),
        }
    }

    #[test]
    fn ecosystem_urls_derive_from_the_single_base() {
        // Every channel derives from the one base URL — the "single package
        // source location, toolchain-derived" contract.
        let http = PackageSource {
            base_url: "https://registry.tatolab.com".into(),
        };
        assert_eq!(
            http.pypi_simple_index_url(),
            "https://registry.tatolab.com/pypi/simple"
        );
        assert_eq!(http.npm_registry_url(), "https://registry.tatolab.com/npm/");
        assert_eq!(
            http.cargo_sparse_index_url(),
            "sparse+https://registry.tatolab.com/cargo/"
        );
        assert!(http.local_tree_root().is_none());

        let file = PackageSource {
            base_url: "file:///srv/tree".into(),
        };
        assert_eq!(file.pypi_simple_index_url(), "file:///srv/tree/pypi/simple");
        assert_eq!(file.npm_registry_url(), "file:///srv/tree/npm/");
        assert_eq!(file.local_tree_root(), Some(PathBuf::from("/srv/tree")));
    }

    #[test]
    fn for_local_tree_canonicalizes_and_errors_on_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = PackageSource::for_local_tree(tmp.path()).unwrap();
        assert!(cfg.base_url.starts_with("file://"));
        // The canonical root round-trips back to the dir.
        assert_eq!(
            cfg.local_tree_root().unwrap().canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
        // A non-existent path is a typed error, not a silent bad URL.
        let missing = tmp.path().join("does-not-exist");
        assert!(PackageSource::for_local_tree(&missing).is_err());
    }

    #[test]
    fn scheme_detection_http_vs_file() {
        let http = PackageSource {
            base_url: "https://registry.tatolab.com".into(),
        };
        assert!(!PackageSourceClient::new(&http).is_file_scheme());
        let file = PackageSource {
            base_url: "file:///tmp/tree".into(),
        };
        assert!(PackageSourceClient::new(&file).is_file_scheme());
    }

    #[test]
    fn file_scheme_layout_paths_are_under_slpkg_subtree() {
        let cfg = PackageSource {
            base_url: "file:///tmp/tree".into(),
        };
        let client = PackageSourceClient::new(&cfg);
        let pr = pkg_ref("tatolab", "escalate");
        assert_eq!(
            client.download_path(&pr, SemVer::new(1, 2, 0)),
            PathBuf::from("/tmp/tree/slpkg/escalate/1.2.0/escalate.slpkg")
        );
        assert_eq!(
            client.index_path(&pr),
            PathBuf::from("/tmp/tree/slpkg/escalate/index.json")
        );
    }

    /// The `file://` publish path maintains `slpkg/<name>/index.json` on every
    /// `upload_slpkg` and the HTTP read path lists from it. Round-trip the two
    /// against a served tree with NO token — the tokenless read the static tree
    /// exists to enable. Mentally revert the `write_version_index` call in
    /// `upload_slpkg`: the HTTP list yields empty and the assertion fails.
    #[test]
    fn upload_maintains_index_and_http_lists_from_it() {
        use std::io::{Read, Write};

        let tree = tempfile::tempdir().unwrap();
        let cfg = file_config(tree.path());
        let client = PackageSourceClient::new(&cfg);
        let pr = pkg_ref("tatolab", "camera");

        client
            .upload_slpkg(&pr, SemVer::new(0, 4, 32), b"a")
            .unwrap();
        client
            .upload_slpkg(&pr, SemVer::new(0, 4, 33), b"b")
            .unwrap();

        // The on-disk index holds the union, canonicalized ascending.
        let index = tree.path().join("slpkg/camera/index.json");
        let body = std::fs::read(&index).unwrap();
        assert_eq!(
            parse_index_ndjson(&body, "camera"),
            vec![SemVer::new(0, 4, 32), SemVer::new(0, 4, 33)]
        );

        // Serve the tree read-only and prove the HTTP list reads that index,
        // and the HTTP download reads the `.slpkg` — both tokenless.
        let root = tree.path().to_path_buf();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split(' ')
                    .nth(1)
                    .unwrap_or("");
                let rel = path.trim_start_matches('/');
                let body = std::fs::read(root.join(rel)).unwrap_or_default();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
            }
        });

        let http_cfg = PackageSource {
            base_url: format!("http://127.0.0.1:{port}"),
        };
        let http_client = PackageSourceClient::new(&http_cfg);
        let versions = http_client.list_versions(&pr).unwrap();
        assert_eq!(versions, vec![SemVer::new(0, 4, 32), SemVer::new(0, 4, 33)]);
        let (bytes, url) = http_client
            .download_slpkg(&pr, SemVer::new(0, 4, 33))
            .unwrap();
        assert_eq!(bytes, b"b");
        assert!(
            url.ends_with("/slpkg/camera/0.4.33/camera.slpkg"),
            "url: {url}"
        );
        server.join().unwrap();
    }

    /// A `404` on the index (no version published yet) yields an empty list,
    /// not a transport error — parity with `file://`'s missing-directory case.
    #[test]
    fn http_list_missing_index_yields_empty() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).unwrap();
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let cfg = PackageSource {
            base_url: format!("http://127.0.0.1:{port}"),
        };
        let client = PackageSourceClient::new(&cfg);
        let versions = client.list_versions(&pkg_ref("tatolab", "nope")).unwrap();
        assert!(
            versions.is_empty(),
            "404 index must list as empty, got {versions:?}"
        );
        server.join().unwrap();
    }

    /// Publishing over a non-`file://` scheme is refused — a static HTTP mount
    /// is read-only.
    #[test]
    fn upload_over_http_is_refused() {
        let cfg = PackageSource {
            base_url: "https://registry.tatolab.com".into(),
        };
        let client = PackageSourceClient::new(&cfg);
        let err = client
            .upload_slpkg(&pkg_ref("tatolab", "camera"), SemVer::new(1, 0, 0), b"x")
            .unwrap_err();
        assert!(
            matches!(err, ResolverError::PackageSourceFetchFailed { .. }),
            "expected a refusal, got {err:?}"
        );
    }

    #[test]
    fn index_ndjson_render_parse_round_trip() {
        use crate::semver::PrereleaseKind;
        let versions = vec![
            SemVer::new(0, 4, 32),
            SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2),
            SemVer::new(0, 4, 33),
            SemVer::new(1, 0, 0),
        ];
        let rendered = render_index_ndjson("camera", &versions);
        assert_eq!(rendered.lines().count(), 4);
        assert!(rendered.contains("\"vers\":\"0.4.33\""));
        assert!(rendered.contains("\"vers\":\"0.4.33-dev.2\""));
        assert!(rendered.ends_with('\n'));
        let parsed = parse_index_ndjson(rendered.as_bytes(), "camera");
        assert_eq!(parsed, versions);
        let mixed = format!("{rendered}{{\"name\":\"display\",\"vers\":\"9.9.9\"}}\n");
        assert_eq!(parse_index_ndjson(mixed.as_bytes(), "camera"), versions);
        let dirty = format!("\n{rendered}not-json\n");
        assert_eq!(parse_index_ndjson(dirty.as_bytes(), "camera"), versions);
    }

    #[test]
    fn merge_versions_sorts_and_dedups() {
        let merged = merge_versions(vec![
            SemVer::new(1, 2, 0),
            SemVer::new(1, 0, 0),
            SemVer::new(1, 2, 0),
            SemVer::new(0, 9, 9),
        ]);
        assert_eq!(
            merged,
            vec![
                SemVer::new(0, 9, 9),
                SemVer::new(1, 0, 0),
                SemVer::new(1, 2, 0)
            ]
        );
    }

    /// The release-manifest publish/fetch round-trip over the `file://`
    /// transport — the hermetic path CI and the scratch package source integration
    /// test ride. Mentally revert `upload_release_manifest` to a no-op and
    /// `fetch_release_manifest` returns `None`, so this locks the write→read
    /// contract, not merely a happy path.
    #[test]
    fn release_manifest_file_scheme_round_trip() {
        use crate::release::{ReleaseManifest, ReleaseManifestMember};

        let tmp_guard = tempfile::tempdir().unwrap();
        let cfg = file_config(tmp_guard.path());
        let client = PackageSourceClient::new(&cfg);

        // Missing manifest ⇒ None (the pre-atomic-release back-compat case).
        assert!(
            client
                .fetch_release_manifest("tatolab", "0.5.1")
                .unwrap()
                .is_none()
        );

        let mut manifest = ReleaseManifest::new(
            "0.5.1",
            vec![
                ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1"),
                ReleaseManifestMember::new("vulkan-jpeg", "0.5.1"),
            ],
        );
        manifest.python = Some("0.5.1".to_string());
        manifest.packages = vec![ReleaseManifestMember::new("@tatolab/jpeg", "1.0.7")];

        let url = client
            .upload_release_manifest("tatolab", &manifest)
            .unwrap();
        assert!(
            url.ends_with("/slpkg/streamlib-release/0.5.1/manifest.json"),
            "url: {url}"
        );
        // The layout must be `<root>/slpkg/streamlib-release/<V>/manifest.json`.
        assert!(
            tmp_guard
                .path()
                .join("slpkg")
                .join("streamlib-release")
                .join("0.5.1")
                .join("manifest.json")
                .is_file()
        );

        let back = client
            .fetch_release_manifest("tatolab", "0.5.1")
            .unwrap()
            .unwrap();
        assert_eq!(back, manifest);

        // The release channel is listable — the consumer's range-aware
        // completeness check discovers available releases this way.
        assert_eq!(
            client.list_release_versions("tatolab").unwrap(),
            vec![SemVer::new(0, 5, 1)]
        );
    }
}
