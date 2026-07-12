// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side release-completeness pre-check.
//!
//! Before a package's Rust build resolves its gitea-registry dependencies via
//! cargo, this checks the registry's **release manifests** against the
//! package's pins. A partial / mid-publish registry — the historical `0.4.36`
//! `streamlib-plugin-sdk` + `vulkan-jpeg` foot-gun — fails fast here with a
//! typed [`BuildError::IncompleteRelease`] naming the exact missing artifacts,
//! instead of surfacing much later as a cryptic
//! `failed to select a version for streamlib-plugin-sdk = ^0.5.1` deep in
//! cargo / `streamlib-macros` version unification.
//!
//! Resolution is **range-aware**: consumer pins are floor reqs (`0.5.0`
//! means `^0.5.0` per cargo) that typically lag the released version
//! (`0.5.1`), so each pin is validated against the *newest listed release
//! whose version satisfies the pin's range* — the release cargo's own
//! max-satisfying-version resolution will predominantly land on. When no
//! listed release satisfies (older exact pins, pre-index registries), the
//! check falls back to fetching the manifest at the pin's floor version.
//!
//! The check is deliberately conservative — it only ever *adds* an
//! actionable early failure for a provably-partial release. It is a no-op
//! (build proceeds) when:
//!
//! - no registry is configured (in-tree / dev builds resolve deps by `path`);
//! - the package declares no gitea-registry pins;
//! - no release manifest covers a pin's range (a pre-atomic-release
//!   registry — logged, then proceed); or
//! - the manifest fetch / listing hits a transient transport error (the real
//!   cargo resolve remains the hard gate — a network blip must not
//!   manufacture a build failure).

use std::collections::BTreeMap;

use streamlib_cargo_build::TatolabRegistryPin;
use streamlib_engine::core::runtime::BuildError;
use streamlib_idents::{
    crates_missing_from_release, RegistryClient, RegistryConfig, SemVer, SemVerRange,
};

/// Registry org the release manifest lives under. Matches the publish
/// scripts' `STREAMLIB_REGISTRY_ORG` default.
fn registry_org() -> String {
    std::env::var("STREAMLIB_REGISTRY_ORG")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tatolab".to_string())
}

/// Map a raw cargo version req to a [`SemVerRange`]. Cargo's bare `0.5.0`
/// means caret; explicit `^` / `~` / `>=` / `=` prefixes parse as-is.
/// `None` for req shapes the range parser doesn't cover (multi-clause
/// reqs) — those pins are skipped rather than misjudged.
fn cargo_req_to_range(req: &str) -> Option<SemVerRange> {
    let req = req.trim();
    let normalized = if req.starts_with(['^', '~', '=', '>', '<', '*']) {
        req.to_string()
    } else {
        // Bare version → caret, per cargo semantics.
        format!("^{req}")
    };
    SemVerRange::from_str(&normalized).ok()
}

/// Fail fast with [`BuildError::IncompleteRelease`] when the configured
/// registry's release manifests cannot satisfy the package's direct
/// gitea-registry `pins`. See the module docs for the resolution model and
/// the no-op cases.
pub(crate) fn assert_release_complete(
    package_label: &str,
    pins: &[TatolabRegistryPin],
) -> Result<(), BuildError> {
    if pins.is_empty() {
        return Ok(());
    }
    // No registry configured ⇒ deps resolve by path (dev / in-tree); nothing
    // to validate against.
    let Some(config) = RegistryConfig::from_env() else {
        return Ok(());
    };
    let client = RegistryClient::new(&config);
    let org = registry_org();

    // Available releases (newest-satisfying selection below). A listing
    // failure degrades to the exact-floor fallback per pin.
    let releases: Vec<SemVer> = match client.list_release_versions(&org) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                package = %package_label,
                error = %e,
                "release-version listing failed — falling back to exact-pin manifest lookups"
            );
            Vec::new()
        }
    };

    // Resolve each pin to the release manifest that should cover it, then
    // group so each manifest is fetched once.
    let mut by_release: BTreeMap<String, Vec<(String, SemVerRange)>> = BTreeMap::new();
    for pin in pins {
        let Some(range) = cargo_req_to_range(&pin.req) else {
            tracing::warn!(
                package = %package_label,
                crate_name = %pin.name,
                req = %pin.req,
                "unsupported version-req shape — skipping release-completeness check for this pin"
            );
            continue;
        };
        // Newest listed release satisfying the pin's range — the release
        // cargo's max-satisfying resolution will predominantly land on.
        // Fall back to the pin's floor version (covers exact pins on
        // registries whose release predates the listing index).
        let target = releases
            .iter()
            .filter(|v| range.matches(**v))
            .max()
            .map(|v| v.to_string())
            .unwrap_or_else(|| pin.version.clone());
        by_release.entry(target).or_default().push((pin.name.clone(), range));
    }

    let mut missing_all: Vec<String> = Vec::new();
    let mut incomplete_versions: Vec<String> = Vec::new();
    for (release_version, required) in &by_release {
        match client.fetch_release_manifest(&org, release_version) {
            Ok(Some(manifest)) => {
                let missing = crates_missing_from_release(&manifest, required);
                if !missing.is_empty() {
                    incomplete_versions.push(release_version.clone());
                    missing_all.extend(missing);
                }
            }
            Ok(None) => {
                tracing::warn!(
                    package = %package_label,
                    version = %release_version,
                    "registry has no release manifest for {release_version} — pre-atomic-release \
                     registry; proceeding without completeness check (a partial release will \
                     surface as a cargo resolve error)"
                );
            }
            Err(e) => {
                // A transient fetch failure must not turn into a hard build
                // failure — the cargo resolve that follows is the real gate.
                tracing::warn!(
                    package = %package_label,
                    version = %release_version,
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
               (cargo xtask static-registry emit) so the full closure lands, or pin a version \
               whose release manifest lists every dependency"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{ReleaseManifest, ReleaseManifestMember};

    fn pin(name: &str, req: &str) -> TatolabRegistryPin {
        TatolabRegistryPin {
            name: name.to_string(),
            req: req.to_string(),
            version: req.trim_start_matches(['=', '^', '~', '>', '<']).trim().to_string(),
        }
    }

    /// Point the registry env at `dir` for the duration of `f`, restoring the
    /// prior values after. SAFETY: callers are `#[serial]`, so no other
    /// thread races these process-global env writes.
    fn with_file_registry<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let prev_url = std::env::var("STREAMLIB_REGISTRY_URL").ok();
        unsafe {
            std::env::set_var("STREAMLIB_REGISTRY_URL", format!("file://{}", dir.display()));
        }
        let out = f();
        unsafe {
            match prev_url {
                Some(v) => std::env::set_var("STREAMLIB_REGISTRY_URL", v),
                None => std::env::remove_var("STREAMLIB_REGISTRY_URL"),
            }
        }
        out
    }

    fn publish_manifest(dir: &std::path::Path, m: &ReleaseManifest) {
        let cfg = RegistryConfig {
            base_url: format!("file://{}", dir.display()),
        };
        RegistryClient::new(&cfg)
            .upload_release_manifest("tatolab", m)
            .unwrap();
    }

    #[test]
    fn cargo_req_mapping_bare_is_caret_and_operators_pass_through() {
        assert_eq!(cargo_req_to_range("0.5.0"), SemVerRange::from_str("^0.5.0").ok());
        assert_eq!(cargo_req_to_range("=0.4.36"), SemVerRange::from_str("=0.4.36").ok());
        assert_eq!(cargo_req_to_range("^0.5.1"), SemVerRange::from_str("^0.5.1").ok());
        assert_eq!(cargo_req_to_range(">=0.5.0"), SemVerRange::from_str(">=0.5.0").ok());
    }

    #[test]
    #[serial_test::serial]
    fn no_registry_configured_is_a_noop() {
        // Clear the env entirely; the check must pass vacuously (dev / path).
        // SAFETY: `#[serial]` — no other thread races these env writes.
        let prev = std::env::var("STREAMLIB_REGISTRY_URL").ok();
        unsafe {
            std::env::remove_var("STREAMLIB_REGISTRY_URL");
        }
        let pins = vec![pin("streamlib-plugin-sdk", "0.5.1")];
        assert!(assert_release_complete("pkg", &pins).is_ok());
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("STREAMLIB_REGISTRY_URL", v);
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn floor_pin_resolves_against_newest_satisfying_release() {
        // The production steady state: consumer pins floor `0.5.0` (bare =
        // caret) while the registry's release is `0.5.1`. The check must key
        // on the NEWEST satisfying release, not the pin's floor — mentally
        // revert to exact-floor keying and the fetch at 0.5.0 finds nothing,
        // the partial 0.5.1 release sails through, and this test fails.
        let tmp = tempfile::tempdir().unwrap();
        // Partial 0.5.1 release: macros present, plugin-sdk missing.
        let partial = ReleaseManifest::new(
            "0.5.1",
            vec![ReleaseManifestMember::new("streamlib-macros", "0.5.1")],
        );
        publish_manifest(tmp.path(), &partial);
        let pins = vec![
            pin("streamlib-plugin-sdk", "0.5.0"),
            pin("streamlib-macros", "0.5.0"),
        ];
        let err = with_file_registry(tmp.path(), || {
            assert_release_complete("pkg", &pins).unwrap_err()
        });
        match err {
            BuildError::IncompleteRelease { release_version, missing, .. } => {
                assert_eq!(release_version, "0.5.1");
                assert!(missing.contains("streamlib-plugin-sdk@^0.5.0"), "missing: {missing}");
                assert!(!missing.contains("streamlib-macros"), "macros satisfied: {missing}");
            }
            other => panic!("expected IncompleteRelease, got {other:?}"),
        }

        // Complete 0.5.1 release ⇒ the same floor pins pass.
        let complete = ReleaseManifest::new(
            "0.5.1",
            vec![
                ReleaseManifestMember::new("streamlib-macros", "0.5.1"),
                ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1"),
            ],
        );
        publish_manifest(tmp.path(), &complete);
        let out = with_file_registry(tmp.path(), || assert_release_complete("pkg", &pins));
        assert!(out.is_ok(), "complete newest release must pass: {out:?}");
    }

    #[test]
    #[serial_test::serial]
    fn exact_pin_checks_its_own_release() {
        // An exact `=` pin (the jpeg-package shape) validates against the
        // release at that exact version, not the newest one.
        let tmp = tempfile::tempdir().unwrap();
        let old = ReleaseManifest::new(
            "0.4.36",
            vec![ReleaseManifestMember::new("streamlib-macros", "0.4.36")],
        );
        let newest = ReleaseManifest::new(
            "0.5.1",
            vec![
                ReleaseManifestMember::new("streamlib-macros", "0.5.1"),
                ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1"),
            ],
        );
        publish_manifest(tmp.path(), &old);
        publish_manifest(tmp.path(), &newest);
        // plugin-sdk pinned exactly at 0.4.36 — absent from the 0.4.36
        // release even though the newest release carries it.
        let pins = vec![pin("streamlib-plugin-sdk", "=0.4.36")];
        let err = with_file_registry(tmp.path(), || {
            assert_release_complete("streamlib-jpeg", &pins).unwrap_err()
        });
        match err {
            BuildError::IncompleteRelease { release_version, missing, .. } => {
                assert_eq!(release_version, "0.4.36");
                assert!(missing.contains("streamlib-plugin-sdk@=0.4.36"), "missing: {missing}");
            }
            other => panic!("expected IncompleteRelease, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn mavlink_1213_scenario_against_file_registry() {
        // The live #1213 failure class, hermetically: the real
        // packages/mavlink Cargo.toml pins streamlib-plugin-sdk from the gitea
        // registry — the exact crate the 0.4.36 partial publish silently
        // skipped. Against a registry whose newest release manifest OMITS
        // plugin-sdk (a partial release that "looks complete"), the pre-check
        // must fast-fail naming plugin-sdk; against a COMPLETE manifest it
        // resolves clean. The manifest is deliberately published at a HIGHER
        // patch than the pins' floor, mirroring the real tree (pins 0.5.0,
        // release 0.5.1).
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root two levels above the orchestrator crate")
            .to_path_buf();
        let mavlink_dir = workspace_root.join("packages").join("mavlink");

        let pins = streamlib_cargo_build::read_tatolab_registry_pins(&mavlink_dir)
            .expect("read mavlink gitea pins");
        assert!(
            pins.iter().any(|p| p.name == "streamlib-plugin-sdk"),
            "packages/mavlink must pin streamlib-plugin-sdk from gitea (the #1213 crate); \
             got {pins:?}"
        );
        let floor: SemVer = pins
            .iter()
            .find(|p| p.name == "streamlib-plugin-sdk")
            .and_then(|p| p.version.parse().ok())
            .expect("plugin-sdk floor version parses");
        // Release at floor + one patch — the version cargo would resolve the
        // caret floor pins up to.
        let release_version = SemVer::new(floor.major, floor.minor, floor.patch + 1).to_string();

        let tmp = tempfile::tempdir().unwrap();

        // Partial release: every mavlink pin EXCEPT plugin-sdk.
        let partial_members: Vec<ReleaseManifestMember> = pins
            .iter()
            .filter(|p| p.name != "streamlib-plugin-sdk")
            .map(|p| ReleaseManifestMember::new(p.name.clone(), release_version.clone()))
            .collect();
        let partial = ReleaseManifest::new(release_version.clone(), partial_members);
        publish_manifest(tmp.path(), &partial);

        let err = with_file_registry(tmp.path(), || {
            assert_release_complete("@tatolab/mavlink", &pins).unwrap_err()
        });
        match err {
            BuildError::IncompleteRelease { missing, .. } => {
                assert!(
                    missing.contains("streamlib-plugin-sdk@"),
                    "the #1213 gap (streamlib-plugin-sdk) must be named; got: {missing}"
                );
            }
            other => panic!("expected IncompleteRelease naming plugin-sdk, got {other:?}"),
        }

        // Complete release ⇒ clean resolve.
        let complete = ReleaseManifest::new(
            release_version.clone(),
            pins.iter()
                .map(|p| ReleaseManifestMember::new(p.name.clone(), release_version.clone()))
                .collect(),
        );
        publish_manifest(tmp.path(), &complete);
        let out = with_file_registry(tmp.path(), || {
            assert_release_complete("@tatolab/mavlink", &pins)
        });
        assert!(out.is_ok(), "a complete release must resolve clean: {out:?}");
    }

    #[test]
    #[serial_test::serial]
    fn unreachable_registry_degrades_to_proceed() {
        // The module-doc promise: a network blip must not manufacture a
        // build failure. Point the registry at a connection-refused endpoint
        // — both the release listing and the fallback manifest fetch error,
        // and the check must degrade to Ok (cargo remains the hard gate).
        // Mentally revert the Err arms to hard failures and this fails.
        // SAFETY: `#[serial]` — no other thread races these env writes.
        let prev_url = std::env::var("STREAMLIB_REGISTRY_URL").ok();
        unsafe {
            std::env::set_var("STREAMLIB_REGISTRY_URL", "http://127.0.0.1:1");
        }
        let pins = vec![pin("streamlib-plugin-sdk", "0.5.0")];
        let out = assert_release_complete("pkg", &pins);
        unsafe {
            match prev_url {
                Some(v) => std::env::set_var("STREAMLIB_REGISTRY_URL", v),
                None => std::env::remove_var("STREAMLIB_REGISTRY_URL"),
            }
        }
        assert!(out.is_ok(), "transport errors must degrade to proceed: {out:?}");
    }

    #[test]
    fn incomplete_release_error_renders_version_missing_and_hint() {
        // The error string is the user-facing artifact — it must carry the
        // release version, every missing name@req, and the actionable hint.
        let err = BuildError::IncompleteRelease {
            package: "@tatolab/mavlink".to_string(),
            release_version: "0.5.1".to_string(),
            missing: "streamlib-plugin-sdk@^0.5.0, vulkan-jpeg@^0.5.0".to_string(),
            hint: "re-run the release publish (cargo xtask static-registry emit)".to_string(),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("incomplete release of 0.5.1"), "{rendered}");
        assert!(rendered.contains("streamlib-plugin-sdk@^0.5.0"), "{rendered}");
        assert!(rendered.contains("vulkan-jpeg@^0.5.0"), "{rendered}");
        assert!(rendered.contains("@tatolab/mavlink"), "{rendered}");
        assert!(rendered.contains("publish-release.sh"), "{rendered}");
    }

    #[test]
    #[serial_test::serial]
    fn prerelease_pin_keys_against_dev_release_manifest() {
        // The -dev.N prerelease train interacting with the gate: a
        // prerelease floor pin (bare `0.5.1-dev.3` = caret per cargo) must
        // key against the newest same-core dev release at-or-above it, per
        // the npm prerelease policy SemVerRange carries. Partial dev release
        // ⇒ typed fast-fail; complete ⇒ clean.
        let tmp = tempfile::tempdir().unwrap();
        let partial = ReleaseManifest::new(
            "0.5.1-dev.5",
            vec![ReleaseManifestMember::new("streamlib-macros", "0.5.1-dev.5")],
        );
        publish_manifest(tmp.path(), &partial);
        let pins = vec![
            pin("streamlib-plugin-sdk", "0.5.1-dev.3"),
            pin("streamlib-macros", "0.5.1-dev.3"),
        ];
        let err = with_file_registry(tmp.path(), || {
            assert_release_complete("pkg", &pins).unwrap_err()
        });
        match err {
            BuildError::IncompleteRelease { release_version, missing, .. } => {
                assert_eq!(release_version, "0.5.1-dev.5");
                assert!(
                    missing.contains("streamlib-plugin-sdk@^0.5.1-dev.3"),
                    "missing: {missing}"
                );
            }
            other => panic!("expected IncompleteRelease, got {other:?}"),
        }

        let complete = ReleaseManifest::new(
            "0.5.1-dev.5",
            vec![
                ReleaseManifestMember::new("streamlib-macros", "0.5.1-dev.5"),
                ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1-dev.5"),
            ],
        );
        publish_manifest(tmp.path(), &complete);
        let out = with_file_registry(tmp.path(), || assert_release_complete("pkg", &pins));
        assert!(out.is_ok(), "complete dev release must pass: {out:?}");
    }

    #[test]
    #[serial_test::serial]
    fn absent_manifest_proceeds_back_compat() {
        // No release manifest anywhere ⇒ pre-atomic-release registry ⇒ warn +
        // proceed (Ok). Mentally revert the Ok(None) arm to an error and this
        // would wrongly block every pre-atomic-release registry.
        let tmp = tempfile::tempdir().unwrap();
        let pins = vec![pin("streamlib-plugin-sdk", "0.5.1")];
        let out = with_file_registry(tmp.path(), || assert_release_complete("pkg", &pins));
        assert!(out.is_ok(), "absent manifest must proceed (back-compat): {out:?}");
    }
}
