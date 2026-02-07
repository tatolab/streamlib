// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::core::streamlib_home::get_streamlib_home;
use crate::core::{Result, StreamError};

/// Manifest of installed packages at `~/.streamlib/packages.yaml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstalledPackageManifest {
    #[serde(default)]
    pub packages: Vec<InstalledPackageEntry>,
}

/// A single installed package entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageEntry {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub installed_from: String,
    pub installed_at: String,
    pub cache_dir: String,
}

impl InstalledPackageManifest {
    /// Load from `~/.streamlib/packages.yaml`, returning `Default` if missing.
    pub fn load() -> Result<Self> {
        let path = get_installed_packages_manifest_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path).map_err(|e| {
            StreamError::Configuration(format!("Failed to read {}: {}", path.display(), e))
        })?;

        let manifest: Self = serde_yaml::from_str(&content).map_err(|e| {
            StreamError::Configuration(format!("Failed to parse {}: {}", path.display(), e))
        })?;

        Ok(manifest)
    }

    /// Save to `~/.streamlib/packages.yaml`.
    pub fn save(&self) -> Result<()> {
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

    /// Find a package by name.
    pub fn find_by_name(&self, name: &str) -> Option<&InstalledPackageEntry> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Add a package entry, replacing any existing entry with the same name.
    pub fn add(&mut self, entry: InstalledPackageEntry) {
        self.packages.retain(|p| p.name != entry.name);
        self.packages.push(entry);
    }

    /// Remove a package by name, returning the removed entry if found.
    pub fn remove_by_name(&mut self, name: &str) -> Option<InstalledPackageEntry> {
        let pos = self.packages.iter().position(|p| p.name == name)?;
        Some(self.packages.remove(pos))
    }
}

/// Get the path to the installed packages manifest file.
pub fn get_installed_packages_manifest_path() -> std::path::PathBuf {
    get_streamlib_home().join("packages.yaml")
}
