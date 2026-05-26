// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::errors::AddModuleError;
use super::processor_registration::host_target_triple;
use super::slpkg::extract_slpkg_to_cache;
use super::workspace::resolve_workspace_root;
use crate::core::{Error, Result};

/// How [`Runner::add_module_with`] should source the manifest for a
/// module. Each variant maps to one of the historical loader methods
/// (`load_project`, `load_package`, `load_workspace_packages`) plus the
/// imperative default chain that bare [`Runner::add_module`] uses.
///
/// The strategy is the in-code equivalent of the manifest's `patch:`
/// table — callers can pin a dep to a workspace stage dir, a specific
/// resolver tier, a `.slpkg` archive, or an arbitrary directory
/// without editing yaml.
///
/// [`Runner::add_module`]: super::super::Runner::add_module
/// [`Runner::add_module_with`]: super::super::Runner::add_module_with
#[derive(Debug, Clone)]
pub enum ModuleResolverStrategy {
    /// Default ident-keyed resolution chain: workspace stage dir
    /// (`<workspace>/target/streamlib-plugins/<org>__<name>/`) first,
    /// falling back to the installed-package cache. Behavior used by
    /// bare [`Runner::add_module`] and by transitive `dependencies:`
    /// entries declared as `"@org/name": "^version"`.
    ///
    /// [`Runner::add_module`]: super::super::Runner::add_module
    DefaultChain,

    /// Look up the workspace stage dir only; do not fall back to the
    /// installed-package cache. Counterpart of the legacy
    /// [`Runner::load_workspace_packages`] per-id lookup.
    ///
    /// [`Runner::load_workspace_packages`]: super::super::Runner::load_workspace_packages
    WorkspaceStaged,

    /// Look up the installed-package cache only; skip workspace.
    /// Useful for runtimes that explicitly want the installed-from-
    /// slpkg copy even when a workspace stage dir exists.
    InstalledCache,

    /// Load the manifest at this directory directly. Counterpart of
    /// the legacy [`Runner::load_project`].
    ///
    /// [`Runner::load_project`]: super::super::Runner::load_project
    ManifestDirectory { path: std::path::PathBuf },

    /// Extract this `.slpkg` archive into the package cache, then
    /// load the extracted manifest. Counterpart of the legacy
    /// [`Runner::load_package`].
    ///
    /// [`Runner::load_package`]: super::super::Runner::load_package
    SlpkgArchive { path: std::path::PathBuf },
}

/// Resolve a [`ModuleResolverStrategy`] to `(manifest_dir, on_disk_version)`.
///
/// `package_ref` supplies the canonical `@org/name` for the
/// ident-keyed strategies ([`ModuleResolverStrategy::DefaultChain`],
/// [`ModuleResolverStrategy::WorkspaceStaged`],
/// [`ModuleResolverStrategy::InstalledCache`]); it is ignored by the
/// path-keyed ones.
///
/// The returned version is sourced from the resolver tier that hit:
/// workspace stage dirs re-parse the staged manifest, the installed
/// cache reads `InstalledPackageEntry::version`, the path-keyed
/// variants read the manifest at the directory. The caller is
/// responsible for validating it against the requested
/// `SemVerRange`.
pub(super) fn resolve_strategy_to_manifest_dir(
    strategy: &ModuleResolverStrategy,
    package_ref: Option<&streamlib_idents::PackageRef>,
) -> std::result::Result<(std::path::PathBuf, streamlib_idents::SemVer), AddModuleError> {
    match strategy {
        ModuleResolverStrategy::DefaultChain => {
            let pkg_ref = package_ref.ok_or_else(|| AddModuleError::StrategyNeedsPackageRef {
                strategy: "DefaultChain".into(),
            })?;
            // Try workspace stage dir first, then installed cache.
            if let Some(workspace_root) = locate_workspace_root_for_strategy()? {
                let staged_dir = workspace_stage_dir(&workspace_root, pkg_ref);
                if staged_dir.join("streamlib.yaml").exists() {
                    return Ok((staged_dir.clone(), read_version_from_manifest_dir(&staged_dir)?));
                }
            }
            lookup_installed_cache(pkg_ref)?
                .ok_or_else(|| AddModuleError::ModuleNotFound { package: pkg_ref.clone() })
        }
        ModuleResolverStrategy::WorkspaceStaged => {
            let pkg_ref = package_ref.ok_or_else(|| AddModuleError::StrategyNeedsPackageRef {
                strategy: "WorkspaceStaged".into(),
            })?;
            let workspace_root = locate_workspace_root_for_strategy()?
                .ok_or(AddModuleError::WorkspaceRootNotFound)?;
            let staged_dir = workspace_stage_dir(&workspace_root, pkg_ref);
            if !staged_dir.join("streamlib.yaml").exists() {
                return Err(AddModuleError::WorkspaceStageMiss {
                    package: pkg_ref.clone(),
                    expected_path: staged_dir,
                });
            }
            // For Rust-impl packages, the cdylib must exist at
            // `lib/<host_triple>/`. Surface the missing-cdylib case
            // explicitly so callers see the actionable diagnostic
            // (rather than letting `register_manifest_processors` fail
            // with a generic "missing dylib for this triple" message).
            check_cdylib_present_when_rust_impl(&staged_dir, pkg_ref)?;
            let version = read_version_from_manifest_dir(&staged_dir)?;
            Ok((staged_dir, version))
        }
        ModuleResolverStrategy::InstalledCache => {
            let pkg_ref = package_ref.ok_or_else(|| AddModuleError::StrategyNeedsPackageRef {
                strategy: "InstalledCache".into(),
            })?;
            lookup_installed_cache(pkg_ref)?
                .ok_or_else(|| AddModuleError::ModuleNotFound { package: pkg_ref.clone() })
        }
        ModuleResolverStrategy::ManifestDirectory { path } => {
            if !path.join("streamlib.yaml").exists() {
                return Err(AddModuleError::ManifestDirectoryMissing { path: path.clone() });
            }
            let version = read_version_from_manifest_dir(path)?;
            Ok((path.clone(), version))
        }
        ModuleResolverStrategy::SlpkgArchive { path } => {
            let extracted =
                extract_slpkg_to_cache(path).map_err(|e| AddModuleError::SlpkgExtractionFailed {
                    archive: path.clone(),
                    detail: e.to_string(),
                })?;
            let version = read_version_from_manifest_dir(&extracted)?;
            Ok((extracted, version))
        }
    }
}

/// Look the canonical [`PackageRef`] up in the installed-package
/// cache (`InstalledPackageManifest`). Returns `(cache_dir, version)`
/// when present.
///
/// [`PackageRef`]: streamlib_idents::PackageRef
fn lookup_installed_cache(
    pkg_ref: &streamlib_idents::PackageRef,
) -> std::result::Result<Option<(std::path::PathBuf, streamlib_idents::SemVer)>, AddModuleError> {
    use crate::core::config::InstalledPackageManifest;
    use crate::core::streamlib_home::get_cached_package_dir;

    let manifest =
        InstalledPackageManifest::load().map_err(|e| AddModuleError::InstalledCacheLoadFailed {
            detail: e.to_string(),
        })?;
    Ok(manifest
        .find_by_ref(pkg_ref)
        .map(|entry| (get_cached_package_dir(&entry.cache_dir), entry.version)))
}

/// Best-effort workspace-root resolution for strategy lookups.
/// Honors `STREAMLIB_WORKSPACE_ROOT` strictly (typo'd env var ⇒
/// `WorkspaceRootInvalid`); otherwise falls back to
/// `cargo locate-project --workspace`, returning `Ok(None)` when no
/// workspace is reachable (so [`ModuleResolverStrategy::DefaultChain`]
/// can silently fall through to the installed cache).
fn locate_workspace_root_for_strategy(
) -> std::result::Result<Option<std::path::PathBuf>, AddModuleError> {
    if let Ok(env_root) = std::env::var("STREAMLIB_WORKSPACE_ROOT") {
        let path = std::path::PathBuf::from(&env_root);
        if !path.is_dir() {
            return Err(AddModuleError::WorkspaceRootInvalid { env_value: env_root });
        }
        return Ok(Some(path));
    }
    Ok(resolve_workspace_root().ok())
}

fn workspace_stage_dir(
    workspace_root: &std::path::Path,
    pkg_ref: &streamlib_idents::PackageRef,
) -> std::path::PathBuf {
    workspace_root
        .join("target")
        .join("streamlib-plugins")
        .join(format!(
            "{}__{}",
            pkg_ref.org.as_str(),
            pkg_ref.name.as_str()
        ))
}

/// Read the `[package].version` field from the streamlib.yaml at
/// `dir`. Surfaces [`AddModuleError::ManifestDirectoryMissing`] when
/// the file is absent and [`AddModuleError::StrategyManifestLoadFailed`]
/// when the parse or package-block lookup fails.
fn read_version_from_manifest_dir(
    dir: &std::path::Path,
) -> std::result::Result<streamlib_idents::SemVer, AddModuleError> {
    use streamlib_idents::Manifest;
    let manifest_path = dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Err(AddModuleError::ManifestDirectoryMissing { path: dir.to_path_buf() });
    }
    let manifest = Manifest::load(dir).map_err(|e| AddModuleError::StrategyManifestLoadFailed {
        source_path: dir.to_path_buf(),
        detail: e.to_string(),
    })?;
    manifest
        .package
        .as_ref()
        .map(|p| p.version)
        .ok_or_else(|| AddModuleError::StrategyManifestLoadFailed {
            source_path: dir.to_path_buf(),
            detail: "manifest has no `package:` block".into(),
        })
}

/// Read just the `(org, name, version)` triple from a manifest
/// directory's streamlib.yaml and build an `any`-version [`ModuleIdent`]
/// from it. Used by the path-keyed wrappers
/// (`Runner::load_project`, `Runner::load_package`) to build the
/// strict ident the unified flow validates against.
///
/// [`ModuleIdent`]: streamlib_idents::ModuleIdent
pub(super) fn read_module_ident_from_manifest_dir(
    dir: &std::path::Path,
) -> Result<streamlib_idents::ModuleIdent> {
    use crate::core::config::ProjectConfig;
    let config = ProjectConfig::load(dir)?;
    let pkg = config.package.as_ref().ok_or_else(|| {
        Error::Configuration(format!(
            "{} at {} has no `[package]` block — required to build a ModuleIdent.",
            ProjectConfig::FILE_NAME,
            dir.display()
        ))
    })?;
    Ok(streamlib_idents::ModuleIdent::any(pkg.org.clone(), pkg.name.clone()))
}

/// Peek the `[package]` block out of a `.slpkg` archive's embedded
/// manifest WITHOUT fully extracting the archive. Used by
/// `Runner::load_package` to build the ident the unified flow
/// validates against — the strategy's extraction step then writes
/// the package to the cache.
pub(super) fn read_module_ident_from_slpkg(
    slpkg_path: &std::path::Path,
) -> Result<streamlib_idents::ModuleIdent> {
    use crate::core::config::ProjectConfig;
    let bytes = std::fs::read(slpkg_path)
        .map_err(|e| Error::Configuration(format!("Failed to read {}: {}", slpkg_path.display(), e)))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .map_err(|e| Error::Configuration(format!("Failed to open .slpkg archive: {}", e)))?;
    let mut manifest_file = archive
        .by_name(ProjectConfig::FILE_NAME)
        .map_err(|e| Error::Configuration(format!(".slpkg archive missing {}: {}", ProjectConfig::FILE_NAME, e)))?;
    let mut yaml_body = String::new();
    std::io::Read::read_to_string(&mut manifest_file, &mut yaml_body)
        .map_err(|e| Error::Configuration(format!("Failed to read manifest from .slpkg: {}", e)))?;
    let config: ProjectConfig = serde_yaml::from_str(&yaml_body)
        .map_err(|e| Error::Configuration(format!("Failed to parse manifest from .slpkg: {}", e)))?;
    let pkg = config.package.as_ref().ok_or_else(|| {
        Error::Configuration(format!(
            ".slpkg at {} has no `[package]` block — required to build a ModuleIdent.",
            slpkg_path.display()
        ))
    })?;
    Ok(streamlib_idents::ModuleIdent::any(pkg.org.clone(), pkg.name.clone()))
}

/// Check that a Rust-impl staged package has its cdylib present at
/// `lib/<host_triple>/`. No-op for schemas-only or non-Rust packages.
/// Used by [`ModuleResolverStrategy::WorkspaceStaged`] to surface the
/// missing-cdylib case as [`AddModuleError::CdylibMissingForRustImpl`]
/// rather than letting `register_manifest_processors` fail later with
/// a less actionable message.
fn check_cdylib_present_when_rust_impl(
    staged_dir: &std::path::Path,
    pkg_ref: &streamlib_idents::PackageRef,
) -> std::result::Result<(), AddModuleError> {
    let body = match std::fs::read_to_string(staged_dir.join("streamlib.yaml")) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    let manifest: streamlib_processor_schema::ProjectConfigMinimal =
        match serde_yaml::from_str(&body) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
    let has_rust = manifest.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Rust
        )
    });
    if !has_rust {
        return Ok(());
    }
    let triple_dir = staged_dir.join("lib").join(host_target_triple());
    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let any_dylib_present = std::fs::read_dir(&triple_dir)
        .map(|iter| {
            iter.flatten()
                .any(|e| e.path().extension().is_some_and(|ext| ext == dylib_ext))
        })
        .unwrap_or(false);
    if !any_dylib_present {
        return Err(AddModuleError::CdylibMissingForRustImpl {
            package: pkg_ref.clone(),
            expected_path: triple_dir,
        });
    }
    Ok(())
}

/// Bridge helper used by `Runner::load_workspace_packages`'s
/// back-compat error translation: split an `"@org/name"` actual-id
/// string back into `(org, name)` halves so the legacy
/// `PackageIdentityMismatch` variant can carry the structured fields
/// the historical tests assert on. Falls back to empty strings when
/// the input shape is unexpected — the wrapper only uses this on
/// inputs that the unified flow itself produced.
pub(super) fn split_canonical_for_legacy_error(actual: &str) -> (String, String) {
    actual
        .strip_prefix('@')
        .and_then(|s| s.split_once('/'))
        .map(|(o, n)| (o.to_string(), n.to_string()))
        .unwrap_or_default()
}
