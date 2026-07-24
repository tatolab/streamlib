// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The **release manifest** — the record that makes a published version
//! surface an *atomic, detectable* release.
//!
//! A release publishes the polyglot SDKs + all `.slpkg` packages as one unit.
//! Individual uploads are a loose pile until the manifest lands: the manifest
//! is written **last**, so its presence at `streamlib-release/<V>/manifest.json`
//! is the completion marker — a consumer that finds it knows every member it
//! lists is published.
//!
//! The manifest is transport-agnostic: [`crate::PackageSourceClient`] writes/reads
//! it over `http(s)://` (static generic store) and `file://` (hermetic
//! local mirror) identically.

/// One published member of a release — a crate or a package, named and
/// versioned. Extra fields a future manifest revision might carry are
/// ignored on read, so the shape can grow without breaking older readers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseManifestMember {
    pub name: String,
    pub version: String,
}

impl ReleaseManifestMember {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// Current [`ReleaseManifest`] schema version. Bumped only on a
/// breaking-shape change; readers ignore unknown fields, so additive
/// growth does not require a bump.
pub const RELEASE_MANIFEST_FORMAT: u32 = 1;

/// The complete set of artifacts that constitute release version `V`.
///
/// Serialized to `streamlib-release/<V>/manifest.json` in the generic store of the
/// package source, written **last** in the publish sequence — its presence
/// means the release is complete by protocol.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseManifest {
    /// Manifest schema version (forward-compat marker).
    pub manifest_format: u32,
    /// The release version `V` (the workspace `[workspace.package].version`,
    /// with a `-dev.N` suffix for a dev publish). The manifest lives under
    /// this exact string in the package source.
    pub release_version: String,
    /// Python SDK version published for this release, if the SDK was part of
    /// the release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    /// Deno SDK version published for this release, if the SDK was part of
    /// the release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deno: Option<String>,
    /// Polyglot packages (`.slpkg`) published for this release, by their
    /// own independent semver.
    #[serde(default)]
    pub packages: Vec<ReleaseManifestMember>,
}

impl ReleaseManifest {
    /// Build a manifest for release `release_version`. `python` / `deno` /
    /// `packages` are filled in by the caller.
    pub fn new(release_version: impl Into<String>) -> Self {
        Self {
            manifest_format: RELEASE_MANIFEST_FORMAT,
            release_version: release_version.into(),
            python: None,
            deno: None,
            packages: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_packages(packages: &[(&str, &str)]) -> ReleaseManifest {
        let mut m = ReleaseManifest::new("0.5.1");
        m.packages = packages
            .iter()
            .map(|(n, v)| ReleaseManifestMember::new(*n, *v))
            .collect();
        m
    }

    #[test]
    fn manifest_json_round_trips_both_optional_and_packages() {
        let mut m = manifest_with_packages(&[("@tatolab/camera", "1.0.0")]);
        m.python = Some("0.5.1".to_string());
        m.deno = Some("0.5.1".to_string());
        m.packages
            .push(ReleaseManifestMember::new("@tatolab/jpeg", "1.0.7"));
        let json = serde_json::to_string_pretty(&m).unwrap();
        let back: ReleaseManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_read_ignores_unknown_fields() {
        // Forward-compat: an older reader must tolerate a newer manifest that
        // grew a field (and a retired one — a legacy `crates` field from a
        // pre-removal manifest is silently ignored). Reverting `#[serde(default)]`
        // / unknown-field tolerance would break older consumers against a newer
        // package source.
        let json = r#"{
            "manifest_format": 1,
            "release_version": "0.5.1",
            "crates": [{"name":"streamlib-macros","version":"0.5.1"}],
            "packages": [{"name":"@tatolab/camera","version":"1.0.0"}],
            "future_field": {"whatever": true}
        }"#;
        let m: ReleaseManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.release_version, "0.5.1");
        assert!(
            m.packages
                .iter()
                .any(|p| p.name == "@tatolab/camera" && p.version == "1.0.0")
        );
    }
}
