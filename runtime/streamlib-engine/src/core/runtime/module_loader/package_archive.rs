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
        .map_err(|e| add_package_failure_to_add_module_error(e, package_archive_path))?;

    tracing::info!(
        replaced = report.replaced_existing,
        package = %report.package,
        slot = %report.package_dir.display(),
        "materialized package archive into its installed slot"
    );
    Ok(report.package_dir)
}

/// Classify an [`AppModulesError`] from the shared add pipeline onto the
/// loader's per-stage taxonomy, so the message names the stage that actually
/// failed (read / extract / validate / promote) instead of blaming extraction
/// for every failure. Each arm forwards the inner `detail` rather than the
/// whole `Display`, which would restate `archive` a second time.
fn add_package_failure_to_add_module_error(
    failure: AppModulesError,
    package_archive_path: &Path,
) -> AddModuleError {
    let archive = package_archive_path.to_path_buf();
    match failure {
        AppModulesError::SourceNotFound { .. } => {
            AddModuleError::PackageArchiveNotFound { archive }
        }
        // Only an I/O failure ON the archive itself is a read failure; one at
        // any other path is a `streamlib_modules/` filesystem failure, which
        // gets the stage-neutral prefix.
        AppModulesError::Io { path, detail } => {
            if path == package_archive_path {
                AddModuleError::PackageArchiveReadFailed { archive, detail }
            } else {
                AddModuleError::PackageArchiveMaterializationFailed {
                    archive,
                    detail: AppModulesError::Io { path, detail }.to_string(),
                }
            }
        }
        AppModulesError::UnsupportedArchive { detail, .. } => {
            AddModuleError::PackageArchiveContainerUnrecognized { archive, detail }
        }
        AppModulesError::ExtractFailed { detail, .. } => {
            AddModuleError::PackageArchiveExtractionFailed { archive, detail }
        }
        AppModulesError::InvalidPackage { detail, .. } => {
            AddModuleError::PackageArchiveContentsNotAValidPackage { archive, detail }
        }
        AppModulesError::MissingPackageIdentity { .. } => {
            AddModuleError::PackageArchiveMissingPackageIdentity { archive }
        }
        AppModulesError::PackageIsNotStandalone { offenders, .. }
        | AppModulesError::InstallPackageIsNotStandalone { offenders, .. } => {
            AddModuleError::PackageArchiveIsNotStandalone { archive, offenders }
        }
        AppModulesError::StagePromoteFailed {
            package_dir,
            detail,
        } => AddModuleError::PackageArchiveInstalledSlotPromoteFailed {
            archive,
            slot: package_dir,
            detail,
        },
        AppModulesError::SlotOccupiedByActiveLink {
            package_dir,
            link_target,
            ..
        } => AddModuleError::InstalledSlotOccupiedByActiveLink {
            archive,
            slot: package_dir,
            link_target,
        },
        other => AddModuleError::PackageArchiveMaterializationFailed {
            archive,
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::archive::ArchiveKind;
    use streamlib_idents::archive::package_archive_fixtures::package_archive_bytes;

    /// Every failure mode below asserts the FULL rendered message, not just the
    /// variant: the defect these guard is a message that names the wrong stage
    /// (an "extraction failed" prefix over a promote / validation / read
    /// failure), which a variant-only assertion cannot see.
    fn extract_failure(archive: &Path, app_modules_root: &Path) -> AddModuleError {
        extract_package_archive_to_installed_slot(archive, Some(app_modules_root))
            .expect_err("the fixture must not materialize a slot")
    }

    fn write_archive(dir: &Path, file_name: &str, entries: &[(String, Vec<u8>)]) -> PathBuf {
        let path = dir.join(file_name);
        std::fs::write(
            &path,
            package_archive_bytes(entries, ArchiveKind::Zip, None),
        )
        .unwrap();
        path
    }

    fn entry(path: &str, body: &str) -> (String, Vec<u8>) {
        (path.to_string(), body.as_bytes().to_vec())
    }

    fn manifest_entry(name: &str) -> (String, Vec<u8>) {
        entry(
            "streamlib.yaml",
            &format!("package:\n  org: tatolab\n  name: {name}\n  version: \"0.1.0\"\n"),
        )
    }

    #[test]
    fn absent_archive_is_reported_as_not_found() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let archive = src.path().join("absent.slpkg");

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(err, AddModuleError::PackageArchiveNotFound { .. }),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!("Package archive not found at {}", archive.display())
        );
    }

    #[test]
    fn unreadable_archive_bytes_are_reported_as_a_read_failure() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        // A directory where an archive file is expected: present on disk, so
        // not a not-found, but its bytes cannot be read.
        let archive = src.path().join("a-directory.slpkg");
        std::fs::create_dir(&archive).unwrap();

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(err, AddModuleError::PackageArchiveReadFailed { .. }),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(
            message.starts_with(&format!(
                "Failed to read package archive at {}: ",
                archive.display()
            )),
            "{message}"
        );
    }

    #[test]
    fn bytes_matching_no_container_are_not_reported_as_an_extraction_failure() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let archive = src.path().join("plain-text.slpkg");
        std::fs::write(&archive, b"this is not an archive").unwrap();

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(
                err,
                AddModuleError::PackageArchiveContainerUnrecognized { .. }
            ),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(
            message.starts_with(&format!(
                "Package archive at {} is not a recognized container: ",
                archive.display()
            )),
            "{message}"
        );
        assert!(
            message.contains("expected a zip-shaped .slpkg/.zip or a gzip-compressed .tar.gz"),
            "the unrecognized-container message must name the containers that work: {message}"
        );
        assert!(
            !message.contains("Failed to extract"),
            "nothing was extracted — the message must not blame extraction: {message}"
        );
    }

    #[test]
    fn a_recognized_container_that_fails_to_unpack_is_an_extraction_failure() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let archive = src.path().join("truncated.slpkg");
        // Zip magic (so the container sniffs as zip) over a body the zip
        // reader cannot open — the one failure "Failed to extract" fits.
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(b"truncated body, not a real central directory");
        std::fs::write(&archive, &bytes).unwrap();

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(err, AddModuleError::PackageArchiveExtractionFailed { .. }),
            "{err:?}"
        );
        assert!(
            err.to_string().starts_with(&format!(
                "Failed to extract package archive at {}: ",
                archive.display()
            )),
            "{err}"
        );
    }

    #[test]
    fn an_archive_with_no_package_root_is_not_reported_as_an_extraction_failure() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        // Unpacks cleanly; it just carries no streamlib.yaml at the package
        // root (nor under a single nested top-level directory).
        let archive = write_archive(
            src.path(),
            "no-manifest.slpkg",
            &[entry("notes.txt", "no manifest in here")],
        );

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(
                err,
                AddModuleError::PackageArchiveContentsNotAValidPackage { .. }
            ),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!(
                "Package archive at {} does not contain a valid streamlib package: \
                 no streamlib.yaml at the package root",
                archive.display()
            ),
            "the extraction succeeded — the message must name the contents, not extraction"
        );
    }

    /// A path artifact is a validation refusal, not a materialization failure.
    /// Without its own arm it falls through to the stage-neutral catch-all,
    /// which restates the archive path a second time — exactly what the arms
    /// above exist to prevent.
    #[test]
    fn an_archive_carrying_a_path_artifact_names_the_offender_once() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let archive = write_archive(
            src.path(),
            "path-artifact.slpkg",
            &[entry(
                "streamlib.yaml",
                "package:\n  org: tatolab\n  name: camera\n  version: 1.0.0\n\
                 patch:\n  '@tatolab/core':\n    path: ../core\n",
            )],
        );

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(err, AddModuleError::PackageArchiveIsNotStandalone { .. }),
            "{err:?}"
        );
        let rendered = err.to_string();
        assert!(rendered.contains("../core"), "{rendered}");
        assert_eq!(
            rendered.matches(&archive.display().to_string()).count(),
            1,
            "the archive path must appear once, not restated by a nested Display: {rendered}"
        );
    }

    #[test]
    fn an_archive_whose_manifest_declares_no_identity_names_the_missing_package_block() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let archive = write_archive(
            src.path(),
            "no-identity.slpkg",
            &[entry("streamlib.yaml", "dependencies: {}\n")],
        );

        let err = extract_failure(&archive, app_root.path());
        assert!(
            matches!(
                err,
                AddModuleError::PackageArchiveMissingPackageIdentity { .. }
            ),
            "{err:?}"
        );
        let message = err.to_string();
        assert!(
            message.starts_with(&format!("Package archive at {} has no", archive.display())),
            "{message}"
        );
        assert!(
            message.contains("`package:` block in its streamlib.yaml"),
            "{message}"
        );
        assert!(
            !message.contains("Failed to extract"),
            "the archive extracted fine — the message must not blame extraction: {message}"
        );
    }

    #[test]
    fn a_failed_slot_promote_names_the_slot_not_extraction() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let package_name = "promote-guard";
        let archive = write_archive(
            src.path(),
            "promote-guard.slpkg",
            &[manifest_entry(package_name)],
        );

        // A regular file where the slot's `@org` parent directory belongs: the
        // archive reads, extracts, and validates, then the promote into
        // `streamlib_modules/@tatolab/promote-guard` cannot create its parent.
        let modules_dir = app_root.path().join("streamlib_modules");
        std::fs::create_dir_all(&modules_dir).unwrap();
        std::fs::write(modules_dir.join("@tatolab"), b"not a directory").unwrap();

        let err = extract_failure(&archive, app_root.path());
        let AddModuleError::PackageArchiveInstalledSlotPromoteFailed { ref slot, .. } = err else {
            panic!("expected PackageArchiveInstalledSlotPromoteFailed, got {err:?}");
        };
        assert_eq!(slot, &modules_dir.join("@tatolab").join(package_name));
        let message = err.to_string();
        assert!(
            message.starts_with(&format!(
                "Failed to publish package archive at {} into its installed slot {}: ",
                archive.display(),
                slot.display()
            )),
            "{message}"
        );
        assert!(
            !message.contains("Failed to extract"),
            "the archive extracted fine — the message must not blame extraction: {message}"
        );
    }
}
