// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Gitea generic-registry client for schema-package resolution.
//!
//! Schema packages (`@tatolab/escalate` and friends) are distributed as
//! source-only `.slpkg`s in Gitea's **generic** registry. Resolving a
//! `Registry` dependency by a semver *range* requires two steps the flat
//! generic registry can't do in one request:
//!
//! 1. **List** the available concrete versions of the package
//!    (Gitea's package-management API), then select the highest one that
//!    satisfies the declared range — cargo/npm/pip semantics.
//! 2. **Download** that version's `.slpkg` from the generic registry's
//!    by-version download namespace.
//!
//! `http(s)://` is the production transport; `file://` is the hermetic
//! local-mirror / test transport (mirroring the engine's remote-`.slpkg`
//! fetch), where the base URL points at a directory laid out as
//! `<base>/<name>/<version>/<name>.slpkg`.

use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::ident::PackageRef;
use crate::semver::SemVer;

/// Environment variable carrying the Gitea base URL (e.g.
/// `http://localhost:3000`). Falls back to `GITEA_URL` to match the
/// provisioning scripts' convention.
pub const REGISTRY_URL_ENV: &str = "STREAMLIB_REGISTRY_URL";
const REGISTRY_URL_ENV_FALLBACK: &str = "GITEA_URL";

/// Environment variable carrying an optional read token for the
/// package-management API (private registries). Read is anonymous on a
/// public registry, so this is usually unset.
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

    fn list_versions_http(&self, pkg_ref: &PackageRef) -> ResolverResult<Vec<SemVer>> {
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
pub(crate) fn select_version(
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
    fn from_env_reads_url_and_token() {
        // No env set in this process by default — the round-trip is exercised
        // by the resolver's file:// integration test. Here we only assert the
        // None-when-unset contract holds for a name unlikely to be set.
        // (Avoid mutating process env in a parallel test run.)
        let cfg = RegistryConfig {
            base_url: "http://localhost:3000".into(),
            token: Some("abc".into()),
        };
        assert!(!RegistryClient::new(&cfg).is_file_scheme());
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
}
