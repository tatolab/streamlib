// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side release-completeness pre-check.
//!
//! Before a package's Rust build resolves its gitea-registry dependencies via
//! cargo, this checks the registry's **release manifest** for the pinned
//! version. A partial / mid-publish registry — the historical `0.4.36`
//! `streamlib-plugin-sdk` + `vulkan-jpeg` foot-gun — fails fast here with a
//! typed [`BuildError::IncompleteRelease`] naming the exact missing artifacts,
//! instead of surfacing much later as a cryptic
//! `failed to select a version for streamlib-plugin-sdk = ^0.5.1` deep in
//! cargo / `streamlib-macros` version unification.
//!
//! The check is deliberately conservative — it only ever *adds* an
//! actionable early failure for a provably-partial release. It is a no-op
//! (build proceeds) when:
//!
//! - no registry is configured (in-tree / dev builds resolve deps by `path`);
//! - the package declares no gitea-registry pins;
//! - the registry has no release manifest for a pinned version (a
//!   pre-atomic-release registry — logged, then proceed); or
//! - the manifest fetch hits a transient transport error (the real cargo
//!   resolve remains the hard gate — a network blip must not manufacture a
//!   build failure).

use std::collections::BTreeMap;

use streamlib_engine::core::runtime::BuildError;
use streamlib_idents::{crates_missing_from_release, RegistryClient, RegistryConfig};

/// Registry org the release manifest lives under. Matches the publish
/// scripts' `GITEA_ORG` default.
fn registry_org() -> String {
    std::env::var("GITEA_ORG")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tatolab".to_string())
}

/// Fail fast with [`BuildError::IncompleteRelease`] when the configured
/// registry holds a partial release for any of `required`'s pinned versions.
///
/// `required` is the consumer's direct gitea-registry pins as
/// `(crate name, exact version)`. Grouped by version, each version's release
/// manifest is fetched once; a pin absent from a *present* manifest is the
/// gap. See the module docs for the no-op cases.
pub(crate) fn assert_release_complete(
    package_label: &str,
    required: &[(String, String)],
) -> Result<(), BuildError> {
    if required.is_empty() {
        return Ok(());
    }
    // No registry configured ⇒ deps resolve by path (dev / in-tree); nothing
    // to validate against.
    let Some(config) = RegistryConfig::from_env() else {
        return Ok(());
    };
    let client = RegistryClient::new(&config);
    let org = registry_org();

    // Group pins by their pinned version so each release manifest is fetched
    // once. A coherent release shares one version across every engine pin;
    // multiple versions here would itself be the skew this catches.
    let mut by_version: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, version) in required {
        by_version
            .entry(version.clone())
            .or_default()
            .push((name.clone(), version.clone()));
    }

    let mut missing_all: Vec<String> = Vec::new();
    let mut incomplete_versions: Vec<String> = Vec::new();
    for (version, pins) in &by_version {
        match client.fetch_release_manifest(&org, version) {
            Ok(Some(manifest)) => {
                let missing = crates_missing_from_release(&manifest, pins);
                if !missing.is_empty() {
                    incomplete_versions.push(version.clone());
                    missing_all.extend(missing);
                }
            }
            Ok(None) => {
                tracing::warn!(
                    package = %package_label,
                    %version,
                    "registry has no release manifest for {version} — pre-atomic-release \
                     registry; proceeding without completeness check (a partial release will \
                     surface as a cargo resolve error)"
                );
            }
            Err(e) => {
                // A transient fetch failure must not turn into a hard build
                // failure — the cargo resolve that follows is the real gate.
                tracing::warn!(
                    package = %package_label,
                    %version,
                    error = %e,
                    "release manifest fetch failed — skipping completeness check for this version"
                );
            }
        }
    }

    if missing_all.is_empty() {
        return Ok(());
    }
    missing_all.sort();
    missing_all.dedup();
    incomplete_versions.sort();
    incomplete_versions.dedup();
    Err(BuildError::IncompleteRelease {
        package: package_label.to_string(),
        release_version: incomplete_versions.join(", "),
        missing: missing_all.join(", "),
        hint: "the registry has a partial or inconsistent release — re-run the release publish \
               (scripts/gitea/publish-release.sh) so the full closure lands, or pin a version \
               whose release manifest lists every dependency"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{ReleaseManifest, ReleaseManifestMember};

    /// Serialize env mutation across the tests in this module — they set /
    /// clear `STREAMLIB_REGISTRY_URL` process-wide.
    fn with_file_registry<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        // Snapshot + restore the registry env so tests don't leak into each
        // other or the wider suite. SAFETY: callers are `#[serial]`, so no
        // other thread races these process-global env writes.
        let prev_url = std::env::var("STREAMLIB_REGISTRY_URL").ok();
        let prev_fallback = std::env::var("GITEA_URL").ok();
        unsafe {
            std::env::set_var("STREAMLIB_REGISTRY_URL", format!("file://{}", dir.display()));
            std::env::remove_var("GITEA_URL");
        }
        let out = f();
        unsafe {
            match prev_url {
                Some(v) => std::env::set_var("STREAMLIB_REGISTRY_URL", v),
                None => std::env::remove_var("STREAMLIB_REGISTRY_URL"),
            }
            if let Some(v) = prev_fallback {
                std::env::set_var("GITEA_URL", v);
            }
        }
        out
    }

    fn publish_manifest(dir: &std::path::Path, m: &ReleaseManifest) {
        let cfg = RegistryConfig {
            base_url: format!("file://{}", dir.display()),
            token: None,
        };
        RegistryClient::new(&cfg)
            .upload_release_manifest("tatolab", m)
            .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn no_registry_configured_is_a_noop() {
        // Clear the env entirely; the check must pass vacuously (dev / path).
        // SAFETY: `#[serial]` — no other thread races these env writes.
        let prev = std::env::var("STREAMLIB_REGISTRY_URL").ok();
        let prev_g = std::env::var("GITEA_URL").ok();
        unsafe {
            std::env::remove_var("STREAMLIB_REGISTRY_URL");
            std::env::remove_var("GITEA_URL");
        }
        let required = vec![("streamlib-plugin-sdk".to_string(), "0.5.1".to_string())];
        assert!(assert_release_complete("pkg", &required).is_ok());
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("STREAMLIB_REGISTRY_URL", v);
            }
            if let Some(v) = prev_g {
                std::env::set_var("GITEA_URL", v);
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn partial_release_fails_fast_naming_the_gap() {
        let tmp = tempfile::tempdir().unwrap();
        // Manifest omits vulkan-jpeg + streamlib-plugin-sdk — the foot-gun.
        let manifest = ReleaseManifest::new(
            "0.5.1",
            vec![ReleaseManifestMember::new("streamlib-macros", "0.5.1")],
        );
        publish_manifest(tmp.path(), &manifest);
        let required = vec![
            ("streamlib-plugin-sdk".to_string(), "0.5.1".to_string()),
            ("streamlib-macros".to_string(), "0.5.1".to_string()),
            ("vulkan-jpeg".to_string(), "0.5.1".to_string()),
        ];
        let err = with_file_registry(tmp.path(), || {
            assert_release_complete("streamlib-jpeg", &required).unwrap_err()
        });
        match err {
            BuildError::IncompleteRelease {
                release_version,
                missing,
                ..
            } => {
                assert_eq!(release_version, "0.5.1");
                assert!(missing.contains("streamlib-plugin-sdk@0.5.1"), "missing: {missing}");
                assert!(missing.contains("vulkan-jpeg@0.5.1"), "missing: {missing}");
                assert!(!missing.contains("streamlib-macros"), "macros is present: {missing}");
            }
            other => panic!("expected IncompleteRelease, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn complete_release_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = ReleaseManifest::new(
            "0.5.1",
            vec![
                ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1"),
                ReleaseManifestMember::new("streamlib-macros", "0.5.1"),
                ReleaseManifestMember::new("vulkan-jpeg", "0.5.1"),
            ],
        );
        publish_manifest(tmp.path(), &manifest);
        let required = vec![
            ("streamlib-plugin-sdk".to_string(), "0.5.1".to_string()),
            ("vulkan-jpeg".to_string(), "0.5.1".to_string()),
        ];
        let out = with_file_registry(tmp.path(), || assert_release_complete("pkg", &required));
        assert!(out.is_ok(), "complete release must pass: {out:?}");
    }

    #[test]
    #[serial_test::serial]
    fn mavlink_1213_scenario_against_file_registry() {
        // The live #1213 failure class, hermetically: the real
        // packages/mavlink Cargo.toml pins streamlib-plugin-sdk from the gitea
        // registry — the exact crate the 0.4.36 partial publish silently
        // skipped. Against a registry whose manifest OMITS plugin-sdk (a
        // partial release that "looks complete"), the pre-check must fast-fail
        // naming plugin-sdk; against a COMPLETE manifest it resolves clean.
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root two levels above the orchestrator crate")
            .to_path_buf();
        let mavlink_dir = workspace_root.join("packages").join("mavlink");

        let pins = streamlib_cargo_build::read_gitea_registry_pins(&mavlink_dir)
            .expect("read mavlink gitea pins");
        assert!(
            pins.iter().any(|p| p.name == "streamlib-plugin-sdk"),
            "packages/mavlink must pin streamlib-plugin-sdk from gitea (the #1213 crate); \
             got {pins:?}"
        );
        // All engine pins share the release version in a coherent release.
        let version = pins
            .iter()
            .find(|p| p.name == "streamlib-plugin-sdk")
            .map(|p| p.version.clone())
            .unwrap();
        let required: Vec<(String, String)> =
            pins.iter().map(|p| (p.name.clone(), p.version.clone())).collect();

        let tmp = tempfile::tempdir().unwrap();
        let cfg = RegistryConfig {
            base_url: format!("file://{}", tmp.path().display()),
            token: None,
        };

        // Partial manifest: every mavlink pin EXCEPT plugin-sdk.
        let partial_members: Vec<ReleaseManifestMember> = required
            .iter()
            .filter(|(name, _)| name != "streamlib-plugin-sdk")
            .map(|(n, v)| ReleaseManifestMember::new(n.clone(), v.clone()))
            .collect();
        let partial = ReleaseManifest::new(version.clone(), partial_members);
        RegistryClient::new(&cfg)
            .upload_release_manifest("tatolab", &partial)
            .unwrap();

        let err = with_file_registry(tmp.path(), || {
            assert_release_complete("@tatolab/mavlink", &required).unwrap_err()
        });
        match err {
            BuildError::IncompleteRelease { missing, .. } => {
                assert!(
                    missing.contains(&format!("streamlib-plugin-sdk@{version}")),
                    "the #1213 gap (streamlib-plugin-sdk) must be named; got: {missing}"
                );
            }
            other => panic!("expected IncompleteRelease naming plugin-sdk, got {other:?}"),
        }

        // Complete manifest: now includes plugin-sdk ⇒ clean resolve.
        let complete = ReleaseManifest::new(
            version.clone(),
            required
                .iter()
                .map(|(n, v)| ReleaseManifestMember::new(n.clone(), v.clone()))
                .collect(),
        );
        RegistryClient::new(&cfg)
            .upload_release_manifest("tatolab", &complete)
            .unwrap();
        let out = with_file_registry(tmp.path(), || {
            assert_release_complete("@tatolab/mavlink", &required)
        });
        assert!(out.is_ok(), "a complete release must resolve clean: {out:?}");
    }

    #[test]
    #[serial_test::serial]
    fn absent_manifest_proceeds_back_compat() {
        // No manifest published for the pinned version ⇒ pre-atomic-release
        // registry ⇒ warn + proceed (Ok). Mentally revert the Ok(None) arm to
        // an error and this would wrongly block every pre-#1218 registry.
        let tmp = tempfile::tempdir().unwrap();
        let required = vec![("streamlib-plugin-sdk".to_string(), "0.5.1".to_string())];
        let out = with_file_registry(tmp.path(), || assert_release_complete("pkg", &required));
        assert!(out.is_ok(), "absent manifest must proceed (back-compat): {out:?}");
    }
}
