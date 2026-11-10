// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The first-party modules the runtime unit carries, each built from its
//! package in the workspace into one wheel in the unit's `lib/tatolab/imports/`
//! beside the lend, from where the runtime installs it into every project's
//! `.venv`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    PINNED_MATURIN_REQUIREMENT_FOR_UVX, RuntimeUnitBuildProfile,
    first_party_modules_directory_beside_the_lend, macos_deployment_target_from_maturin_pyproject,
    runtime_unit_lend_directory_in_the_workspace, the_one_wheel_in,
};

/// The platforms a first-party module is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstPartyModulePlatforms {
    /// Linux and macOS.
    LinuxAndMacos,
    /// Linux alone: what it drives has no macOS counterpart yet.
    LinuxOnly,
}

impl FirstPartyModulePlatforms {
    /// Whether a unit built on this host carries the module.
    pub fn includes_this_host(self) -> bool {
        match self {
            Self::LinuxAndMacos => true,
            Self::LinuxOnly => cfg!(target_os = "linux"),
        }
    }
}

/// A first-party module, as the workspace-relative package its wheel is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstPartyModulePackage {
    /// The package directory, relative to the workspace.
    pub package_relative_to_workspace: &'static str,
    /// The platforms the unit carries it on.
    pub platforms: FirstPartyModulePlatforms,
}

const fn on_linux_and_macos(package: &'static str) -> FirstPartyModulePackage {
    FirstPartyModulePackage {
        package_relative_to_workspace: package,
        platforms: FirstPartyModulePlatforms::LinuxAndMacos,
    }
}

const fn on_linux_only(package: &'static str) -> FirstPartyModulePackage {
    FirstPartyModulePackage {
        package_relative_to_workspace: package,
        platforms: FirstPartyModulePlatforms::LinuxOnly,
    }
}

/// Every first-party module the runtime unit carries.
pub const FIRST_PARTY_MODULE_PACKAGES: [FirstPartyModulePackage; 14] = [
    on_linux_and_macos("packages/streamlib-h264"),
    on_linux_and_macos("packages/streamlib-h265"),
    on_linux_and_macos("packages/streamlib-webrtc"),
    on_linux_only("packages/tatolab-browser"),
    on_linux_and_macos("packages/tatolab-camera"),
    on_linux_and_macos("packages/tatolab-compositor"),
    on_linux_and_macos("packages/tatolab-microphone"),
    on_linux_and_macos("packages/tatolab-moq"),
    on_linux_and_macos("packages/tatolab-ops"),
    on_linux_and_macos("packages/tatolab-opus"),
    on_linux_only("packages/tatolab-rive"),
    on_linux_and_macos("packages/tatolab-speaker"),
    on_linux_and_macos("packages/tatolab-testpattern"),
    on_linux_only("packages/tatolab-virtualcamera"),
];

/// The build backend a package's `pyproject.toml` names for a native extension.
const MATURIN_BUILD_BACKEND: &str = "maturin";

/// Where each module's wheel is built before it is placed in the unit, relative to the workspace.
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
/// built wheel of each first-party module this host carries.
pub fn stage_the_first_party_modules(
    workspace_root: &Path,
    build_profile: RuntimeUnitBuildProfile,
) -> Result<()> {
    let modules_directory =
        runtime_unit_first_party_modules_directory_in_the_workspace(workspace_root)?;
    if modules_directory.exists() {
        std::fs::remove_dir_all(&modules_directory)
            .with_context(|| format!("removing {}", modules_directory.display()))?;
    }
    std::fs::create_dir_all(&modules_directory)
        .with_context(|| format!("creating {}", modules_directory.display()))?;
    for package in FIRST_PARTY_MODULE_PACKAGES
        .iter()
        .filter(|package| package.platforms.includes_this_host())
    {
        let package_directory = workspace_root.join(package.package_relative_to_workspace);
        let module_wheel =
            build_a_first_party_module_wheel(workspace_root, &package_directory, build_profile)?;
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

/// How a first-party module's package is built into a wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstPartyModuleWheelBuild {
    /// Pure Python: `uv build --wheel`.
    PurePython,
    /// A native extension: maturin, at the macOS deployment target its
    /// `pyproject.toml` names when built on macOS.
    NativeExtension {
        macos_deployment_target: Option<String>,
    },
}

/// How the package whose `pyproject.toml` reads `pyproject_text` is built.
pub fn how_a_first_party_module_wheel_is_built(
    pyproject_text: &str,
) -> Result<FirstPartyModuleWheelBuild> {
    let pyproject: toml::Table = pyproject_text.parse().context("parsing pyproject.toml")?;
    let build_backend = pyproject
        .get("build-system")
        .and_then(|build_system| build_system.get("build-backend"))
        .and_then(toml::Value::as_str)
        .context("pyproject.toml names no [build-system].build-backend")?;
    if build_backend != MATURIN_BUILD_BACKEND {
        return Ok(FirstPartyModuleWheelBuild::PurePython);
    }
    let macos_deployment_target = if cfg!(target_os = "macos") {
        Some(macos_deployment_target_from_maturin_pyproject(
            pyproject_text,
        )?)
    } else {
        None
    };
    Ok(FirstPartyModuleWheelBuild::NativeExtension {
        macos_deployment_target,
    })
}

fn build_a_first_party_module_wheel(
    workspace_root: &Path,
    package_directory: &Path,
    build_profile: RuntimeUnitBuildProfile,
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
    let pyproject_path = package_directory.join("pyproject.toml");
    let pyproject_text = std::fs::read_to_string(&pyproject_path)
        .with_context(|| format!("reading {}", pyproject_path.display()))?;
    let wheel_build = how_a_first_party_module_wheel_is_built(&pyproject_text)
        .with_context(|| format!("reading {}", pyproject_path.display()))?;
    let (mut build_command, described_command) = match wheel_build {
        FirstPartyModuleWheelBuild::PurePython => {
            let mut uv_build = std::process::Command::new("uv");
            uv_build
                .args(["build", "--wheel", "--out-dir"])
                .arg(&wheel_directory)
                .arg(package_directory);
            (uv_build, "uv build --wheel".to_owned())
        }
        FirstPartyModuleWheelBuild::NativeExtension {
            macos_deployment_target,
        } => {
            let mut maturin_build = std::process::Command::new("uvx");
            maturin_build
                .args([PINNED_MATURIN_REQUIREMENT_FOR_UVX, "build", "--out"])
                .arg(&wheel_directory)
                .current_dir(package_directory);
            if build_profile == RuntimeUnitBuildProfile::Release {
                maturin_build.arg("--release");
            }
            if let Some(macos_deployment_target) = macos_deployment_target {
                maturin_build.env("MACOSX_DEPLOYMENT_TARGET", macos_deployment_target);
            }
            (
                maturin_build,
                format!("uvx {PINNED_MATURIN_REQUIREMENT_FOR_UVX} build"),
            )
        }
    };
    let exit_status = build_command
        .status()
        .with_context(|| format!("failed to run `{described_command}` — is uv on PATH?"))?;
    anyhow::ensure!(
        exit_status.success(),
        "`{described_command}` of {} failed ({exit_status})",
        package_directory.display()
    );
    the_one_wheel_in(&wheel_directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_workspace_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
    }

    #[test]
    fn every_package_holding_a_python_project_is_a_first_party_module_the_unit_carries() {
        let packages_directory = the_workspace_root().join("packages");
        let mut packages_holding_a_python_project: Vec<String> =
            std::fs::read_dir(&packages_directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|package| package.join("pyproject.toml").is_file())
                .map(|package| {
                    format!(
                        "packages/{}",
                        package.file_name().unwrap().to_str().unwrap()
                    )
                })
                .collect();
        packages_holding_a_python_project.sort();
        let carried: Vec<&str> = FIRST_PARTY_MODULE_PACKAGES
            .iter()
            .map(|package| package.package_relative_to_workspace)
            .collect();
        assert_eq!(packages_holding_a_python_project, carried);
    }

    #[test]
    fn a_native_module_is_built_with_maturin_and_a_pure_one_with_uv() {
        assert_eq!(
            how_a_first_party_module_wheel_is_built(
                "[build-system]\nbuild-backend = \"hatchling.build\"\n"
            )
            .unwrap(),
            FirstPartyModuleWheelBuild::PurePython
        );
        let native = how_a_first_party_module_wheel_is_built(
            "[build-system]\nbuild-backend = \"maturin\"\n\n\
             [tool.maturin.target.aarch64-apple-darwin]\nmacos-deployment-target = \"26.0\"\n",
        )
        .unwrap();
        assert_eq!(
            native,
            FirstPartyModuleWheelBuild::NativeExtension {
                macos_deployment_target: cfg!(target_os = "macos").then(|| "26.0".to_owned()),
            }
        );
        assert!(how_a_first_party_module_wheel_is_built("[project]\nname = \"x\"\n").is_err());
    }

    #[test]
    fn every_module_built_on_macos_names_its_macos_deployment_target() {
        for package in FIRST_PARTY_MODULE_PACKAGES
            .iter()
            .filter(|package| package.platforms == FirstPartyModulePlatforms::LinuxAndMacos)
        {
            let pyproject_text = std::fs::read_to_string(
                the_workspace_root()
                    .join(package.package_relative_to_workspace)
                    .join("pyproject.toml"),
            )
            .unwrap();
            if how_a_first_party_module_wheel_is_built(&pyproject_text).unwrap()
                == FirstPartyModuleWheelBuild::PurePython
            {
                continue;
            }
            assert!(
                macos_deployment_target_from_maturin_pyproject(&pyproject_text).is_ok(),
                "{}",
                package.package_relative_to_workspace
            );
        }
    }
}
