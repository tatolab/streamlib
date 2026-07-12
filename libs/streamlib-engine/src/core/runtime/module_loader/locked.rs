// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Strict-from-lockfile resolution for a locked run.
//!
//! [`LockedResolution`] turns an application lockfile
//! ([`streamlib_idents::APP_LOCKFILE_NAME`]) into the pin set the recursive
//! module walker consults in **locked mode**: every package — top-level or
//! transitive — is forced to its pinned version's installed-cache slot as a
//! [`Strategy::Path`] with [`BuildPolicy::NeverBuild`]. No registry list /
//! download, no git fetch, no `.slpkg` re-fetch, no build — the run loads
//! strictly from the pre-materialized cache and is offline by construction.
//!
//! This is the run-side half of the install/run split: `streamlib install`
//! resolves the range→concrete tree, materializes every package into the
//! cache, and writes the lockfile; a locked run consumes that lockfile here
//! and does zero live re-resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use streamlib_idents::{Lockfile, Org, Package, PackageRef, SemVer, SemVerRange};

use super::build_orchestrator::BuildPolicy;
use super::errors::AddModuleError;
use super::source::Strategy;
use crate::core::streamlib_home::get_cached_package_dir;

/// One pinned package: its concrete version and the installed-cache slot
/// `streamlib install` materialized it into.
#[derive(Debug, Clone)]
pub(crate) struct LockedPin {
    pub version: SemVer,
    pub slot_dir: PathBuf,
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
    /// key is parsed to a typed [`PackageRef`]; the cache slot is derived as
    /// `cache/packages/<name>-<version>` (where `materialize` always stages).
    pub(crate) fn from_lockfile(
        lockfile: &Lockfile,
        lockfile_path: &Path,
    ) -> Result<Self, AddModuleError> {
        let mut pins = HashMap::with_capacity(lockfile.packages.len());
        for (key, entry) in &lockfile.packages {
            let pkg_ref = parse_package_ref_key(key).map_err(|detail| {
                AddModuleError::LockfileReadFailed {
                    path: lockfile_path.to_path_buf(),
                    detail,
                }
            })?;
            let cache_key = format!("{}-{}", pkg_ref.name.as_str(), entry.version);
            let slot_dir = get_cached_package_dir(&cache_key);
            pins.insert(
                pkg_ref,
                LockedPin {
                    version: entry.version,
                    slot_dir,
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
    /// version check. `required_by` is the human-readable requirer for the
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
        if !pin.slot_dir.join(streamlib_idents::Manifest::FILE_NAME).exists() {
            return Err(AddModuleError::LockedSlotMissing {
                package: pkg_ref.clone(),
                version: pin.version,
                expected_dir: pin.slot_dir.clone(),
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

/// Parse a canonical `@org/name` lockfile key into a typed [`PackageRef`].
/// Mirrors [`PackageRef`]'s `Deserialize` shape (strip `@`, split on `/`,
/// validate each segment) without routing through serde.
fn parse_package_ref_key(key: &str) -> Result<PackageRef, String> {
    let stripped = key
        .strip_prefix('@')
        .ok_or_else(|| format!("lockfile key '{key}' must start with '@'"))?;
    let (org_str, name_str) = stripped
        .split_once('/')
        .ok_or_else(|| format!("lockfile key '{key}' must have shape '@org/name'"))?;
    let org = Org::new(org_str).map_err(|e| format!("lockfile key '{key}': {e}"))?;
    let name = Package::new(name_str).map_err(|e| format!("lockfile key '{key}': {e}"))?;
    Ok(PackageRef::new(org, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{LockfileEntry, LockfileSource};
    use std::collections::BTreeMap;

    fn lockfile_with(entries: &[(&str, SemVer)]) -> Lockfile {
        let mut packages = BTreeMap::new();
        for (key, version) in entries {
            packages.insert(
                key.to_string(),
                LockfileEntry {
                    version: *version,
                    source: LockfileSource::Registry {
                        url: "file:///x".into(),
                    },
                    content_hash: "sha256:0".into(),
                },
            );
        }
        Lockfile { version: 1, packages }
    }

    #[test]
    fn parse_key_accepts_canonical_and_rejects_malformed() {
        assert!(parse_package_ref_key("@tatolab/core").is_ok());
        assert!(parse_package_ref_key("tatolab/core").is_err());
        assert!(parse_package_ref_key("@tatolab").is_err());
        assert!(parse_package_ref_key("@Tatolab/core").is_err()); // org must be lowercase
    }

    #[test]
    fn from_lockfile_maps_slot_and_version() {
        let lf = lockfile_with(&[("@tatolab/core", SemVer::new(1, 2, 3))]);
        let locked = LockedResolution::from_lockfile(&lf, Path::new("x.lock")).unwrap();
        let pkg = PackageRef::new(Org::new("tatolab").unwrap(), Package::new("core").unwrap());
        let pin = locked.pins.get(&pkg).unwrap();
        assert_eq!(pin.version, SemVer::new(1, 2, 3));
        // Slot is derived name-version, matching where `materialize` stages.
        assert_eq!(pin.slot_dir, get_cached_package_dir("core-1.2.3"));
    }

    #[test]
    fn resolve_missing_package_is_lockfile_miss() {
        let lf = lockfile_with(&[("@tatolab/core", SemVer::new(1, 0, 0))]);
        let locked = LockedResolution::from_lockfile(&lf, Path::new("x.lock")).unwrap();
        let absent =
            PackageRef::new(Org::new("tatolab").unwrap(), Package::new("h264").unwrap());
        let err = locked.resolve(&absent, "top-level").unwrap_err();
        assert!(matches!(err, AddModuleError::LockfileMiss { .. }), "got {err:?}");
    }

    #[test]
    fn resolve_present_but_uninstalled_is_locked_slot_missing() {
        // The package IS pinned, but its cache slot doesn't exist — the
        // pinned set was never materialized. Fails loud, not a live fetch.
        let lf = lockfile_with(&[("@tatolab/never-installed-xyz", SemVer::new(9, 9, 9))]);
        let locked = LockedResolution::from_lockfile(&lf, Path::new("x.lock")).unwrap();
        let pkg = PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("never-installed-xyz").unwrap(),
        );
        let err = locked.resolve(&pkg, "top-level").unwrap_err();
        assert!(matches!(err, AddModuleError::LockedSlotMissing { .. }), "got {err:?}");
    }

    #[test]
    fn pinned_packages_is_sorted_and_complete() {
        let lf = lockfile_with(&[
            ("@tatolab/zeta", SemVer::new(1, 0, 0)),
            ("@tatolab/alpha", SemVer::new(2, 0, 0)),
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
