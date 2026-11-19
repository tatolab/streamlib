// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The extension SDK kit a project's views and controls are checked against: the first found of
//! the runtime unit's, the one its starter names, and the project's own installed SDK.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use streamlib_consumer_rhi::runtime_unit_layout::extension_sdk_kit_directory_beside_the_lend;

/// The variable a runtime's starter names a kit in, read once as the runtime starts. A packaged
/// Tato Studio names none: the unit it carries holds the kit, which is looked in first.
pub const EXTENSION_SDK_KIT_ENVIRONMENT_VARIABLE: &str = "TATOLAB_EXTENSION_SDK_KIT";

/// The package name a kit's `package.json` carries.
const EXTENSION_SDK_PACKAGE_NAME: &str = "@tatolab/tato-app-view-sdk";

/// Where a project's installed SDK sits, relative to the project.
const PROJECT_INSTALLED_EXTENSION_SDK: &str = "node_modules/@tatolab/tato-app-view-sdk";

/// The kit's folder holding the files a check folder starts from.
pub(super) const KIT_CHECK_FOLDER: &str = "check_folder";

/// The kit's tie lines, which tie each control's default export to its manifest entry.
pub(super) const KIT_TIE_LINES_FILE: &str = "tie_lines.json";

/// The kit's strict lint rules.
pub(super) const KIT_LINT_RULES_FILE: &str = "oxlint.json";

/// A kit found, where, and the SDK version it was written at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExtensionSdkKitFound {
    pub(super) directory: PathBuf,
    /// Which of the three places it is, in words: `the runtime unit's`.
    pub(super) found_as: &'static str,
    pub(super) sdk_version: String,
}

impl ExtensionSdkKitFound {
    /// The kit as a check's `checked_with` names it.
    pub(super) fn as_checked_with(&self) -> String {
        format!(
            "{EXTENSION_SDK_PACKAGE_NAME} {} ({} kit, {})",
            self.sdk_version,
            self.found_as,
            self.directory.display()
        )
    }
}

#[derive(Deserialize)]
struct KitPackageManifest {
    name: String,
    version: String,
}

/// The kit at `directory`, or why it holds none.
fn the_kit_at(directory: &Path, found_as: &'static str) -> Result<ExtensionSdkKitFound, String> {
    let manifest_file = directory.join("package.json");
    let manifest_bytes = std::fs::read(&manifest_file).map_err(|unreadable| {
        format!("{} cannot be read: {unreadable}", manifest_file.display())
    })?;
    let manifest: KitPackageManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|not_one| {
            format!(
                "{} is not a package manifest naming its version: {not_one}",
                manifest_file.display()
            )
        })?;
    if manifest.name != EXTENSION_SDK_PACKAGE_NAME {
        return Err(format!(
            "{} names the package {}, not {EXTENSION_SDK_PACKAGE_NAME}",
            manifest_file.display(),
            manifest.name
        ));
    }
    let missing: Vec<&str> = [KIT_CHECK_FOLDER, KIT_TIE_LINES_FILE, KIT_LINT_RULES_FILE]
        .into_iter()
        .filter(|kit_part| !directory.join(kit_part).exists())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} holds the SDK but no kit: it has no {}",
            directory.display(),
            missing.join(", ")
        ));
    }
    Ok(ExtensionSdkKitFound {
        directory: directory.to_path_buf(),
        found_as,
        sdk_version: manifest.version,
    })
}

/// The first kit found of: the one beside the runtime unit's lend at `lend_directory`, the one
/// its starter names as `kit_its_starter_names`, and the project at `project_directory`'s
/// installed SDK. Or why none is, naming each place looked in.
pub(super) fn the_extension_sdk_kit_found(
    lend_directory: Option<&Path>,
    kit_its_starter_names: Option<&Path>,
    project_directory: &Path,
) -> Result<ExtensionSdkKitFound, String> {
    let places: [(Result<PathBuf, String>, &'static str); 3] = [
        (
            lend_directory
                .and_then(extension_sdk_kit_directory_beside_the_lend)
                .ok_or_else(|| "this runtime was handed no runtime unit lend".to_owned()),
            "the runtime unit's",
        ),
        (
            kit_its_starter_names.map(Path::to_path_buf).ok_or_else(|| {
                format!("its starter set no {EXTENSION_SDK_KIT_ENVIRONMENT_VARIABLE}")
            }),
            "the starter's",
        ),
        (
            Ok(project_directory.join(PROJECT_INSTALLED_EXTENSION_SDK)),
            "the project's installed",
        ),
    ];
    let mut why_not = Vec::new();
    for (place, found_as) in places {
        let looked = place.and_then(|directory| {
            if directory.is_dir() {
                the_kit_at(&directory, found_as)
            } else {
                Err(format!("{} is not there", directory.display()))
            }
        });
        match looked {
            Ok(found) => return Ok(found),
            Err(why) => why_not.push(format!("{found_as} kit: {why}")),
        }
    }
    Err(format!(
        "the runtime found no extension SDK kit to check the project's views and controls \
         against. It looked for {}",
        why_not.join("; ")
    ))
}
