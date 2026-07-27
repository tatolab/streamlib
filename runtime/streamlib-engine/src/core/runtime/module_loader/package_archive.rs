// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use streamlib_idents::PackageRef;
use streamlib_idents::app_modules::{AppModulesStagingDir, promote_staged_package_root};
use streamlib_idents::archive::{
    extract_archive_bytes_to_dir, locate_package_root_in_extracted_dir,
};

use crate::core::streamlib_home::{installed_package_slot_dir, installed_packages_modules_dir};
use crate::core::{Error, Result};

/// Materialize a package archive into the co-located
/// `streamlib_modules/@org/name` slot derived from the package's own
/// `streamlib.yaml`, and return the slot. The container is whatever the shared
/// reader sniffs from magic bytes — `.slpkg`, `.zip`, or `.tar.gz` — and
/// contents nested under a single top-level directory are tolerated, so a
/// hand-rolled `tar czf pkg.tar.gz my-package/` loads exactly like a published
/// `.slpkg`.
///
/// Identity is read from the extracted tree, never from the archive index: an
/// archive index lookup would miss a nested `my-package/streamlib.yaml`.
/// `app_modules_root` pins the app whose `streamlib_modules/` owns the slot.
/// Always overwrites the slot on load.
#[tracing::instrument(skip(app_modules_root), fields(archive = %package_archive_path.display()))]
pub fn extract_package_archive_to_installed_slot(
    package_archive_path: &Path,
    app_modules_root: Option<&Path>,
) -> Result<PathBuf> {
    use crate::core::config::ProjectConfig;

    let archive_bytes = std::fs::read(package_archive_path).map_err(|e| {
        Error::Configuration(format!(
            "Failed to read {}: {}",
            package_archive_path.display(),
            e
        ))
    })?;

    let modules_dir = installed_packages_modules_dir(app_modules_root);
    std::fs::create_dir_all(&modules_dir).map_err(|e| {
        Error::Configuration(format!("Failed to create {}: {}", modules_dir.display(), e))
    })?;
    let staging = AppModulesStagingDir::create(&modules_dir).map_err(|e| {
        Error::Configuration(format!("Failed to stage package archive extraction: {e}"))
    })?;

    let source_label = package_archive_path.display().to_string();
    extract_archive_bytes_to_dir(&archive_bytes, staging.path(), &source_label)
        .map_err(|e| Error::Configuration(format!("Failed to extract package archive: {e}")))?;
    let staged_package_root =
        locate_package_root_in_extracted_dir(staging.path(), &source_label)
            .map_err(|e| Error::Configuration(format!("Failed to extract package archive: {e}")))?;

    let manifest_path = staged_package_root.join(ProjectConfig::FILE_NAME);
    let manifest_yaml = std::fs::read_to_string(&manifest_path).map_err(|e| {
        Error::Configuration(format!("Failed to read {}: {}", manifest_path.display(), e))
    })?;
    let config: ProjectConfig = serde_yaml::from_str(&manifest_yaml)
        .map_err(|e| Error::Configuration(format!("Failed to parse manifest: {}", e)))?;
    let package = config.package.as_ref().ok_or_else(|| {
        Error::Configuration("streamlib.yaml missing [package] section".to_string())
    })?;

    let pkg_ref = PackageRef::new(package.org.clone(), package.name.clone());
    let slot_dir = installed_package_slot_dir(app_modules_root, &pkg_ref);
    let promoted =
        promote_staged_package_root(&staged_package_root, &slot_dir, &modules_dir, false).map_err(
            |e| {
                Error::Configuration(format!(
                    "Failed to publish {} into {}: {e}",
                    package_archive_path.display(),
                    slot_dir.display()
                ))
            },
        )?;
    tracing::info!(
        replaced = promoted.replaced,
        package = %pkg_ref,
        slot = %slot_dir.display(),
        "materialized package archive into its installed slot"
    );
    Ok(slot_dir)
}
