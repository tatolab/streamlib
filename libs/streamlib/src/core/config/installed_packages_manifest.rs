// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use streamlib_idents::{PackageRef, SemVer};

use crate::core::streamlib_home::get_streamlib_home;
use crate::core::{Result, StreamError};

/// Format-version of the on-disk installed-package manifest. Bumped when the
/// shape changes; mismatched files are reset on load with a warning. Pre-#717
/// installs were keyed on bare `Package::name` strings (no org segment) and
/// carried no `format_version` field — they parse as `format_version: 0`,
/// mismatch the current value, and wipe-on-load.
const CURRENT_FORMAT_VERSION: u32 = 1;

/// Manifest of installed packages at `~/.streamlib/packages.yaml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstalledPackageManifest {
    /// Format-version marker. See [`CURRENT_FORMAT_VERSION`].
    #[serde(default)]
    pub format_version: u32,

    #[serde(default)]
    pub packages: Vec<InstalledPackageEntry>,
}

/// A single installed package entry. Identity is the canonical
/// [`PackageRef`] (`@org/name`); the runtime looks entries up by typed key
/// — no string parsing at the lookup site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageEntry {
    pub name: PackageRef,
    pub version: SemVer,
    #[serde(default)]
    pub description: Option<String>,
    pub installed_from: String,
    pub installed_at: String,
    pub cache_dir: String,
}

impl InstalledPackageManifest {
    /// Load from `~/.streamlib/packages.yaml`. Returns `Default` when:
    ///
    /// - The file does not exist (fresh install).
    /// - The file's `format_version` is older than [`CURRENT_FORMAT_VERSION`].
    /// - The file fails to parse against the current shape (likely a
    ///   pre-#717 manifest with bare-name keys).
    ///
    /// In the latter two cases a warning is emitted; the on-disk slpkg
    /// caches under `~/.streamlib/cache/` are preserved, so reinstalling
    /// the relevant packages with `streamlib pkg install` repopulates the
    /// manifest cleanly.
    pub fn load() -> Result<Self> {
        let path = get_installed_packages_manifest_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path).map_err(|e| {
            StreamError::Configuration(format!("Failed to read {}: {}", path.display(), e))
        })?;

        match serde_yaml::from_str::<Self>(&content) {
            Ok(manifest) if manifest.format_version == CURRENT_FORMAT_VERSION => Ok(manifest),
            Ok(other) => {
                tracing::warn!(
                    "Installed-package manifest at {} has format_version={} but expected {}. \
                     Resetting (existing slpkg caches under ~/.streamlib/cache/ are preserved; \
                     reinstall packages with `streamlib pkg install` to repopulate).",
                    path.display(),
                    other.format_version,
                    CURRENT_FORMAT_VERSION,
                );
                Ok(Self::default())
            }
            Err(e) => {
                tracing::warn!(
                    "Installed-package manifest at {} could not be parsed against the \
                     current shape (likely a pre-#717 format with bare-name entries): {}. \
                     Resetting (existing slpkg caches under ~/.streamlib/cache/ are preserved; \
                     reinstall packages with `streamlib pkg install` to repopulate).",
                    path.display(),
                    e,
                );
                Ok(Self::default())
            }
        }
    }

    /// Save to `~/.streamlib/packages.yaml`, stamping the current
    /// format-version.
    pub fn save(&mut self) -> Result<()> {
        self.format_version = CURRENT_FORMAT_VERSION;

        let path = get_installed_packages_manifest_path();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let content = serde_yaml::to_string(self).map_err(|e| {
            StreamError::Configuration(format!("Failed to serialize packages manifest: {}", e))
        })?;

        std::fs::write(&path, content).map_err(|e| {
            StreamError::Configuration(format!("Failed to write {}: {}", path.display(), e))
        })?;

        Ok(())
    }

    /// Find a package by canonical [`PackageRef`].
    pub fn find_by_ref(&self, package_ref: &PackageRef) -> Option<&InstalledPackageEntry> {
        self.packages.iter().find(|p| &p.name == package_ref)
    }

    /// Add a package entry, replacing any existing entry with the same
    /// canonical [`PackageRef`].
    pub fn add(&mut self, entry: InstalledPackageEntry) {
        self.packages.retain(|p| p.name != entry.name);
        self.packages.push(entry);
    }

    /// Remove a package by canonical [`PackageRef`], returning the removed
    /// entry if found.
    pub fn remove_by_ref(&mut self, package_ref: &PackageRef) -> Option<InstalledPackageEntry> {
        let pos = self.packages.iter().position(|p| &p.name == package_ref)?;
        Some(self.packages.remove(pos))
    }
}

/// Get the path to the installed packages manifest file.
pub fn get_installed_packages_manifest_path() -> std::path::PathBuf {
    get_streamlib_home().join("packages.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_idents::{Org, Package};

    fn ref_for(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    fn entry(org: &str, name: &str, version: SemVer) -> InstalledPackageEntry {
        InstalledPackageEntry {
            name: ref_for(org, name),
            version,
            description: None,
            installed_from: "test".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
            cache_dir: format!("{}-{}-{}", org, name, version),
        }
    }

    #[test]
    fn add_and_find_by_canonical_ref() {
        let mut m = InstalledPackageManifest::default();
        m.add(entry("tatolab", "core", SemVer::new(1, 0, 0)));
        m.add(entry("tatolab", "h264", SemVer::new(0, 4, 0)));

        let core_ref = ref_for("tatolab", "core");
        let found = m.find_by_ref(&core_ref).expect("core present");
        assert_eq!(found.name.org.as_str(), "tatolab");
        assert_eq!(found.name.name.as_str(), "core");
        assert_eq!(found.version, SemVer::new(1, 0, 0));

        // Different org with the same short name must NOT match — the bare-
        // name collapse this migration eliminates.
        let other_ref = ref_for("acme", "core");
        assert!(m.find_by_ref(&other_ref).is_none());
    }

    #[test]
    fn add_replaces_existing_with_same_canonical_ref() {
        let mut m = InstalledPackageManifest::default();
        m.add(entry("tatolab", "core", SemVer::new(1, 0, 0)));
        m.add(entry("tatolab", "core", SemVer::new(1, 1, 0)));
        assert_eq!(m.packages.len(), 1);
        let core = m.find_by_ref(&ref_for("tatolab", "core")).unwrap();
        assert_eq!(core.version, SemVer::new(1, 1, 0));
    }

    #[test]
    fn remove_by_canonical_ref_returns_entry() {
        let mut m = InstalledPackageManifest::default();
        m.add(entry("tatolab", "core", SemVer::new(1, 0, 0)));
        m.add(entry("tatolab", "h264", SemVer::new(0, 4, 0)));
        let removed = m.remove_by_ref(&ref_for("tatolab", "core")).unwrap();
        assert_eq!(removed.name.name.as_str(), "core");
        assert_eq!(m.packages.len(), 1);
        assert!(m.find_by_ref(&ref_for("tatolab", "core")).is_none());
    }

    #[test]
    fn yaml_roundtrip_canonical_ref() {
        // Canonical-key roundtrip: serialize, deserialize, lookup must all
        // agree on `@org/name`. Mentally reverting the `name: PackageRef`
        // migration breaks this because the bare-name shape no longer
        // round-trips through `PackageRef`'s Deserialize.
        let mut m = InstalledPackageManifest {
            format_version: CURRENT_FORMAT_VERSION,
            packages: Vec::new(),
        };
        m.add(entry("tatolab", "core", SemVer::new(1, 0, 0)));
        let yaml = serde_yaml::to_string(&m).unwrap();
        // Wire form must contain the canonical joined string, not bare name.
        assert!(yaml.contains("@tatolab/core"), "yaml = {}", yaml);
        assert!(!yaml.contains("name: core\n"), "bare-name leak: {}", yaml);

        let back: InstalledPackageManifest = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(back.packages.len(), 1);
        assert_eq!(back.packages[0].name, ref_for("tatolab", "core"));
    }

    #[test]
    fn legacy_bare_name_format_fails_to_parse() {
        // The pre-#717 shape had `name: core` (bare). Parsing must fail
        // against the current shape so `load()` triggers wipe-on-mismatch.
        let legacy_yaml = r#"
packages:
  - name: core
    version: "1.0.0"
    installed_from: test
    installed_at: "1970-01-01T00:00:00Z"
    cache_dir: core-1.0.0
"#;
        let res: std::result::Result<InstalledPackageManifest, _> =
            serde_yaml::from_str(legacy_yaml);
        assert!(
            res.is_err(),
            "pre-#717 bare-name shape MUST NOT parse — wipe-on-mismatch requires it",
        );
    }

    #[test]
    fn future_format_version_is_treated_as_mismatch() {
        // A manifest from a future streamlib that bumps the format_version
        // must not silently round-trip into the current shape — `load()`
        // would wipe-on-mismatch with a warning, and this serialization-
        // level check confirms the field round-trips honestly.
        let future_yaml = format!(
            "format_version: {}\npackages: []\n",
            CURRENT_FORMAT_VERSION + 1
        );
        let parsed: InstalledPackageManifest = serde_yaml::from_str(&future_yaml).unwrap();
        assert_eq!(parsed.format_version, CURRENT_FORMAT_VERSION + 1);
        assert_ne!(parsed.format_version, CURRENT_FORMAT_VERSION);
    }
}
