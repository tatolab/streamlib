// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The **release manifest** — the record that makes a published version
//! surface an *atomic, detectable* release.
//!
//! A release publishes a defined closure (all engine library crates + the
//! polyglot SDKs + all packages) as one unit. Individual crate/package
//! uploads are a loose pile until the manifest lands: the manifest is
//! written **last**, so its presence at `streamlib-release/<V>/manifest.json`
//! is the completion marker — a consumer that finds it knows every member it
//! lists is published, and a consumer that resolves a pin absent from it gets
//! an actionable "incomplete release" error up front instead of a cryptic
//! cargo/`streamlib-macros` version-unification failure deep in the build.
//!
//! The manifest is transport-agnostic: [`crate::RegistryClient`] writes/reads
//! it over `http(s)://` (Gitea generic registry) and `file://` (hermetic
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
/// Serialized to `streamlib-release/<V>/manifest.json` in the generic
/// registry and written **last** in the publish sequence — its presence
/// means the release is complete by protocol.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseManifest {
    /// Manifest schema version (forward-compat marker).
    pub manifest_format: u32,
    /// The release version `V` (the workspace `[workspace.package].version`,
    /// with a `-dev.N` suffix for a dev publish). The manifest lives under
    /// this exact string in the registry.
    pub release_version: String,
    /// The engine crate closure — every `streamlib*` / `vulkan-jpeg` library
    /// crate published for this release (the `compute_release_closure`
    /// output). This is the load-bearing set the consumer completeness check
    /// validates against.
    pub crates: Vec<ReleaseManifestMember>,
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
    /// Build a manifest for release `release_version` from its crate closure.
    /// `python` / `deno` / `packages` are filled in by the caller.
    pub fn new(release_version: impl Into<String>, crates: Vec<ReleaseManifestMember>) -> Self {
        Self {
            manifest_format: RELEASE_MANIFEST_FORMAT,
            release_version: release_version.into(),
            crates,
            python: None,
            deno: None,
            packages: Vec::new(),
        }
    }

    /// Whether the manifest lists a crate member named `name` at exactly
    /// `version`.
    pub fn contains_crate(&self, name: &str, version: &str) -> bool {
        self.crates
            .iter()
            .any(|m| m.name == name && m.version == version)
    }
}

/// Given the direct crate pins a consumer declares (`(name, exact version)`),
/// return the `name@version` of each pin the release `manifest` does **not**
/// list as a crate member. An empty result means every pin is covered by the
/// release — a partial/mid-publish registry yields the missing names so the
/// caller can name the gap.
///
/// The pins are the consumer's *direct* gitea-registry cargo deps at the
/// release version; the manifest's `crates` set is the full published
/// closure. A pin absent from the closure is exactly the `0.4.36`
/// `streamlib-plugin-sdk` / `vulkan-jpeg` foot-gun this manifest exists to
/// catch.
pub fn crates_missing_from_release(
    manifest: &ReleaseManifest,
    required: &[(String, String)],
) -> Vec<String> {
    let mut missing: Vec<String> = required
        .iter()
        .filter(|(name, version)| !manifest.contains_crate(name, version))
        .map(|(name, version)| format!("{name}@{version}"))
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(crates: &[(&str, &str)]) -> ReleaseManifest {
        ReleaseManifest::new(
            "0.5.1",
            crates
                .iter()
                .map(|(n, v)| ReleaseManifestMember::new(*n, *v))
                .collect(),
        )
    }

    #[test]
    fn complete_release_reports_no_missing() {
        let m = manifest_with(&[
            ("streamlib-plugin-sdk", "0.5.1"),
            ("streamlib-macros", "0.5.1"),
            ("vulkan-jpeg", "0.5.1"),
        ]);
        let required = vec![
            ("streamlib-plugin-sdk".to_string(), "0.5.1".to_string()),
            ("streamlib-macros".to_string(), "0.5.1".to_string()),
        ];
        assert!(crates_missing_from_release(&m, &required).is_empty());
    }

    #[test]
    fn partial_release_names_the_gap() {
        // The historical foot-gun: a closure that published streamlib-macros
        // but silently skipped streamlib-plugin-sdk + vulkan-jpeg. The check
        // must name exactly the pins that aren't in the manifest.
        let m = manifest_with(&[("streamlib-macros", "0.5.1")]);
        let required = vec![
            ("streamlib-plugin-sdk".to_string(), "0.5.1".to_string()),
            ("streamlib-macros".to_string(), "0.5.1".to_string()),
            ("vulkan-jpeg".to_string(), "0.5.1".to_string()),
        ];
        let missing = crates_missing_from_release(&m, &required);
        assert_eq!(
            missing,
            vec![
                "streamlib-plugin-sdk@0.5.1".to_string(),
                "vulkan-jpeg@0.5.1".to_string(),
            ]
        );
    }

    #[test]
    fn version_mismatch_counts_as_missing() {
        // A pin at a version the release doesn't carry (skew) is missing —
        // string-exact match, since the manifest records stamped versions.
        let m = manifest_with(&[("streamlib-macros", "0.5.1")]);
        let required = vec![("streamlib-macros".to_string(), "0.4.36".to_string())];
        assert_eq!(
            crates_missing_from_release(&m, &required),
            vec!["streamlib-macros@0.4.36".to_string()]
        );
    }

    #[test]
    fn manifest_json_round_trips_both_optional_and_packages() {
        let mut m = manifest_with(&[("streamlib-plugin-sdk", "0.5.1")]);
        m.python = Some("0.5.1".to_string());
        m.deno = Some("0.5.1".to_string());
        m.packages = vec![ReleaseManifestMember::new("@tatolab/jpeg", "1.0.7")];
        let json = serde_json::to_string_pretty(&m).unwrap();
        let back: ReleaseManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_read_ignores_unknown_fields() {
        // Forward-compat: an older reader must tolerate a newer manifest that
        // grew a field. Reverting `#[serde(default)]` / unknown-field
        // tolerance would break older consumers against a newer registry.
        let json = r#"{
            "manifest_format": 1,
            "release_version": "0.5.1",
            "crates": [{"name":"streamlib-macros","version":"0.5.1"}],
            "future_field": {"whatever": true}
        }"#;
        let m: ReleaseManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.release_version, "0.5.1");
        assert!(m.contains_crate("streamlib-macros", "0.5.1"));
        assert!(m.packages.is_empty());
    }
}
