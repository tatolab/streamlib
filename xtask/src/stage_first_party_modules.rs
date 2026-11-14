// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The first-party packages the runtime unit carries, `tatolab-stream` and the core
//! modules, each built from its package in the workspace into one wheel in the unit's
//! `lib/tatolab/imports/` beside the lend, from where the runtime installs it into every
//! project's `.venv`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    first_party_modules_directory_beside_the_lend, runtime_unit_lend_directory_in_the_workspace,
    the_one_wheel_in,
};

/// The first-party packages the runtime unit carries, as the workspace-relative packages their
/// wheels are built from: `tatolab-stream`, at the runtime's own commit, and the core modules.
/// Each is pure Python; every other module is a project a project imports.
pub const FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE: [&str; 3] = [
    "sdk/tatolab-stream",
    "packages/tatolab-compositor",
    "packages/tatolab-testpattern",
];

/// The build backend a native extension names, which no package the unit carries may use.
const MATURIN_BUILD_BACKEND: &str = "maturin";

/// Where each package's wheel is built before it is placed in the unit, relative to the workspace.
const FIRST_PARTY_MODULE_WHEEL_BUILDS_RELATIVE_TO_WORKSPACE: &str =
    "target/first-party-module-wheels";

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

/// Replace the runtime unit's first-party modules directory with a freshly
/// built wheel of each first-party package the unit carries.
pub fn stage_the_first_party_modules(workspace_root: &Path) -> Result<()> {
    let modules_directory =
        runtime_unit_first_party_modules_directory_in_the_workspace(workspace_root)?;
    if modules_directory.exists() {
        std::fs::remove_dir_all(&modules_directory)
            .with_context(|| format!("removing {}", modules_directory.display()))?;
    }
    std::fs::create_dir_all(&modules_directory)
        .with_context(|| format!("creating {}", modules_directory.display()))?;
    for package in FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE {
        let module_wheel =
            build_a_first_party_module_wheel(workspace_root, &workspace_root.join(package))?;
        let wheel_file_name = module_wheel
            .file_name()
            .with_context(|| format!("{} names no file", module_wheel.display()))?;
        std::fs::copy(&module_wheel, modules_directory.join(wheel_file_name)).with_context(
            || {
                format!(
                    "copying {} into {}",
                    module_wheel.display(),
                    modules_directory.display()
                )
            },
        )?;
    }
    tracing::info!(
        "build-runtime: first-party module wheels staged in {}",
        modules_directory.display()
    );
    Ok(())
}

/// Whether the package whose `pyproject.toml` reads `pyproject_text` builds a native extension.
pub fn builds_a_native_extension(pyproject_text: &str) -> Result<bool> {
    let pyproject: toml::Table = pyproject_text.parse().context("parsing pyproject.toml")?;
    let build_backend = pyproject
        .get("build-system")
        .and_then(|build_system| build_system.get("build-backend"))
        .and_then(toml::Value::as_str)
        .context("pyproject.toml names no [build-system].build-backend")?;
    Ok(build_backend == MATURIN_BUILD_BACKEND)
}

fn build_a_first_party_module_wheel(
    workspace_root: &Path,
    package_directory: &Path,
) -> Result<PathBuf> {
    let package_folder_name = package_directory
        .file_name()
        .with_context(|| format!("{} names no folder", package_directory.display()))?;
    let wheel_directory = workspace_root
        .join(FIRST_PARTY_MODULE_WHEEL_BUILDS_RELATIVE_TO_WORKSPACE)
        .join(package_folder_name);
    if wheel_directory.exists() {
        std::fs::remove_dir_all(&wheel_directory)
            .with_context(|| format!("clearing {}", wheel_directory.display()))?;
    }
    let exit_status = std::process::Command::new("uv")
        .args(["build", "--wheel", "--out-dir"])
        .arg(&wheel_directory)
        .arg(package_directory)
        .status()
        .context("failed to run `uv build --wheel` — is uv on PATH?")?;
    anyhow::ensure!(
        exit_status.success(),
        "`uv build --wheel` of {} failed ({exit_status})",
        package_directory.display()
    );
    the_one_wheel_in(&wheel_directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_first_party_package_the_unit_carries_is_pure_python_in_the_workspace() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for package in FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE {
            let pyproject_text =
                std::fs::read_to_string(workspace_root.join(package).join("pyproject.toml"))
                    .unwrap();
            assert!(
                !builds_a_native_extension(&pyproject_text).unwrap(),
                "{package} builds a native extension, which the unit cannot build for a machine \
                 with no Rust toolchain"
            );
        }
    }

    #[test]
    fn a_maturin_backend_is_a_native_extension_and_hatchling_is_not() {
        assert!(
            !builds_a_native_extension("[build-system]\nbuild-backend = \"hatchling.build\"\n")
                .unwrap()
        );
        assert!(
            builds_a_native_extension("[build-system]\nbuild-backend = \"maturin\"\n").unwrap()
        );
        assert!(builds_a_native_extension("[project]\nname = \"x\"\n").is_err());
    }
}
