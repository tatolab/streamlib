// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Where the Slang shader compiler library is looked for.

use std::path::{Path, PathBuf};

use crate::runtime_unit_layout::BUNDLED_SLANG_COMPILER_DIRECTORY_NAME;
use crate::runtime_unit_layout::{
    SLANG_COMPILER_LIBRARY_FILE_NAME, bundled_slang_compiler_directory_in_the_lend,
    lend_directory_in_the_runtime_unit,
};
#[cfg(target_os = "macos")]
use crate::runtime_unit_layout::{
    app_bundle_contents_directory_holding_the_executable_directory,
    bundled_slang_compiler_directory_in_the_app_bundle_contents,
};
use crate::vulkan_loader_library::directory_of_the_image_this_code_is_in;

/// The Slang compiler libraries the runtime unit carries, in the order they
/// are tried: beside the image this code is in (the wheel's `_engine`), in the
/// lend beside a binary's directory, and on macOS among the app bundle's
/// `Frameworks/`. Every one is a path with a slash, so dlopen takes it
/// verbatim and no library search path is consulted.
pub fn slang_compiler_library_candidate_paths() -> Vec<PathBuf> {
    directory_of_the_image_this_code_is_in()
        .map(|directory_of_the_image| {
            slang_compiler_libraries_the_runtime_unit_carries_for_the_image_in(
                &directory_of_the_image,
            )
        })
        .unwrap_or_default()
}

fn slang_compiler_libraries_the_runtime_unit_carries_for_the_image_in(
    directory_of_the_image: &Path,
) -> Vec<PathBuf> {
    let mut candidate_paths = vec![
        directory_of_the_image
            .join(BUNDLED_SLANG_COMPILER_DIRECTORY_NAME)
            .join(SLANG_COMPILER_LIBRARY_FILE_NAME),
    ];
    if let Some(runtime_unit_root) = directory_of_the_image.parent() {
        candidate_paths.push(
            bundled_slang_compiler_directory_in_the_lend(&lend_directory_in_the_runtime_unit(
                runtime_unit_root,
            ))
            .join(SLANG_COMPILER_LIBRARY_FILE_NAME),
        );
    }
    #[cfg(target_os = "macos")]
    if let Some(app_bundle_contents_directory) =
        app_bundle_contents_directory_holding_the_executable_directory(directory_of_the_image)
    {
        candidate_paths.push(
            bundled_slang_compiler_directory_in_the_app_bundle_contents(
                app_bundle_contents_directory,
            )
            .join(SLANG_COMPILER_LIBRARY_FILE_NAME),
        );
    }
    candidate_paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compiler_beside_the_image_is_tried_before_the_lent_one() {
        let candidate_paths = slang_compiler_libraries_the_runtime_unit_carries_for_the_image_in(
            Path::new("/opt/tatolab/bin"),
        );
        assert_eq!(
            candidate_paths[..2],
            [
                PathBuf::from("/opt/tatolab/bin/_slang_compiler")
                    .join(SLANG_COMPILER_LIBRARY_FILE_NAME),
                PathBuf::from("/opt/tatolab/lib/tatolab/lend/tatolab/runtime/_slang_compiler")
                    .join(SLANG_COMPILER_LIBRARY_FILE_NAME),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_app_bundle_binary_finds_the_compiler_among_the_bundle_frameworks() {
        let candidate_paths = slang_compiler_libraries_the_runtime_unit_carries_for_the_image_in(
            Path::new("/Applications/Tatolab.app/Contents/MacOS"),
        );
        assert!(
            candidate_paths.contains(
                &PathBuf::from(
                    "/Applications/Tatolab.app/Contents/Frameworks/tatolab-runtime/_slang_compiler"
                )
                .join(SLANG_COMPILER_LIBRARY_FILE_NAME)
            )
        );
    }
}
