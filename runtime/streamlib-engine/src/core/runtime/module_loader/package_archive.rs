// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use streamlib_idents::PackageRef;
use streamlib_idents::app_modules::AppModulesStagingDir;
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
    publish_staged_package_root_into_slot(&staged_package_root, &slot_dir)?;
    tracing::info!(
        package = %pkg_ref,
        slot = %slot_dir.display(),
        "materialized package archive into its installed slot"
    );
    Ok(slot_dir)
}

/// Move `staged_package_root` onto `slot_dir`, replacing whatever occupied the
/// slot. Staging and slot share the modules dir's filesystem, so the rename is
/// atomic; a cross-device rename (a staging dir on another mount) falls back to
/// a recursive copy rather than failing the load.
fn publish_staged_package_root_into_slot(
    staged_package_root: &Path,
    slot_dir: &Path,
) -> Result<()> {
    if let Some(slot_parent) = slot_dir.parent() {
        std::fs::create_dir_all(slot_parent).map_err(|e| {
            Error::Configuration(format!("Failed to create {}: {}", slot_parent.display(), e))
        })?;
    }
    remove_slot_entry(slot_dir)?;
    match std::fs::rename(staged_package_root, slot_dir) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            copy_dir_recursive(staged_package_root, slot_dir).map_err(|copy_error| {
                Error::Configuration(format!(
                    "Failed to publish {} into {}: rename failed ({rename_error}), \
                     copy fallback failed ({copy_error})",
                    staged_package_root.display(),
                    slot_dir.display()
                ))
            })
        }
    }
}

/// Clear a slot entry: a symlink (an active `streamlib link`) is unlinked
/// without being followed into the linked checkout, a directory is removed
/// recursively, an absent path is a no-op.
fn remove_slot_entry(slot_dir: &Path) -> Result<()> {
    let removal = match std::fs::symlink_metadata(slot_dir) {
        Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(slot_dir),
        Ok(_) => std::fs::remove_file(slot_dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
    };
    removal.map_err(|e| {
        Error::Configuration(format!(
            "Failed to clear the installed slot {}: {}",
            slot_dir.display(),
            e
        ))
    })
}

/// Recursive directory copy used only as the cross-device fallback for the
/// staging-to-slot promote. Symlinks are copied by target (the extractor has
/// already refused any target that escapes the package root).
fn copy_dir_recursive(source_dir: &Path, dest_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dest_dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &dest_path)?;
        } else {
            std::fs::copy(&source_path, &dest_path)?;
        }
    }
    Ok(())
}
