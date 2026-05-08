// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Project and runtime configuration.

mod installed_packages_manifest;
mod project_config;

pub use installed_packages_manifest::{InstalledPackageEntry, InstalledPackageManifest};
pub use project_config::ProjectConfig;
