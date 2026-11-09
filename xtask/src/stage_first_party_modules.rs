// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The first-party modules the runtime unit carries, each copied from its
//! package in the workspace into the unit's `lib/tatolab/imports/<module>/`
//! beside the lend, from where the runtime installs it into every project's
//! `.venv`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    first_party_modules_directory_beside_the_lend, runtime_unit_lend_directory_in_the_workspace,
};

/// Each first-party module, as the workspace-relative package it is copied from.
pub const FIRST_PARTY_MODULE_PACKAGES_RELATIVE_TO_WORKSPACE: [&str; 1] = ["packages/tatolab-ops"];

/// What of a module's package the unit carries: what builds and installs it.
pub const FIRST_PARTY_MODULE_ENTRIES_A_UNIT_CARRIES: [&str; 5] = [
    "pyproject.toml",
    "hatch_build.py",
    "README.md",
    "LICENSE",
    "python",
];

/// The cache directory a module's sources never carry into the unit.
const PYTHON_BYTECODE_CACHE_DIRECTORY_NAME: &str = "__pycache__";

/// The first-party modules directory of the runtime unit in `workspace_root`.
pub fn runtime_unit_first_party_modules_directory_in_the_workspace(
    workspace_root: &Path,
) -> Result<PathBuf> {
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    first_party_modules_directory_beside_the_lend(&lend_directory).with_context(|| {
        format!(
            "the lend at {} has no directory beside it",
            lend_directory.display()
        )
    })
}

/// Replace the runtime unit's first-party modules directory with a fresh copy
/// of each first-party module in `workspace_root`.
pub fn stage_the_first_party_modules(workspace_root: &Path) -> Result<()> {
    let modules_directory =
        runtime_unit_first_party_modules_directory_in_the_workspace(workspace_root)?;
    if modules_directory.exists() {
        std::fs::remove_dir_all(&modules_directory)
            .with_context(|| format!("removing {}", modules_directory.display()))?;
    }
    for package in FIRST_PARTY_MODULE_PACKAGES_RELATIVE_TO_WORKSPACE {
        let package_directory = workspace_root.join(package);
        let module_folder_name = package_directory
            .file_name()
            .with_context(|| format!("{package} names no folder"))?;
        copy_a_first_party_module(
            &package_directory,
            &modules_directory.join(module_folder_name),
        )?;
    }
    tracing::info!(
        "build-runtime: first-party modules staged in {}",
        modules_directory.display()
    );
    Ok(())
}

/// Copy what the unit carries of the module at `package_directory` into
/// `module_folder`, each link copied as the file it points at.
pub fn copy_a_first_party_module(package_directory: &Path, module_folder: &Path) -> Result<()> {
    std::fs::create_dir_all(module_folder)
        .with_context(|| format!("creating {}", module_folder.display()))?;
    for entry in FIRST_PARTY_MODULE_ENTRIES_A_UNIT_CARRIES {
        let source = package_directory.join(entry);
        if !source.exists() {
            continue;
        }
        copy_following_links(&source, &module_folder.join(entry))?;
    }
    Ok(())
}

fn copy_following_links(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_dir() {
        std::fs::copy(source, destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
        return Ok(());
    }
    std::fs::create_dir_all(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("reading {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("reading {}", source.display()))?;
        if entry.file_name() == PYTHON_BYTECODE_CACHE_DIRECTORY_NAME {
            continue;
        }
        copy_following_links(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_module_is_copied_with_its_links_resolved_and_without_its_tests_or_bytecode() {
        let scratch = tempfile::tempdir().unwrap();
        let package = scratch.path().join("tatolab-ops");
        std::fs::create_dir_all(package.join("python/tatolab/ops/__pycache__")).unwrap();
        std::fs::create_dir_all(package.join("tests")).unwrap();
        std::fs::write(
            package.join("pyproject.toml"),
            "[project]\nname = \"tatolab-ops\"\n",
        )
        .unwrap();
        std::fs::write(package.join("python/tatolab/ops/__init__.py"), "").unwrap();
        std::fs::write(package.join("python/tatolab/ops/__pycache__/x.pyc"), "").unwrap();
        std::fs::write(package.join("tests/test_ops.py"), "").unwrap();
        std::fs::write(scratch.path().join("LICENSE"), "licence text").unwrap();
        std::os::unix::fs::symlink("../LICENSE", package.join("LICENSE")).unwrap();
        let module_folder = scratch.path().join("unit/imports/tatolab-ops");

        copy_a_first_party_module(&package, &module_folder).unwrap();

        assert!(module_folder.join("pyproject.toml").is_file());
        assert!(
            module_folder
                .join("python/tatolab/ops/__init__.py")
                .is_file()
        );
        assert!(
            !module_folder
                .join("python/tatolab/ops/__pycache__")
                .exists()
        );
        assert!(!module_folder.join("tests").exists());
        let licence = module_folder.join("LICENSE");
        assert!(!licence.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(licence).unwrap(), "licence text");
    }

    #[test]
    fn every_first_party_module_package_is_in_the_workspace() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for package in FIRST_PARTY_MODULE_PACKAGES_RELATIVE_TO_WORKSPACE {
            assert!(
                workspace_root
                    .join(package)
                    .join("pyproject.toml")
                    .is_file(),
                "{package}"
            );
        }
    }
}
