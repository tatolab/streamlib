// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/runtime-client`, the TypeScript client of the control API, copied from
//! `sdk/tatolab-runtime-client` into the runtime unit's `lib/tatolab/runtime-client/` beside the
//! lend, so the client a consumer takes from a unit matches the contract that unit serves.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    runtime_client_package_directory_beside_the_lend, runtime_unit_lend_directory_in_the_workspace,
};
use crate::generate_runtime_client::{
    RUNTIME_CLIENT_PACKAGE_FILES_THE_RUNTIME_UNIT_CARRIES,
    RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE,
};

/// The TypeScript client package's directory in the runtime unit of `workspace_root`.
pub fn runtime_unit_runtime_client_package_directory_in_the_workspace(
    workspace_root: &Path,
) -> Result<PathBuf> {
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    runtime_client_package_directory_beside_the_lend(&lend_directory).with_context(|| {
        format!(
            "the lend at {} has no directory beside it",
            lend_directory.display()
        )
    })
}

/// Replace the runtime unit's TypeScript client package with the workspace's.
pub fn stage_the_runtime_client_package(workspace_root: &Path) -> Result<()> {
    let staged_package_directory =
        runtime_unit_runtime_client_package_directory_in_the_workspace(workspace_root)?;
    copy_the_runtime_client_package(
        &workspace_root.join(RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE),
        &staged_package_directory,
    )?;
    tracing::info!(
        "build-runtime: @tatolab/runtime-client staged in {}",
        staged_package_directory.display()
    );
    Ok(())
}

fn copy_the_runtime_client_package(
    package_directory: &Path,
    staged_package_directory: &Path,
) -> Result<()> {
    if staged_package_directory.exists() {
        std::fs::remove_dir_all(staged_package_directory)
            .with_context(|| format!("removing {}", staged_package_directory.display()))?;
    }
    std::fs::create_dir_all(staged_package_directory)
        .with_context(|| format!("creating {}", staged_package_directory.display()))?;
    for file_name in RUNTIME_CLIENT_PACKAGE_FILES_THE_RUNTIME_UNIT_CARRIES {
        let package_file = package_directory.join(file_name);
        std::fs::copy(&package_file, staged_package_directory.join(file_name)).with_context(
            || {
                format!(
                    "copying {} into {}",
                    package_file.display(),
                    staged_package_directory.display()
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_staged_package_holds_every_file_its_package_json_publishes() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let package_directory = workspace_root.join(RUNTIME_CLIENT_PACKAGE_RELATIVE_TO_WORKSPACE);
        let staged = tempfile::tempdir().unwrap();
        let staged_package_directory = staged.path().join("runtime-client");
        std::fs::create_dir_all(&staged_package_directory).unwrap();
        std::fs::write(staged_package_directory.join("stale.js"), "").unwrap();

        copy_the_runtime_client_package(&package_directory, &staged_package_directory).unwrap();

        let package_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(package_directory.join("package.json")).unwrap(),
        )
        .unwrap();
        let mut published_files: Vec<&str> = package_json["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file.as_str().unwrap())
            .chain(["package.json"])
            .collect();
        published_files.sort_unstable();
        let mut staged_files: Vec<String> = std::fs::read_dir(&staged_package_directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        staged_files.sort_unstable();
        assert_eq!(staged_files, published_files);
    }
}
