// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Naming the Vulkan driver the lend carries on macOS to the Vulkan loader.
//!
//! A stock Mac has no Vulkan driver, so the lend carries MoltenVK and its ICD
//! manifest in `tatolab/runtime/_vulkan_driver/`; nothing else does, so on
//! every other floor this changes nothing. The same rule as
//! `tatolab/runtime/_bundled_vulkan_driver.py`, which a processor interpreter
//! runs again on the environment it inherits — so the manifest is named by its
//! canonical path, and naming it twice adds nothing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The bundled ICD manifest, relative to the lend.
const BUNDLED_ICD_MANIFEST_RELATIVE_TO_THE_LEND: &str =
    "tatolab/runtime/_vulkan_driver/MoltenVK_icd.json";

/// Either one replaces the loader's driver search outright; a user who set one
/// has chosen their drivers, and adding the bundled one would override that.
const DRIVER_SEARCH_REPLACING_ENVIRONMENT_VARIABLES: [&str; 2] =
    ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"];

/// Additive: the loader enumerates these beside every driver it finds itself,
/// so a user's own Vulkan install stays discoverable.
pub(crate) const DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE: &str = "VK_ADD_DRIVER_FILES";

/// The value `VK_ADD_DRIVER_FILES` takes to name the lend's bundled ICD
/// manifest, or `None` to leave the environment unchanged: the lend carries no
/// manifest, a driver-replacing variable is set, or the manifest is already
/// named.
pub(crate) fn vk_add_driver_files_naming_the_bundled_icd_manifest(
    processor_interpreter_lend_directory: &Path,
    environment_variable: impl Fn(&str) -> Option<OsString>,
) -> Option<OsString> {
    let bundled_icd_manifest =
        processor_interpreter_lend_directory.join(BUNDLED_ICD_MANIFEST_RELATIVE_TO_THE_LEND);
    if !bundled_icd_manifest.is_file() {
        return None;
    }
    if DRIVER_SEARCH_REPLACING_ENVIRONMENT_VARIABLES
        .iter()
        .any(|name| environment_variable(name).is_some_and(|value| !value.is_empty()))
    {
        return None;
    }
    let manifests_already_added: Vec<PathBuf> =
        environment_variable(DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE)
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|manifest| !manifest.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default();
    if manifests_already_added.contains(&bundled_icd_manifest) {
        return None;
    }
    std::env::join_paths(
        manifests_already_added
            .iter()
            .map(|manifest| manifest.as_os_str())
            .chain(std::iter::once(bundled_icd_manifest.as_os_str())),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn a_lend_carrying_the_bundled_icd_manifest() -> tempfile::TempDir {
        let lend = tempfile::TempDir::new().unwrap();
        let manifest = lend.path().join(BUNDLED_ICD_MANIFEST_RELATIVE_TO_THE_LEND);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, "{}").unwrap();
        lend
    }

    fn an_environment_of(variables: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let variables: HashMap<String, OsString> = variables
            .iter()
            .map(|(name, value)| (name.to_string(), OsString::from(value)))
            .collect();
        move |name| variables.get(name).cloned()
    }

    #[test]
    fn a_lend_carrying_no_manifest_leaves_the_environment_unchanged() {
        let lend = tempfile::TempDir::new().unwrap();

        assert_eq!(
            vk_add_driver_files_naming_the_bundled_icd_manifest(
                lend.path(),
                an_environment_of(&[])
            ),
            None
        );
    }

    #[test]
    fn a_driver_replacing_variable_leaves_the_environment_unchanged() {
        let lend = a_lend_carrying_the_bundled_icd_manifest();

        for replacing_variable in DRIVER_SEARCH_REPLACING_ENVIRONMENT_VARIABLES {
            assert_eq!(
                vk_add_driver_files_naming_the_bundled_icd_manifest(
                    lend.path(),
                    an_environment_of(&[(replacing_variable, "/drivers/mine.json")])
                ),
                None,
                "{replacing_variable} set must leave the driver search alone"
            );
        }
    }

    #[test]
    fn an_empty_driver_replacing_variable_reads_as_unset() {
        let lend = a_lend_carrying_the_bundled_icd_manifest();

        assert!(
            vk_add_driver_files_naming_the_bundled_icd_manifest(
                lend.path(),
                an_environment_of(&[("VK_DRIVER_FILES", ""), ("VK_ICD_FILENAMES", "")])
            )
            .is_some()
        );
    }

    #[test]
    fn the_manifest_is_appended_once_after_those_already_added() {
        let lend = a_lend_carrying_the_bundled_icd_manifest();
        let bundled_icd_manifest = lend.path().join(BUNDLED_ICD_MANIFEST_RELATIVE_TO_THE_LEND);

        let named_alone = vk_add_driver_files_naming_the_bundled_icd_manifest(
            lend.path(),
            an_environment_of(&[]),
        )
        .unwrap();
        assert_eq!(named_alone, bundled_icd_manifest.as_os_str());

        let named_after_another = vk_add_driver_files_naming_the_bundled_icd_manifest(
            lend.path(),
            an_environment_of(&[(DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE, "/drivers/a.json")]),
        )
        .unwrap();
        assert_eq!(
            named_after_another,
            OsString::from(format!(
                "/drivers/a.json:{}",
                bundled_icd_manifest.display()
            ))
        );
    }

    #[test]
    fn naming_the_manifest_again_leaves_the_environment_unchanged() {
        let lend = a_lend_carrying_the_bundled_icd_manifest();
        let already_named = vk_add_driver_files_naming_the_bundled_icd_manifest(
            lend.path(),
            an_environment_of(&[(DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE, "/drivers/a.json")]),
        )
        .unwrap();

        assert_eq!(
            vk_add_driver_files_naming_the_bundled_icd_manifest(
                lend.path(),
                an_environment_of(&[(
                    DRIVER_SEARCH_ADDING_ENVIRONMENT_VARIABLE,
                    already_named.to_str().unwrap()
                )])
            ),
            None
        );
    }
}
