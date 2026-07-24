// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Strict-from-lockfile resolution for a locked run.
//!
//! [`LockedResolution`] turns an application lockfile
//! ([`streamlib_idents::APP_LOCKFILE_NAME`]) into the pin set the recursive
//! module walker consults in **locked mode**: every package — top-level or
//! transitive — is forced to its pinned version's installed-cache slot as a
//! [`Strategy::Path`] with [`BuildPolicy::NeverBuild`]. No package source list /
//! download, no git fetch, no `.slpkg` re-fetch, no build — the run loads
//! strictly from the pre-materialized cache and is offline by construction.
//!
//! Each pin carries the lockfile's `content_hash`; resolution re-hashes the
//! slot's manifest + schema set through the resolver's own hashing routine
//! ([`streamlib_idents::content_hash_for_package_dir`]) and refuses a slot
//! whose content drifted from the pin — a tampered or republished-in-place
//! slot fails typed instead of silently loading different content.
//!
//! This is the run-side half of the install/run split: `streamlib install`
//! resolves the range→concrete tree, materializes every package into the
//! cache, and writes the lockfile; a locked run consumes that lockfile here
//! and does zero live re-resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use streamlib_idents::{Lockfile, PackageRef, SemVer, SemVerRange};

use super::build_orchestrator::BuildPolicy;
use super::errors::AddModuleError;
use super::source::Strategy;
use crate::core::streamlib_home::installed_package_slot_dir;

/// One pinned package: its concrete version, the installed-cache slot
/// `streamlib install` materialized it into, and the content hash the
/// lockfile pinned for that slot.
#[derive(Debug, Clone)]
pub(crate) struct LockedPin {
    pub version: SemVer,
    pub slot_dir: PathBuf,
    pub expected_content_hash: String,
}

/// The immutable pin set a locked run resolves against, keyed by canonical
/// `@org/name`. Built once from an application [`Lockfile`] and shared
/// across the whole load (top-level + every transitive edge) so a single
/// version-per-package pin is enforced by construction.
#[derive(Debug)]
pub(crate) struct LockedResolution {
    pins: HashMap<PackageRef, LockedPin>,
}

impl LockedResolution {
    /// Build the pin set from a parsed [`Lockfile`]. Each entry's `@org/name`
    /// key is parsed to a typed [`PackageRef`]; the slot is the co-located
    /// `<app-root>/streamlib_modules/@org/name` dir the seam derives from the
    /// lockfile's parent (where `materialize` stages).
    pub(crate) fn from_lockfile(
        lockfile: &Lockfile,
        lockfile_path: &Path,
    ) -> Result<Self, AddModuleError> {
        // The lockfile sits at `<app-root>/streamlib.lock`, so its parent IS
        // the app root the slot deriver is threaded with.
        let app_root = lockfile_path.parent();
        let mut pins = HashMap::with_capacity(lockfile.packages.len());
        for (key, entry) in &lockfile.packages {
            let pkg_ref = parse_lockfile_package_ref_key(key).map_err(|detail| {
                AddModuleError::LockfileReadFailed {
                    path: lockfile_path.to_path_buf(),
                    detail,
                }
            })?;
            let slot_dir = installed_package_slot_dir(app_root, &pkg_ref);
            pins.insert(
                pkg_ref,
                LockedPin {
                    version: entry.version,
                    slot_dir,
                    expected_content_hash: entry.content_hash.clone(),
                },
            );
        }
        Ok(Self { pins })
    }

    /// Read + parse an application lockfile from disk and build the pin set.
    pub(crate) fn from_lockfile_path(path: &Path) -> Result<Self, AddModuleError> {
        let lockfile = streamlib_idents::read_lockfile(path).map_err(|e| {
            AddModuleError::LockfileReadFailed {
                path: path.to_path_buf(),
                detail: e.to_string(),
            }
        })?;
        Self::from_lockfile(&lockfile, path)
    }

    /// Every pinned package, sorted by canonical name for deterministic
    /// top-level load ordering. The full flat closure — a locked run adds
    /// each as a top-level module; the single-version gate dedups the
    /// transitive re-encounters.
    pub(crate) fn pinned_packages(&self) -> Vec<(PackageRef, SemVer)> {
        let mut out: Vec<(PackageRef, SemVer)> = self
            .pins
            .iter()
            .map(|(pkg_ref, pin)| (pkg_ref.clone(), pin.version))
            .collect();
        out.sort_by(|a, b| a.0.to_string().cmp(&b.0.to_string()));
        out
    }

    /// Resolve `pkg_ref` to the `(ModuleIdent, Strategy)` a locked walk uses.
    /// The strategy is the pinned cache slot loaded as-is ([`BuildPolicy::NeverBuild`]);
    /// the ident carries an [`SemVerRange::Exact`] pin so a slot whose
    /// on-disk version drifted from the lock fails loud at the walker's
    /// version check. The slot's manifest + schema content is re-hashed and
    /// compared to the pinned `content_hash` — the run-time integrity gate
    /// that closes the tampered / republished-in-place slot hole.
    /// `required_by` is the human-readable requirer for the
    /// [`AddModuleError::LockfileMiss`] message (the parent package, or
    /// `"top-level"` for a root add).
    pub(crate) fn resolve(
        &self,
        pkg_ref: &PackageRef,
        required_by: &str,
    ) -> Result<(streamlib_idents::ModuleIdent, Strategy), AddModuleError> {
        let pin = self
            .pins
            .get(pkg_ref)
            .ok_or_else(|| AddModuleError::LockfileMiss {
                package: pkg_ref.clone(),
                required_by: required_by.to_string(),
            })?;

        // The lockfile pins a version; the run must load exactly that
        // version's pre-materialized slot. A missing slot means the pinned
        // set was never installed (or the cache was cleared) — fail loud
        // naming `streamlib install`, never fall through to a live fetch.
        if !pin
            .slot_dir
            .join(streamlib_idents::Manifest::FILE_NAME)
            .exists()
        {
            return Err(AddModuleError::LockedSlotMissing {
                package: pkg_ref.clone(),
                version: pin.version,
                expected_dir: pin.slot_dir.clone(),
            });
        }

        // Content-hash integrity gate: re-hash the slot's manifest + schema
        // set with the resolver's own hashing routine and compare to the
        // pin. Cheap — a handful of small YAML files per package, once per
        // package per load, not on any per-frame path. Catches a slot whose
        // content was tampered with or republished in place after install.
        let actual =
            streamlib_idents::content_hash_for_package_dir(&pin.slot_dir).map_err(|e| {
                AddModuleError::LockedSlotContentMismatch {
                    package: pkg_ref.clone(),
                    expected: pin.expected_content_hash.clone(),
                    actual: format!("<unhashable: {e}>"),
                }
            })?;
        if actual != pin.expected_content_hash {
            return Err(AddModuleError::LockedSlotContentMismatch {
                package: pkg_ref.clone(),
                expected: pin.expected_content_hash.clone(),
                actual,
            });
        }

        let ident = streamlib_idents::ModuleIdent::new(
            pkg_ref.org.clone(),
            pkg_ref.name.clone(),
            SemVerRange::Exact(pin.version),
        );
        let strategy = Strategy::Path {
            path: pin.slot_dir.clone(),
            build: BuildPolicy::NeverBuild,
        };
        Ok((ident, strategy))
    }
}

/// Parse a canonical `@org/name` lockfile key into a typed [`PackageRef`]
/// via the official `Deserialize` path — the single parser for the
/// canonical form (`streamlib-idents` deliberately exposes no `parse` API).
fn parse_lockfile_package_ref_key(key: &str) -> Result<PackageRef, String> {
    serde_yaml::from_value::<PackageRef>(serde_yaml::Value::String(key.to_string()))
        .map_err(|e| format!("lockfile key '{key}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use streamlib_idents::{LockfileEntry, LockfileSource, Org, Package};

    fn lockfile_with(entries: &[(&str, SemVer, &str)]) -> Lockfile {
        let mut packages = BTreeMap::new();
        for (key, version, hash) in entries {
            packages.insert(
                key.to_string(),
                LockfileEntry {
                    version: *version,
                    source: LockfileSource::ByVersion {
                        url: "file:///x".into(),
                    },
                    content_hash: hash.to_string(),
                },
            );
        }
        Lockfile {
            version: 1,
            packages,
        }
    }

    #[test]
    fn parse_key_accepts_canonical_and_rejects_malformed() {
        assert!(parse_lockfile_package_ref_key("@tatolab/core").is_ok());
        assert!(parse_lockfile_package_ref_key("tatolab/core").is_err());
        assert!(parse_lockfile_package_ref_key("@tatolab").is_err());
        assert!(parse_lockfile_package_ref_key("@Tatolab/core").is_err()); // org must be lowercase
    }

    #[test]
    fn from_lockfile_maps_slot_version_and_hash() {
        let lockfile_path = Path::new("/app/streamlib.lock");
        let lf = lockfile_with(&[("@tatolab/core", SemVer::new(1, 2, 3), "sha256:aa")]);
        let locked = LockedResolution::from_lockfile(&lf, lockfile_path).unwrap();
        let pkg = PackageRef::new(Org::new("tatolab").unwrap(), Package::new("core").unwrap());
        let pin = locked.pins.get(&pkg).unwrap();
        assert_eq!(pin.version, SemVer::new(1, 2, 3));
        assert_eq!(pin.expected_content_hash, "sha256:aa");
        // The slot is the co-located `<app-root>/streamlib_modules/@org/name`
        // dir the seam derives from the lockfile's parent — the app root the
        // install write and this locked read share (write==read).
        assert_eq!(
            pin.slot_dir,
            installed_package_slot_dir(lockfile_path.parent(), &pkg)
        );
    }

    #[test]
    fn resolve_missing_package_is_lockfile_miss() {
        let lf = lockfile_with(&[("@tatolab/core", SemVer::new(1, 0, 0), "sha256:aa")]);
        let locked = LockedResolution::from_lockfile(&lf, Path::new("x.lock")).unwrap();
        let absent = PackageRef::new(Org::new("tatolab").unwrap(), Package::new("h264").unwrap());
        let err = locked.resolve(&absent, "top-level").unwrap_err();
        assert!(
            matches!(err, AddModuleError::LockfileMiss { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_present_but_uninstalled_is_locked_slot_missing() {
        // The package IS pinned, but its cache slot doesn't exist — the
        // pinned set was never materialized. Fails loud, not a live fetch.
        let lf = lockfile_with(&[(
            "@tatolab/never-installed-xyz",
            SemVer::new(9, 9, 9),
            "sha256:aa",
        )]);
        let locked = LockedResolution::from_lockfile(&lf, Path::new("x.lock")).unwrap();
        let pkg = PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("never-installed-xyz").unwrap(),
        );
        let err = locked.resolve(&pkg, "top-level").unwrap_err();
        assert!(
            matches!(err, AddModuleError::LockedSlotMissing { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn pinned_packages_is_sorted_and_complete() {
        let lf = lockfile_with(&[
            ("@tatolab/zeta", SemVer::new(1, 0, 0), "sha256:aa"),
            ("@tatolab/alpha", SemVer::new(2, 0, 0), "sha256:bb"),
        ]);
        let locked = LockedResolution::from_lockfile(&lf, Path::new("x.lock")).unwrap();
        let names: Vec<String> = locked
            .pinned_packages()
            .into_iter()
            .map(|(p, _)| p.to_string())
            .collect();
        assert_eq!(names, vec!["@tatolab/alpha", "@tatolab/zeta"]);
    }
}
