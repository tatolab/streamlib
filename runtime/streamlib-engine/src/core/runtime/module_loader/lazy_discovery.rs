// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lazy plugin auto-discovery: given a referenced processor-type
//! [`SchemaIdent`] whose type is not yet registered, scan the app's
//! `streamlib_modules/` folder to find the package that declares it, so the
//! runtime can load that package on first reference without the app calling
//! `add_module`.
//!
//! Discovery matches a manifest's `package:` block org + name and its
//! declared processor short names against the referenced ident's `org` /
//! `package` / `type`. The referenced version is deliberately ignored — the
//! installed version is pinned in `streamlib.lock` at `streamlib add` time,
//! not at the `add_processor` reference site.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use streamlib_idents::app_modules::APP_MODULES_DIR_NAME;
use streamlib_idents::{ModuleIdent, PackageRef, SemVerRange};
use streamlib_processor_schema::ProjectConfigMinimal;

use crate::core::descriptors::SchemaIdent;
use crate::core::error::{Error, Result};

/// A `streamlib_modules/@org/name` package folder whose manifest declares a
/// processor matching the referenced ident.
struct ProviderMatch {
    package: PackageRef,
    package_dir: PathBuf,
}

/// Resolve the module that provides `processor_type` by scanning
/// `<app_modules_root>/streamlib_modules/`.
///
/// - `Ok(Some(module_ident))` — exactly one package declares the type; load
///   this module.
/// - `Ok(None)` — no package in `streamlib_modules/` declares the type (or
///   the folder is absent). The caller degrades to the existing
///   [`Error::UnknownProcessorType`] path.
/// - `Err(Error::AmbiguousProcessorTypeProviders)` — two or more package
///   folders declare the same type; a malformed install the caller cannot
///   resolve automatically.
#[tracing::instrument(skip(app_modules_root), fields(processor_type = %processor_type))]
pub(super) fn resolve_providing_module(
    app_modules_root: &Path,
    processor_type: &SchemaIdent,
) -> Result<Option<ModuleIdent>> {
    let modules_dir = app_modules_root.join(APP_MODULES_DIR_NAME);
    if !modules_dir.is_dir() {
        tracing::debug!(
            modules_dir = %modules_dir.display(),
            "no streamlib_modules/ folder — nothing to lazily discover"
        );
        return Ok(None);
    }

    let matches = scan_for_providers(&modules_dir, processor_type)?;

    match matches.len() {
        0 => {
            tracing::debug!(
                modules_dir = %modules_dir.display(),
                "no package in streamlib_modules/ declares the referenced processor type"
            );
            Ok(None)
        }
        1 => {
            let found = &matches[0];
            tracing::info!(
                package = %found.package,
                source = %found.package_dir.display(),
                "lazily discovered the package providing the referenced processor type"
            );
            // Load by @org/name at any version — the installed version is
            // pinned in streamlib.lock, resolved by Strategy::InstalledCache
            // against the same streamlib_modules/ folder.
            Ok(Some(ModuleIdent::new(
                found.package.org.clone(),
                found.package.name.clone(),
                SemVerRange::Any,
            )))
        }
        _ => {
            // Two or more folders declare the same type. Impossible for a
            // well-formed install (a fully-qualified type embeds its package,
            // and `streamlib add` writes one slot per @org/name), so this is a
            // duplicate/malformed folder the caller must resolve by hand.
            let mut packages: Vec<PackageRef> =
                matches.iter().map(|m| m.package.clone()).collect();
            packages.sort_by_key(|p| p.to_string());
            tracing::warn!(
                processor_type = %processor_type,
                folders = matches.len(),
                "ambiguous processor-type providers in streamlib_modules/"
            );
            Err(Error::AmbiguousProcessorTypeProviders {
                processor_type: processor_type.clone(),
                packages,
            })
        }
    }
}

/// Walk `streamlib_modules/@org/name` and collect every package folder whose
/// manifest declares a processor matching `processor_type`. A malformed or
/// unreadable manifest is skipped (warned) rather than failing the scan — a
/// single broken folder must not defeat discovery of a valid sibling.
fn scan_for_providers(
    modules_dir: &Path,
    processor_type: &SchemaIdent,
) -> Result<Vec<ProviderMatch>> {
    let mut matches = Vec::new();
    // Guard against a duplicate physical directory landing in the result twice.
    let mut seen_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    let org_entries = std::fs::read_dir(modules_dir).map_err(|e| {
        Error::Configuration(format!(
            "scanning streamlib_modules/ at {} for lazy discovery: {e}",
            modules_dir.display()
        ))
    })?;

    for org_entry in org_entries.flatten() {
        let org_dir = org_entry.path();
        if !org_dir.is_dir() || is_ignored_entry(&org_entry.file_name()) {
            continue;
        }
        let name_entries = match std::fs::read_dir(&org_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(
                    dir = %org_dir.display(),
                    error = %e,
                    "skipping unreadable streamlib_modules/ org folder during lazy discovery"
                );
                continue;
            }
        };
        for name_entry in name_entries.flatten() {
            let package_dir = name_entry.path();
            if !package_dir.is_dir() || is_ignored_entry(&name_entry.file_name()) {
                continue;
            }
            if !seen_dirs.insert(package_dir.clone()) {
                continue;
            }
            if let Some(package) = package_declares_processor(&package_dir, processor_type) {
                matches.push(ProviderMatch {
                    package,
                    package_dir,
                });
            }
        }
    }

    Ok(matches)
}

/// Whether the `streamlib.yaml` at `package_dir` declares the referenced
/// ident's owning package (matching org and name) AND a processor whose short
/// name equals the referenced type name. Returns the owning [`PackageRef`] on
/// a match; a missing / unreadable / malformed manifest is treated as "no
/// match" (this folder just isn't the provider).
fn package_declares_processor(
    package_dir: &Path,
    processor_type: &SchemaIdent,
) -> Option<PackageRef> {
    let manifest_path = package_dir.join(streamlib_idents::Manifest::FILE_NAME);
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let config: ProjectConfigMinimal = match serde_yaml::from_str(&content) {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(
                manifest = %manifest_path.display(),
                error = %e,
                "skipping unparseable streamlib.yaml during lazy discovery"
            );
            return None;
        }
    };
    let package = config.package.as_ref()?;
    if package.org.as_str() != processor_type.org.as_str()
        || package.name.as_str() != processor_type.package.as_str()
    {
        return None;
    }
    let declares_type = config
        .processors
        .iter()
        .any(|processor| processor.name == processor_type.r#type.as_str());
    if declares_type {
        Some(PackageRef::new(package.org.clone(), package.name.clone()))
    } else {
        None
    }
}

/// Directory entries readers of `streamlib_modules/` must skip: dot-prefixed
/// (in-flight `.staging-*` promotes and any hidden folder).
fn is_ignored_entry(file_name: &std::ffi::OsStr) -> bool {
    file_name
        .to_str()
        .map(|name| name.starts_with('.'))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a `streamlib_modules/@org/name/streamlib.yaml` under `root`
    /// declaring `processors` (short names) for package `@org/name`.
    fn write_package(root: &Path, org: &str, name: &str, processors: &[&str]) {
        write_package_at(root, &format!("@{org}"), name, org, name, processors);
    }

    /// Write a package at an arbitrary `<dir_org>/<dir_name>` slot whose
    /// manifest declares `manifest_org`/`manifest_name` — lets a test build a
    /// folder whose path disagrees with its declared identity (the malformed
    /// duplicate case).
    fn write_package_at(
        root: &Path,
        dir_org: &str,
        dir_name: &str,
        manifest_org: &str,
        manifest_name: &str,
        processors: &[&str],
    ) {
        let dir = root.join(APP_MODULES_DIR_NAME).join(dir_org).join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut yaml = format!(
            "package:\n  org: {manifest_org}\n  name: {manifest_name}\n  version: 1.0.0\nprocessors:\n"
        );
        for proc in processors {
            yaml.push_str(&format!(
                "  - name: {proc}\n    description: d\n    runtime: rust\n    \
                 execution: manual\n    inputs: []\n    outputs: []\n"
            ));
        }
        std::fs::write(dir.join("streamlib.yaml"), yaml).unwrap();
    }

    fn ident(org: &str, package: &str, type_name: &str) -> SchemaIdent {
        SchemaIdent::new(
            streamlib_idents::Org::new(org).unwrap(),
            streamlib_idents::Package::new(package).unwrap(),
            streamlib_idents::TypeName::new(type_name).unwrap(),
            streamlib_idents::SemVer::new(1, 0, 0),
        )
    }

    #[test]
    fn resolves_single_provider() {
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "tatolab", "camera", &["Camera", "Preview"]);
        write_package(root.path(), "tatolab", "display", &["Display"]);

        let resolved =
            resolve_providing_module(root.path(), &ident("tatolab", "camera", "Camera")).unwrap();
        let module = resolved.expect("Camera must resolve to the camera package");
        assert_eq!(module.package_ref().to_string(), "@tatolab/camera");
    }

    #[test]
    fn returns_none_when_no_package_declares_the_type() {
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "tatolab", "camera", &["Camera"]);

        // Package present, but no folder declares `Ghost`.
        let resolved =
            resolve_providing_module(root.path(), &ident("tatolab", "camera", "Ghost")).unwrap();
        assert!(resolved.is_none(), "an undeclared type must resolve to None");

        // Package absent entirely.
        let resolved =
            resolve_providing_module(root.path(), &ident("tatolab", "missing", "Thing")).unwrap();
        assert!(resolved.is_none(), "an absent package must resolve to None");
    }

    #[test]
    fn returns_none_when_modules_folder_absent() {
        let root = tempfile::tempdir().unwrap();
        // No streamlib_modules/ folder created at all.
        let resolved =
            resolve_providing_module(root.path(), &ident("tatolab", "camera", "Camera")).unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn ambiguous_when_two_folders_declare_the_same_type() {
        // Two folders whose manifests both declare @tatolab/dup/Thing — a
        // malformed install (the second folder's path disagrees with its
        // declared identity). Discovery must refuse rather than pick one.
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "tatolab", "dup", &["Thing"]);
        write_package_at(root.path(), "@tatolab", "dup-alias", "tatolab", "dup", &["Thing"]);

        let err = resolve_providing_module(root.path(), &ident("tatolab", "dup", "Thing"))
            .expect_err("two folders declaring the same type must be ambiguous");
        match err {
            Error::AmbiguousProcessorTypeProviders {
                processor_type,
                packages,
            } => {
                assert_eq!(processor_type.r#type.as_str(), "Thing");
                assert_eq!(packages.len(), 2, "both folders must be reported");
            }
            other => panic!("expected AmbiguousProcessorTypeProviders, got {other:?}"),
        }
    }

    #[test]
    fn skips_staging_and_hidden_entries() {
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "tatolab", "camera", &["Camera"]);
        // A `.staging-` org-level entry must not be scanned even if it holds a
        // manifest declaring the type.
        write_package_at(
            root.path(),
            ".staging-tatolab",
            "camera",
            "tatolab",
            "camera",
            &["Camera"],
        );

        let resolved =
            resolve_providing_module(root.path(), &ident("tatolab", "camera", "Camera")).unwrap();
        // Only the real folder matches — the staging one is skipped, so it's a
        // single (not ambiguous) match.
        assert!(resolved.is_some(), "the real package must still resolve");
    }

    #[test]
    fn version_at_reference_site_is_ignored() {
        // The manifest declares version 1.0.0; a reference at 9.9.9 (different
        // version) still resolves the package — the reference site carries no
        // binding version, streamlib.lock pins it.
        let root = tempfile::tempdir().unwrap();
        write_package(root.path(), "tatolab", "camera", &["Camera"]);
        let referenced = SchemaIdent::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("camera").unwrap(),
            streamlib_idents::TypeName::new("Camera").unwrap(),
            streamlib_idents::SemVer::new(9, 9, 9),
        );
        let resolved = resolve_providing_module(root.path(), &referenced).unwrap();
        assert!(resolved.is_some(), "version at reference site must not gate discovery");
    }
}
