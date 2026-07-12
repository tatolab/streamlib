// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Gitea generic-registry client for schema-package resolution.
//!
//! Schema packages (`@tatolab/escalate` and friends) are distributed as
//! source-only `.slpkg`s in Gitea's **generic** registry. Resolving a
//! `Registry` dependency by a semver *range* requires two steps the flat
//! generic registry can't do in one request:
//!
//! 1. **List** the available concrete versions of the package, then select
//!    the highest one that satisfies the declared range — cargo/npm/pip
//!    semantics.
//! 2. **Download** that version's `.slpkg` from the generic registry's
//!    by-version download namespace.
//!
//! Both steps are **anonymous** on a public registry, matching cargo's
//! sparse index. The generic registry has no native version-listing query
//! (Gitea's `/api/v1/packages` management API `401`s anonymously), so each
//! publish writes a cargo-sparse-shaped **version index** as a plain generic
//! file at `<base>/api/packages/<org>/generic/<name>/index/index.json`
//! (anonymously downloadable like any generic file). The list step reads
//! that index; the only token-requiring path is **publish**.
//!
//! `http(s)://` is the production transport; `file://` is the hermetic
//! local-mirror / test transport (mirroring the engine's remote-`.slpkg`
//! fetch), where the base URL points at a directory laid out as
//! `<base>/<name>/<version>/<name>.slpkg` (the `file://` list step
//! enumerates the per-version subdirectories directly — no index file).

use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::ident::PackageRef;
use crate::release::ReleaseManifest;
use crate::semver::SemVer;

/// Generic-registry "package name" the release manifest is published under.
/// Reserved channel — a `@org/name` package can never collide with it
/// because package names never equal this literal.
pub const RELEASE_MANIFEST_CHANNEL: &str = "streamlib-release";
/// File name of the release manifest inside its per-version directory.
pub const RELEASE_MANIFEST_FILE: &str = "manifest.json";

/// Environment variable carrying the Gitea base URL (e.g.
/// `http://localhost:3000`). Falls back to `GITEA_URL` to match the
/// provisioning scripts' convention.
pub const REGISTRY_URL_ENV: &str = "STREAMLIB_REGISTRY_URL";
const REGISTRY_URL_ENV_FALLBACK: &str = "GITEA_URL";

/// Environment variable carrying the registry credential. The read path
/// (list + download) is anonymous on a public registry, so this is **only**
/// required to *publish*; it is also sent on reads when set, for private
/// registries that gate generic downloads behind auth.
pub const REGISTRY_TOKEN_ENV: &str = "STREAMLIB_REGISTRY_TOKEN";

/// Resolved configuration for the Gitea generic registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    /// Base URL — `http(s)://host[:port]` for production, or
    /// `file:///abs/dir` for a hermetic local mirror.
    pub base_url: String,
    /// Optional bearer/`token` credential for the management API.
    pub token: Option<String>,
}

impl RegistryConfig {
    /// Build from the environment. Returns `None` when no base URL is set,
    /// so default callers (e.g. `ResolverOptions::default()` in a build
    /// script) transparently pick up a configured registry without one.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var(REGISTRY_URL_ENV)
            .ok()
            .or_else(|| std::env::var(REGISTRY_URL_ENV_FALLBACK).ok())
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())?;
        let token = std::env::var(REGISTRY_TOKEN_ENV).ok().filter(|s| !s.is_empty());
        Some(Self { base_url, token })
    }
}

/// One entry in Gitea's `GET /api/v1/packages/{owner}` response.
#[derive(Debug, serde::Deserialize)]
struct GiteaPackageEntry {
    name: String,
    version: String,
    #[serde(rename = "type")]
    package_type: String,
}

/// Client over a single [`RegistryConfig`].
pub struct RegistryClient<'a> {
    config: &'a RegistryConfig,
}

impl<'a> RegistryClient<'a> {
    pub fn new(config: &'a RegistryConfig) -> Self {
        Self { config }
    }

    fn is_file_scheme(&self) -> bool {
        self.config.base_url.starts_with("file://")
    }

    /// The on-disk directory a `file://` base URL points at.
    fn file_base_dir(&self) -> PathBuf {
        PathBuf::from(self.config.base_url.trim_start_matches("file://"))
    }

    /// List every concrete version of `pkg_ref` the registry holds.
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
            std::fs::read(&path).map_err(|e| ResolverError::RegistryFetchFailed {
                name: pkg_ref.to_string(),
                detail: format!("reading {} : {e}", path.display()),
            })?
        } else {
            http_get_bytes(&url, self.config.token.as_deref()).map_err(|detail| {
                ResolverError::RegistryFetchFailed {
                    name: pkg_ref.to_string(),
                    detail,
                }
            })?
        };
        Ok((bytes, url))
    }

    /// Publish the source-only `.slpkg` `bytes` for `version` of `pkg_ref` to
    /// the generic registry, returning the canonical URL they were published
    /// to. The mirror image of [`Self::download_slpkg`]: `file://` writes the
    /// mirror layout, `http(s)://` uploads with the token. The remote upload
    /// is a delete-then-PUT so a republish of the same version overwrites the
    /// prior bytes — Gitea generic packages are immutable per
    /// (name, version, file), so a bare re-PUT of an existing file 409s.
    pub fn upload_slpkg(
        &self,
        pkg_ref: &PackageRef,
        version: SemVer,
        bytes: &[u8],
    ) -> ResolverResult<String> {
        let url = self.download_url(pkg_ref, version);
        let upload_err = |detail: String| ResolverError::RegistryFetchFailed {
            name: pkg_ref.to_string(),
            detail,
        };
        if self.is_file_scheme() {
            let path = self.download_path(pkg_ref, version);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| upload_err(format!("creating {} : {e}", parent.display())))?;
            }
            std::fs::write(&path, bytes)
                .map_err(|e| upload_err(format!("writing {} : {e}", path.display())))?;
        } else {
            // Overwrite-on-republish: drop the existing version (a 404 is the
            // benign first-publish case) before the PUT, since Gitea rejects a
            // duplicate generic file upload.
            let version_url = format!(
                "{}/api/v1/packages/{}/generic/{}/{}",
                self.config.base_url,
                pkg_ref.org.as_str(),
                pkg_ref.name.as_str(),
                version,
            );
            http_delete(&version_url, self.config.token.as_deref());
            http_put_bytes(&url, self.config.token.as_deref(), bytes).map_err(upload_err)?;
            // Maintain the anonymous version index so the read path lists
            // versions tokenless. Recompute from the authed management
            // listing unioned with the just-published version — every publish
            // rewrites the full correct index, so a missed or stale index
            // self-heals on the next publish.
            self.write_version_index(pkg_ref, version)?;
        }
        Ok(url)
    }

    /// Rewrite the anonymous version index for `pkg_ref` after a publish.
    /// The index is the union of the authed management-API listing and the
    /// `just_published` version (covers any management-API propagation lag),
    /// serialized as cargo-sparse-shaped NDJSON and uploaded (delete-then-PUT,
    /// since the `index` pseudo-version is immutable per generic-file rules).
    ///
    /// Ordering note: [`Self::upload_slpkg`] PUTs the artifact *before* calling
    /// this, so a failure here surfaces as an `Err` even though the `.slpkg` is
    /// already published and downloadable — the package just isn't *listable*
    /// until its index lands. This is deliberately a hard failure (the index is
    /// the read path; an unlistable artifact isn't resolvable) and is fully
    /// recoverable: re-running publish is idempotent and rewrites the full
    /// correct index from the management listing.
    fn write_version_index(
        &self,
        pkg_ref: &PackageRef,
        just_published: SemVer,
    ) -> ResolverResult<()> {
        let upload_err = |detail: String| ResolverError::RegistryFetchFailed {
            name: pkg_ref.to_string(),
            detail,
        };
        let mut versions = self.list_versions_via_management_api(pkg_ref)?;
        versions.push(just_published);
        let versions = merge_versions(versions);
        let body = render_index_ndjson(pkg_ref.name.as_str(), &versions);
        // Drop the prior `index` pseudo-version, then PUT the fresh index.
        let index_version_url = format!(
            "{}/api/v1/packages/{}/generic/{}/index",
            self.config.base_url,
            pkg_ref.org.as_str(),
            pkg_ref.name.as_str(),
        );
        http_delete(&index_version_url, self.config.token.as_deref());
        http_put_bytes(&self.index_url(pkg_ref), self.config.token.as_deref(), body.as_bytes())
            .map_err(|detail| upload_err(format!("publishing version index: {detail}")))?;
        Ok(())
    }

    /// Canonical URL of the release manifest for `release_version` — a plain
    /// generic file under the reserved [`RELEASE_MANIFEST_CHANNEL`].
    fn release_manifest_url(&self, org: &str, release_version: &str) -> String {
        format!(
            "{}/api/packages/{}/generic/{}/{}/{}",
            self.config.base_url,
            org,
            RELEASE_MANIFEST_CHANNEL,
            release_version,
            RELEASE_MANIFEST_FILE,
        )
    }

    /// `file://` mirror layout for the release manifest:
    /// `<base>/streamlib-release/<V>/manifest.json`.
    fn release_manifest_path(&self, release_version: &str) -> PathBuf {
        self.file_base_dir()
            .join(RELEASE_MANIFEST_CHANNEL)
            .join(release_version)
            .join(RELEASE_MANIFEST_FILE)
    }

    /// [`PackageRef`] for the reserved release-manifest channel under `org`,
    /// so the channel rides the same list/index machinery as any generic
    /// package.
    fn release_channel_ref(&self, org: &str) -> ResolverResult<PackageRef> {
        let org = crate::ident::Org::new(org)?;
        let name = crate::ident::Package::new(RELEASE_MANIFEST_CHANNEL)?;
        Ok(PackageRef::new(org, name))
    }

    /// List every release version that has a published release manifest.
    /// `file://` enumerates the mirror directory; `http(s)://` reads the
    /// channel's anonymous version index (maintained by
    /// [`Self::upload_release_manifest`]). An empty list is the
    /// pre-atomic-release back-compat case.
    pub fn list_release_versions(&self, org: &str) -> ResolverResult<Vec<SemVer>> {
        let channel = self.release_channel_ref(org)?;
        self.list_versions(&channel)
    }

    /// Publish the release `manifest` for its `release_version`, returning the
    /// canonical URL it was written to. This is the **atomicity flip** — the
    /// caller runs it *last*, after every crate / SDK / package has landed, so
    /// the manifest's presence marks the release complete. `file://` writes
    /// the mirror layout; `http(s)://` deletes-then-PUTs (Gitea generic files
    /// are immutable per (name, version, file), so a bare re-PUT 409s) and
    /// rewrites the channel's anonymous version index so consumers can list
    /// available releases tokenlessly. `org` is the registry org the release
    /// lives under (e.g. `tatolab`).
    pub fn upload_release_manifest(
        &self,
        org: &str,
        manifest: &ReleaseManifest,
    ) -> ResolverResult<String> {
        let upload_err = |detail: String| ResolverError::RegistryFetchFailed {
            name: format!("{}/{}", RELEASE_MANIFEST_CHANNEL, manifest.release_version),
            detail,
        };
        let release_semver: SemVer = manifest
            .release_version
            .parse()
            .map_err(|e| upload_err(format!("release_version is not a semver: {e}")))?;
        let body = serde_json::to_vec_pretty(manifest)
            .map_err(|e| upload_err(format!("serializing release manifest: {e}")))?;
        let url = self.release_manifest_url(org, &manifest.release_version);
        if self.is_file_scheme() {
            let path = self.release_manifest_path(&manifest.release_version);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| upload_err(format!("creating {} : {e}", parent.display())))?;
            }
            std::fs::write(&path, &body)
                .map_err(|e| upload_err(format!("writing {} : {e}", path.display())))?;
        } else {
            // Overwrite-on-republish: drop the existing version before the PUT.
            let version_url = format!(
                "{}/api/v1/packages/{}/generic/{}/{}",
                self.config.base_url,
                org,
                RELEASE_MANIFEST_CHANNEL,
                manifest.release_version,
            );
            http_delete(&version_url, self.config.token.as_deref());
            http_put_bytes(&url, self.config.token.as_deref(), &body).map_err(upload_err)?;
            // Maintain the channel's anonymous version index so the consumer
            // read path can list release versions tokenlessly (mirrors the
            // upload_slpkg ordering: artifact first, index last).
            let channel = self.release_channel_ref(org)?;
            self.write_version_index(&channel, release_semver)?;
        }
        Ok(url)
    }

    /// Fetch the release manifest for `release_version`. `Ok(None)` when no
    /// manifest is published for that version — the back-compat case for a
    /// pre-atomic-release registry, which the consumer treats as "no
    /// completeness signal, proceed". `Err` only on a real transport / parse
    /// failure.
    pub fn fetch_release_manifest(
        &self,
        org: &str,
        release_version: &str,
    ) -> ResolverResult<Option<ReleaseManifest>> {
        let fetch_err = |detail: String| ResolverError::RegistryFetchFailed {
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
            let url = self.release_manifest_url(org, release_version);
            match http_get_optional(&url, self.config.token.as_deref())
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

    /// Canonical generic-registry download URL (recorded in the lockfile).
    fn download_url(&self, pkg_ref: &PackageRef, version: SemVer) -> String {
        let name = pkg_ref.name.as_str();
        format!(
            "{}/api/packages/{}/generic/{}/{}/{}.slpkg",
            self.config.base_url,
            pkg_ref.org.as_str(),
            name,
            version,
            name,
        )
    }

    /// `file://` mirror layout: `<base>/<name>/<version>/<name>.slpkg`.
    fn download_path(&self, pkg_ref: &PackageRef, version: SemVer) -> PathBuf {
        let name = pkg_ref.name.as_str();
        self.file_base_dir()
            .join(name)
            .join(version.to_string())
            .join(format!("{name}.slpkg"))
    }

    /// Anonymous version-index URL — a plain generic file under the literal
    /// `index` pseudo-version. Downloaded tokenless like any generic file;
    /// the `index` segment is not a semver, so the `.slpkg` version namespace
    /// can never collide with it.
    fn index_url(&self, pkg_ref: &PackageRef) -> String {
        format!(
            "{}/api/packages/{}/generic/{}/index/index.json",
            self.config.base_url,
            pkg_ref.org.as_str(),
            pkg_ref.name.as_str(),
        )
    }

    /// List versions by reading the anonymous, cargo-sparse-shaped version
    /// index (`index/index.json`). No token is required on a public registry;
    /// the configured token is still sent when set, for private registries
    /// that gate generic downloads. A `404` (no index published yet) yields
    /// an empty list — parity with `file://`'s missing-directory case — so
    /// `select_version` reports `RegistryNoMatchingVersion` rather than a
    /// transport error.
    fn list_versions_http(&self, pkg_ref: &PackageRef) -> ResolverResult<Vec<SemVer>> {
        let url = self.index_url(pkg_ref);
        let body = match http_get_optional(&url, self.config.token.as_deref()) {
            Ok(Some(body)) => body,
            Ok(None) => return Ok(Vec::new()),
            Err(detail) => {
                return Err(ResolverError::RegistryFetchFailed {
                    name: pkg_ref.to_string(),
                    detail: format!("listing versions: {detail}"),
                })
            }
        };
        Ok(parse_index_ndjson(&body, pkg_ref.name.as_str()))
    }

    /// List versions via Gitea's **authed** package-management API. Used only
    /// on the publish path to recompute the anonymous index — never on the
    /// read path (the management API `401`s anonymously). `index` and any
    /// other non-semver pseudo-version is dropped by the semver parse.
    fn list_versions_via_management_api(
        &self,
        pkg_ref: &PackageRef,
    ) -> ResolverResult<Vec<SemVer>> {
        let name = pkg_ref.name.as_str();
        let url = format!(
            "{}/api/v1/packages/{}?type=generic&q={}",
            self.config.base_url,
            pkg_ref.org.as_str(),
            name,
        );
        let body = http_get_bytes(&url, self.config.token.as_deref()).map_err(|detail| {
            ResolverError::RegistryFetchFailed {
                name: pkg_ref.to_string(),
                detail: format!("listing versions: {detail}"),
            }
        })?;
        let entries: Vec<GiteaPackageEntry> =
            serde_json::from_slice(&body).map_err(|e| ResolverError::RegistryFetchFailed {
                name: pkg_ref.to_string(),
                detail: format!("parsing package list JSON: {e}"),
            })?;
        Ok(entries
            .into_iter()
            .filter(|e| e.package_type == "generic" && e.name == name)
            .filter_map(|e| SemVer::from_dotted(&e.version).ok())
            .collect())
    }

    fn list_versions_file(&self, pkg_ref: &PackageRef) -> ResolverResult<Vec<SemVer>> {
        let dir = self.file_base_dir().join(pkg_ref.name.as_str());
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut versions = Vec::new();
        let entries = std::fs::read_dir(&dir).map_err(|e| ResolverError::RegistryFetchFailed {
            name: pkg_ref.to_string(),
            detail: format!("reading {} : {e}", dir.display()),
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| ResolverError::RegistryFetchFailed {
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
            ResolverError::RegistryNoMatchingVersion {
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
/// line, trailing newline) — the byte shape published to `index/index.json`.
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
/// or non-404 status error (`Err`). Used for the optional version index.
fn http_get_optional(url: &str, token: Option<&str>) -> Result<Option<Vec<u8>>, String> {
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

/// Blocking GET of `url`'s raw body. `token` is sent as a Gitea
/// `Authorization: token <t>` header when present.
fn http_get_bytes(url: &str, token: Option<&str>) -> Result<Vec<u8>, String> {
    let mut req = ureq::get(url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("token {t}"));
    }
    let response = req
        .call()
        .map_err(|e| format!("HTTP request to {url} failed: {e}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|e| format!("reading HTTP response body from {url}: {e}"))?;
    Ok(bytes)
}

/// Blocking PUT of `bytes` to `url`, with a Gitea `Authorization: token <t>`
/// header when present. Used to publish a package `.slpkg` to the generic
/// registry.
fn http_put_bytes(url: &str, token: Option<&str>, bytes: &[u8]) -> Result<(), String> {
    let mut req = ureq::put(url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("token {t}"));
    }
    req.send_bytes(bytes)
        .map(|_| ())
        .map_err(|e| format!("HTTP PUT to {url} failed: {e}"))
}

/// Best-effort blocking DELETE of `url` (a generic-package version). Errors
/// are intentionally swallowed — a 404 (nothing to overwrite) is the common,
/// benign case on a first publish.
fn http_delete(url: &str, token: Option<&str>) {
    let mut req = ureq::delete(url);
    if let Some(t) = token {
        req = req.set("Authorization", &format!("token {t}"));
    }
    let _ = req.call();
}

/// Persist downloaded `.slpkg` bytes into the resolver cache as a file
/// `extract_slpkg` can read. Content-addressed by the bytes' hash so a
/// re-resolve reuses the artifact, with an atomic temp-then-rename publish.
pub(crate) fn cache_slpkg_bytes(
    pkg_ref: &PackageRef,
    bytes: &[u8],
    cache_dir: &Path,
) -> ResolverResult<PathBuf> {
    let dir = cache_dir.join("registry");
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
        assert_eq!(select_version(&pr, &range, &avail).unwrap(), SemVer::new(1, 2, 0));
    }

    #[test]
    fn select_version_prefers_release_over_prerelease() {
        // A release-req range must pick the release even when higher-ordinal
        // prereleases share or exceed the core. No logic change in
        // `select_version` — the new Ord + npm range policy carry this.
        use crate::semver::PrereleaseKind;
        let pr = pkg_ref("tatolab", "camera");
        let avail = vec![
            SemVer::new_prerelease(1, 2, 0, PrereleaseKind::Dev, 9),
            SemVer::new(1, 2, 0),
            SemVer::new_prerelease(1, 2, 1, PrereleaseKind::Dev, 1),
        ];
        let range = SemVerRange::from_str("^1.2.0").unwrap();
        assert_eq!(select_version(&pr, &range, &avail).unwrap(), SemVer::new(1, 2, 0));
    }

    #[test]
    fn select_version_picks_highest_prerelease_for_prerelease_range() {
        // A prerelease-req range selects the highest same-core prerelease when
        // no release is available.
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
        // so a prerelease publish is listable from the file:// mirror.
        use crate::semver::PrereleaseKind;
        let tmp = std::env::temp_dir().join(format!("slpkg-pre-{}", std::process::id()));
        let pkg_dir = tmp.join("camera");
        std::fs::create_dir_all(pkg_dir.join("0.4.33-dev.2")).unwrap();
        std::fs::create_dir_all(pkg_dir.join("0.4.33")).unwrap();
        let cfg = RegistryConfig {
            base_url: format!("file://{}", tmp.display()),
            token: None,
        };
        let client = RegistryClient::new(&cfg);
        let mut versions = client.list_versions(&pkg_ref("tatolab", "camera")).unwrap();
        versions.sort();
        assert_eq!(
            versions,
            vec![
                SemVer::new_prerelease(0, 4, 33, PrereleaseKind::Dev, 2),
                SemVer::new(0, 4, 33),
            ]
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn select_version_errors_when_none_match() {
        let pr = pkg_ref("tatolab", "escalate");
        let avail = vec![SemVer::new(2, 0, 0), SemVer::new(3, 1, 0)];
        let range = SemVerRange::from_str("^1.0.0").unwrap();
        let err = select_version(&pr, &range, &avail).unwrap_err();
        match err {
            ResolverError::RegistryNoMatchingVersion { range, available, .. } => {
                assert_eq!(range, "^1.0.0");
                assert!(available.contains("2.0.0"));
                assert!(available.contains("3.1.0"));
            }
            other => panic!("expected RegistryNoMatchingVersion, got {other:?}"),
        }
    }

    #[test]
    fn scheme_detection_http_vs_file() {
        // The client routes list/download by URL scheme. `from_env`'s env
        // parsing is intentionally NOT unit-tested here — mutating process
        // env would race the resolver's `registry: None` tests that rely on
        // an unset registry env; it's covered via the resolver's default
        // path in real usage and the file:// integration tests.
        let http = RegistryConfig {
            base_url: "http://localhost:3000".into(),
            token: Some("abc".into()),
        };
        assert!(!RegistryClient::new(&http).is_file_scheme());
        let file = RegistryConfig {
            base_url: "file:///tmp/mirror".into(),
            token: None,
        };
        assert!(RegistryClient::new(&file).is_file_scheme());
    }

    #[test]
    fn file_scheme_layout_paths() {
        let cfg = RegistryConfig {
            base_url: "file:///tmp/mirror".into(),
            token: None,
        };
        let client = RegistryClient::new(&cfg);
        assert!(client.is_file_scheme());
        let pr = pkg_ref("tatolab", "escalate");
        assert_eq!(
            client.download_path(&pr, SemVer::new(1, 2, 0)),
            PathBuf::from("/tmp/mirror/escalate/1.2.0/escalate.slpkg")
        );
    }

    /// Locks the production `http://` **read** path end-to-end against a
    /// one-shot localhost server, with **no token configured** — the
    /// tokenless read the sparse index exists to enable. The list step reads
    /// the anonymous `index/index.json` (NDJSON) and the download step reads
    /// the generic-file URL. Mentally revert `list_versions_http` to the
    /// authed `/api/v1/packages` management API: against a real public Gitea
    /// the no-token list `401`s, so this locks the tokenless contract rather
    /// than merely exercising a happy path.
    #[test]
    fn http_list_and_download_against_localhost() {
        use std::io::{Read, Write};

        let slpkg_body = b"slpkg-zip-bytes".to_vec();
        // Anonymous version-index (NDJSON) body. A foreign-named line is
        // included to prove the name filter drops it.
        let index_ndjson = "{\"name\":\"escalate\",\"vers\":\"1.0.0\"}\n\
             {\"name\":\"escalate\",\"vers\":\"1.2.0\"}\n\
             {\"name\":\"other\",\"vers\":\"9.9.9\"}\n"
            .to_string();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let slpkg_for_server = slpkg_body.clone();
        let server = std::thread::spawn(move || {
            // Serve exactly two requests: the index read, then the download.
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let request_line = req.lines().next().unwrap_or("");
                let body: Vec<u8> = if request_line.contains("/index/index.json") {
                    index_ndjson.clone().into_bytes()
                } else {
                    slpkg_for_server.clone()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
            }
        });

        let cfg = RegistryConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            token: None, // <-- no token: the whole point of the sparse index.
        };
        let client = RegistryClient::new(&cfg);
        let pr = pkg_ref("tatolab", "escalate");

        // Index read: only matching-name lines are returned, sorted-as-served.
        let versions = client.list_versions(&pr).unwrap();
        assert_eq!(versions, vec![SemVer::new(1, 0, 0), SemVer::new(1, 2, 0)]);

        // Generic download by exact version returns the bytes + canonical URL.
        let (bytes, url) = client.download_slpkg(&pr, SemVer::new(1, 2, 0)).unwrap();
        assert_eq!(bytes, slpkg_body);
        assert!(
            url.ends_with("/api/packages/tatolab/generic/escalate/1.2.0/escalate.slpkg"),
            "unexpected download url: {url}"
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

        let cfg = RegistryConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            token: None,
        };
        let client = RegistryClient::new(&cfg);
        let versions = client.list_versions(&pkg_ref("tatolab", "nope")).unwrap();
        assert!(versions.is_empty(), "404 index must list as empty, got {versions:?}");
        server.join().unwrap();
    }

    /// NDJSON render → parse is a faithful round-trip, name-filtered.
    /// Includes a prerelease entry — the index is how a published `-dev.N`
    /// becomes listable, so the round-trip must carry it.
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
        // One line per version, trailing newline, cargo-sparse `vers` field.
        assert_eq!(rendered.lines().count(), 4);
        assert!(rendered.contains("\"vers\":\"0.4.33\""));
        assert!(rendered.contains("\"vers\":\"0.4.33-dev.2\""));
        assert!(rendered.ends_with('\n'));
        let parsed = parse_index_ndjson(rendered.as_bytes(), "camera");
        assert_eq!(parsed, versions);
        // A line for a different package name is dropped by the filter.
        let mixed = format!("{rendered}{{\"name\":\"display\",\"vers\":\"9.9.9\"}}\n");
        assert_eq!(parse_index_ndjson(mixed.as_bytes(), "camera"), versions);
        // Blank lines and garbage degrade gracefully (skipped, not fatal).
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
            vec![SemVer::new(0, 9, 9), SemVer::new(1, 0, 0), SemVer::new(1, 2, 0)]
        );
    }

    /// Locks the **publish** path: `upload_slpkg` PUTs the `.slpkg`, then
    /// recomputes the version index from the authed management listing unioned
    /// with the just-published version and PUTs it to `index/index.json`. A
    /// stateful localhost server records every request; we assert the index
    /// PUT body is canonical NDJSON containing both the pre-existing version
    /// and the new one. Mentally revert the `write_version_index` call in
    /// `upload_slpkg`: no index PUT is recorded and the assertion fails.
    #[test]
    fn upload_writes_version_index_round_trip() {
        use std::io::{Read, Write};
        use std::sync::mpsc;

        // The management API reports one pre-existing version; the publish is
        // a new, higher version. The index must end up holding the union.
        let mgmt_json = r#"[{"name":"camera","version":"0.4.32","type":"generic"}]"#;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<(String, String, Vec<u8>)>();
        let mgmt = mgmt_json.to_string();
        let server = std::thread::spawn(move || {
            // upload_slpkg → 5 requests: DELETE slpkg ver, PUT slpkg,
            // GET mgmt list, DELETE index ver, PUT index.
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                // Read the full request: headers, then exactly Content-Length
                // body bytes (a single `read` can split header from body).
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let n = stream.read(&mut chunk).unwrap();
                    if n == 0 {
                        break raw.windows(4).position(|w| w == b"\r\n\r\n");
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header_str = String::from_utf8_lossy(&raw[..pos]).to_lowercase();
                        let want = header_str
                            .split("content-length:")
                            .nth(1)
                            .and_then(|s| s.split("\r\n").next())
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() - (pos + 4) >= want {
                            break Some(pos);
                        }
                    }
                };
                let pos = header_end.unwrap_or(raw.len().saturating_sub(0));
                let head = String::from_utf8_lossy(&raw[..pos]).to_string();
                let request_line = head.lines().next().unwrap_or("").to_string();
                let method = request_line.split(' ').next().unwrap_or("").to_string();
                let path = request_line.split(' ').nth(1).unwrap_or("").to_string();
                let body: Vec<u8> = raw.get(pos + 4..).map(|b| b.to_vec()).unwrap_or_default();
                tx.send((method.clone(), path.clone(), body)).unwrap();

                let resp_body: &[u8] = if method == "GET" {
                    mgmt.as_bytes()
                } else {
                    b""
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(resp_body).unwrap();
                stream.flush().unwrap();
            }
        });

        let cfg = RegistryConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            token: Some("publish-token".into()),
        };
        let client = RegistryClient::new(&cfg);
        let pr = pkg_ref("tatolab", "camera");
        client
            .upload_slpkg(&pr, SemVer::new(0, 4, 33), b"new-slpkg-bytes")
            .unwrap();
        server.join().unwrap();

        let reqs: Vec<(String, String, Vec<u8>)> = rx.try_iter().collect();
        // The index PUT carries the union of {0.4.32 (mgmt), 0.4.33 (new)}.
        let index_put = reqs
            .iter()
            .find(|(m, p, _)| m == "PUT" && p.ends_with("/index/index.json"))
            .expect("an index PUT must be recorded");
        let body = String::from_utf8_lossy(&index_put.2);
        let parsed = parse_index_ndjson(body.as_bytes(), "camera");
        assert_eq!(
            parsed,
            vec![SemVer::new(0, 4, 32), SemVer::new(0, 4, 33)],
            "index body must hold the management-listing ∪ published version, got {body:?}"
        );
        // The .slpkg itself was PUT too.
        assert!(
            reqs.iter().any(|(m, p, _)| m == "PUT"
                && p.ends_with("/generic/camera/0.4.33/camera.slpkg")),
            "the .slpkg PUT must be recorded; saw {reqs:?}"
        );
    }

    /// The release-manifest publish/fetch round-trip over the `file://`
    /// transport — the hermetic path CI and the scratch-registry integration
    /// test ride. Mentally revert `upload_release_manifest` to a no-op and
    /// `fetch_release_manifest` returns `None`, so this locks the write→read
    /// contract, not merely a happy path.
    #[test]
    fn release_manifest_file_scheme_round_trip() {
        use crate::release::{ReleaseManifest, ReleaseManifestMember};

        let tmp_guard = tempfile::tempdir().unwrap();
        let tmp = tmp_guard.path().to_path_buf();
        let cfg = RegistryConfig {
            base_url: format!("file://{}", tmp.display()),
            token: None,
        };
        let client = RegistryClient::new(&cfg);

        // Missing manifest ⇒ None (the pre-atomic-release back-compat case).
        assert!(client.fetch_release_manifest("tatolab", "0.5.1").unwrap().is_none());

        let mut manifest = ReleaseManifest::new(
            "0.5.1",
            vec![
                ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1"),
                ReleaseManifestMember::new("vulkan-jpeg", "0.5.1"),
            ],
        );
        manifest.python = Some("0.5.1".to_string());
        manifest.packages = vec![ReleaseManifestMember::new("@tatolab/jpeg", "1.0.7")];

        let url = client.upload_release_manifest("tatolab", &manifest).unwrap();
        assert!(url.ends_with("/generic/streamlib-release/0.5.1/manifest.json"), "url: {url}");
        // The mirror layout must be `<base>/streamlib-release/<V>/manifest.json`.
        assert!(tmp.join("streamlib-release").join("0.5.1").join("manifest.json").is_file());

        let back = client.fetch_release_manifest("tatolab", "0.5.1").unwrap().unwrap();
        assert_eq!(back, manifest);

        // The release channel is listable — the consumer's range-aware
        // completeness check discovers available releases this way.
        assert_eq!(
            client.list_release_versions("tatolab").unwrap(),
            vec![SemVer::new(0, 5, 1)]
        );
    }
}
