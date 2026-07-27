// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use streamlib_idents::app_modules::{
    ActiveLinkSlotPolicy, AddPackageOptions, AddPackageSource, AppModulesError,
    LockfileRecordingPolicy,
};

use super::errors::AddModuleError;
use crate::core::streamlib_home::resolved_app_modules_dir;

/// Materialize a package archive into the co-located
/// `streamlib_modules/@org/name` slot derived from the package's own
/// `streamlib.yaml`, and return the slot. The container is whatever the shared
/// reader sniffs from magic bytes — `.slpkg`, `.zip`, or `.tar.gz` — and
/// contents nested under a single top-level directory are tolerated, so a
/// hand-rolled `tar czf pkg.tar.gz my-package/` loads exactly like a published
/// `.slpkg`.
///
/// Runs the one shared add pipeline ([`AppModulesDir::add_package`]) under the
/// runtime's two policy deviations from `streamlib add`: the app's
/// `streamlib.lock` is never rewritten by a run, and a slot holding an active
/// `streamlib link` is refused instead of unlinked. `app_modules_root` pins the
/// app whose `streamlib_modules/` owns the slot.
///
/// [`AppModulesDir::add_package`]: streamlib_idents::app_modules::AppModulesDir::add_package
#[tracing::instrument(skip(app_modules_root), fields(archive = %package_archive_path.display()))]
pub fn extract_package_archive_to_installed_slot(
    package_archive_path: &Path,
    app_modules_root: Option<&Path>,
) -> std::result::Result<PathBuf, AddModuleError> {
    let report = resolved_app_modules_dir(app_modules_root)
        .add_package(
            &AddPackageSource::Archive {
                path: package_archive_path.to_path_buf(),
            },
            &AddPackageOptions {
                lockfile_recording_policy: LockfileRecordingPolicy::SkipLockfileRecording,
                active_link_slot_policy: ActiveLinkSlotPolicy::RefuseWhenSlotIsActiveLink,
                ..Default::default()
            },
        )
        .map_err(|e| match e {
            AppModulesError::SlotOccupiedByActiveLink {
                package_dir,
                link_target,
                ..
            } => AddModuleError::InstalledSlotOccupiedByActiveLink {
                archive: package_archive_path.to_path_buf(),
                slot: package_dir,
                link_target,
            },
            other => AddModuleError::PackageArchiveExtractionFailed {
                archive: package_archive_path.to_path_buf(),
                detail: other.to_string(),
            },
        })?;

    tracing::info!(
        replaced = report.replaced_existing,
        package = %report.package,
        slot = %report.package_dir.display(),
        "materialized package archive into its installed slot"
    );
    Ok(report.package_dir)
}
