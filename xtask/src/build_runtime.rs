// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask build-runtime`: the runtime unit's one build.
//!
//! Builds the maturin project as a wheel, then lays the wheel's contents out as
//! the lend — the directory holding `tatolab/runtime/` that a processor
//! interpreter puts first on `PYTHONPATH`. The wheel is kept beside the lend
//! because CI hands that same file to the jobs that install the engine. Then
//! builds `tatolabd` and `tatolab` and places them in `bin/`, beside `lib/`,
//! the install prefix's own shape, in which `tatolabd` finds the lend from its
//! own directory.
//!
//! The unit's layout below its root is `streamlib-consumer-rhi`'s
//! `runtime_unit_layout`, included by path because xtask does not link the
//! engine; only where the root sits in the workspace, and the `wheel/` CI
//! hands on, are xtask's own.

use crate::check_no_tatolab_namespace_package_init::ensure_lend_directory_keeps_tatolab_a_namespace;
use anyhow::{Context, Result};
use engine_build_id_composition::{
    RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE, mint_per_build_nonce,
};
use runtime_unit_layout::{
    BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, BUNDLED_VULKAN_DRIVER_FILE_NAMES,
    LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND, bundled_vulkan_driver_directory_in_the_lend,
    lend_directory_in_the_runtime_unit,
};
use std::path::{Path, PathBuf};

#[path = "../../runtime/streamlib-engine/src/core/engine_build_id_composition.rs"]
#[allow(dead_code)]
mod engine_build_id_composition;

#[path = "../../runtime/streamlib-consumer-rhi/src/runtime_unit_layout.rs"]
#[allow(dead_code)]
mod runtime_unit_layout;

/// Where `cargo xtask build-runtime` lays out the runtime unit, relative to the
/// workspace.
pub const RUNTIME_UNIT_ROOT_RELATIVE_TO_WORKSPACE: &str = "target/tatolab-runtime";

/// Where the runtime unit's wheel is kept, relative to the runtime unit's root.
const WHEEL_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT: &str = "wheel";

/// The runtime unit `cargo xtask build-runtime` lays out in `workspace_root`.
pub fn runtime_unit_root_in_the_workspace(workspace_root: &Path) -> PathBuf {
    workspace_root.join(RUNTIME_UNIT_ROOT_RELATIVE_TO_WORKSPACE)
}

/// The lend of the runtime unit in `workspace_root`.
pub fn runtime_unit_lend_directory_in_the_workspace(workspace_root: &Path) -> PathBuf {
    lend_directory_in_the_runtime_unit(&runtime_unit_root_in_the_workspace(workspace_root))
}

/// Where the runtime unit in `workspace_root` keeps its two binaries.
fn runtime_unit_binary_directory_in_the_workspace(workspace_root: &Path) -> PathBuf {
    runtime_unit_root_in_the_workspace(workspace_root)
        .join(BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
}

/// Where the runtime unit in `workspace_root` keeps its wheel.
fn runtime_unit_wheel_directory_in_the_workspace(workspace_root: &Path) -> PathBuf {
    runtime_unit_root_in_the_workspace(workspace_root)
        .join(WHEEL_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
}

/// Each binary the runtime unit carries, as the cargo package that builds it
/// and the binary target's name, which is also its file name in `bin/`.
const RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS: [(&str, &str); 2] =
    [("tatolabd", "tatolabd"), ("tatolab-cli", "tatolab")];

/// The cargo feature that moves the machine runtime lock, the state directory
/// and the runtime directory under `TATOLAB_TEST_MACHINE_ROOT`; each runtime-unit
/// binary package declares it under this one name.
pub const MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE: &str =
    "machine-directories-under-a-test-root";

/// The file at the runtime unit's root whose presence says both binaries were
/// built with [`MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE`]; a harness
/// refuses a unit without it, so a test never touches the real machine's lock.
pub const MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME: &str =
    "machine-directories-under-a-test-root";

/// The maturin project the runtime unit is built from.
const RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE: &str = "sdk/streamlib-python-wheel";

/// The one maturin pin for building the runtime unit, as `uvx` resolves it.
pub const PINNED_MATURIN_REQUIREMENT_FOR_UVX: &str = "maturin@1.9.6";

const MACOS_BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE: &str =
    "scripts/stage_macos_bundled_vulkan_driver.sh";

/// Left in the lend so the namespace gate refuses it by path.
const TATOLAB_NAMESPACE_PACKAGE_INIT_MEMBER: &str = "tatolab/__init__.py";

/// Which cargo profile maturin builds the runtime unit with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeUnitBuildProfile {
    /// Unoptimized, the profile `maturin develop` builds.
    Debug,
    /// Optimized, as a release wheel is built.
    Release,
}

impl RuntimeUnitBuildProfile {
    /// The profile `cargo xtask build-runtime [--release]` asks for.
    pub fn from_release_flag(release: bool) -> Self {
        if release { Self::Release } else { Self::Debug }
    }
}

/// Where a runtime unit's binaries keep the machine's directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeUnitMachineDirectories {
    /// The real machine's: what a release and a developer's runtime use.
    TheMachines,
    /// Under `TATOLAB_TEST_MACHINE_ROOT`, for the integration suite.
    UnderATestRoot,
}

impl RuntimeUnitMachineDirectories {
    /// What `cargo xtask build-runtime [--machine-directories-under-a-test-root]` asks for.
    pub fn from_machine_directories_under_a_test_root_flag(
        machine_directories_under_a_test_root: bool,
    ) -> Self {
        if machine_directories_under_a_test_root {
            Self::UnderATestRoot
        } else {
            Self::TheMachines
        }
    }
}

/// The marker file of the runtime unit rooted at `runtime_unit_root`.
pub fn machine_directories_under_a_test_root_marker_in(runtime_unit_root: &Path) -> PathBuf {
    runtime_unit_root.join(MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME)
}

/// Build the runtime unit's wheel and replace the lend with its contents.
pub fn run(
    workspace_root: &Path,
    build_profile: RuntimeUnitBuildProfile,
    machine_directories: RuntimeUnitMachineDirectories,
) -> Result<()> {
    let maturin_project_directory =
        workspace_root.join(RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE);
    let wheel_directory = runtime_unit_wheel_directory_in_the_workspace(workspace_root);
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    let building_for_macos = cfg!(target_os = "macos");

    let macos_deployment_target = if building_for_macos {
        stage_macos_bundled_vulkan_driver(workspace_root)?;
        let maturin_pyproject_path = maturin_project_directory.join("pyproject.toml");
        let maturin_pyproject = std::fs::read_to_string(&maturin_pyproject_path)
            .with_context(|| format!("reading {}", maturin_pyproject_path.display()))?;
        Some(macos_deployment_target_from_maturin_pyproject(
            &maturin_pyproject,
        )?)
    } else {
        None
    };

    // Gone before anything is rebuilt, so a failed build never leaves a unit
    // whose marker vouches for binaries it no longer carries.
    let machine_directories_marker = machine_directories_under_a_test_root_marker_in(
        &runtime_unit_root_in_the_workspace(workspace_root),
    );
    remove_the_machine_directories_marker(&machine_directories_marker)?;

    let runtime_unit_engine_build_nonce = mint_per_build_nonce()
        .context("reading /dev/urandom for the runtime unit's engine build nonce")?;
    build_runtime_unit_wheel(
        &maturin_project_directory,
        &wheel_directory,
        build_profile,
        macos_deployment_target.as_deref(),
        &runtime_unit_engine_build_nonce,
    )?;
    let runtime_unit_wheel = the_one_wheel_in(&wheel_directory)?;
    replace_lend_directory_with_wheel_contents(&runtime_unit_wheel, &lend_directory)?;
    if building_for_macos {
        ensure_lend_carries_macos_bundled_vulkan_driver(&lend_directory)?;
    }
    tracing::info!(
        "build-runtime: {} unpacked into the lend at {}",
        runtime_unit_wheel.display(),
        lend_directory.display()
    );

    let bin_directory = runtime_unit_binary_directory_in_the_workspace(workspace_root);
    let built_binaries = build_runtime_unit_binaries(
        workspace_root,
        build_profile,
        machine_directories,
        &runtime_unit_engine_build_nonce,
    )?;
    replace_runtime_unit_binaries_in(&bin_directory, &built_binaries)?;
    tracing::info!(
        "build-runtime: tatolabd and tatolab placed in {}",
        bin_directory.display()
    );
    if machine_directories == RuntimeUnitMachineDirectories::UnderATestRoot {
        write_the_machine_directories_marker(&machine_directories_marker)?;
        tracing::info!(
            "build-runtime: both binaries keep the machine's directories under \
             TATOLAB_TEST_MACHINE_ROOT; {} says so",
            machine_directories_marker.display()
        );
    }
    Ok(())
}

/// Remove `marker` when present.
pub fn remove_the_machine_directories_marker(marker: &Path) -> Result<()> {
    match std::fs::remove_file(marker) {
        Ok(()) => Ok(()),
        Err(absent) if absent.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(removal_failure) => {
            Err(removal_failure).with_context(|| format!("removing {}", marker.display()))
        }
    }
}

/// Write `marker`, naming the feature and the variable it makes required.
pub fn write_the_machine_directories_marker(marker: &Path) -> Result<()> {
    std::fs::write(
        marker,
        format!(
            "tatolabd and tatolab in bin/ were built with the \
             {MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE} feature: each requires \
             TATOLAB_TEST_MACHINE_ROOT and keeps the machine runtime lock, the state directory \
             and the runtime directory under it.\n"
        ),
    )
    .with_context(|| format!("writing {}", marker.display()))
}

/// The `cargo build` arguments after `build` that select the runtime unit's
/// binary packages, their profile and their features.
pub fn runtime_unit_binaries_cargo_build_selection(
    build_profile: RuntimeUnitBuildProfile,
    machine_directories: RuntimeUnitMachineDirectories,
) -> Vec<String> {
    let mut selection = Vec::new();
    for (package, _) in RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS {
        selection.push("-p".to_owned());
        selection.push(package.to_owned());
    }
    if build_profile == RuntimeUnitBuildProfile::Release {
        selection.push("--release".to_owned());
    }
    if machine_directories == RuntimeUnitMachineDirectories::UnderATestRoot {
        selection.push("--features".to_owned());
        selection.push(
            RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS
                .iter()
                .map(|(package, _)| {
                    format!("{package}/{MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE}")
                })
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    selection
}

/// One binary cargo built, and the name it takes in `bin/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltRuntimeUnitBinary {
    pub binary_target_name: String,
    pub built_executable: PathBuf,
}

fn build_runtime_unit_binaries(
    workspace_root: &Path,
    build_profile: RuntimeUnitBuildProfile,
    machine_directories: RuntimeUnitMachineDirectories,
    runtime_unit_engine_build_nonce: &str,
) -> Result<Vec<BuiltRuntimeUnitBinary>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let cargo_build_selection =
        runtime_unit_binaries_cargo_build_selection(build_profile, machine_directories);
    let mut cargo_build = std::process::Command::new(cargo);
    cargo_build
        .args(["build", "--message-format=json-render-diagnostics"])
        .args(&cargo_build_selection)
        .current_dir(workspace_root)
        .env(
            RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE,
            runtime_unit_engine_build_nonce,
        )
        .stderr(std::process::Stdio::inherit());

    let cargo_build_package_flags = cargo_build_selection.join(" ");
    let cargo_build_output = cargo_build
        .output()
        .with_context(|| format!("failed to run `cargo build {cargo_build_package_flags}`"))?;
    anyhow::ensure!(
        cargo_build_output.status.success(),
        "`cargo build {cargo_build_package_flags}` failed ({})",
        cargo_build_output.status
    );
    runtime_unit_binaries_from_cargo_build_messages(&String::from_utf8_lossy(
        &cargo_build_output.stdout,
    ))
}

/// Each runtime-unit binary's executable, read from the JSON messages
/// `cargo build --message-format=json` writes, refusing a build that names
/// one of them no executable.
pub fn runtime_unit_binaries_from_cargo_build_messages(
    cargo_build_messages: &str,
) -> Result<Vec<BuiltRuntimeUnitBinary>> {
    let mut built_binaries: Vec<BuiltRuntimeUnitBinary> = Vec::new();
    for cargo_build_message in cargo_build_messages.lines() {
        let Ok(cargo_build_message) =
            serde_json::from_str::<serde_json::Value>(cargo_build_message)
        else {
            continue;
        };
        if cargo_build_message["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(built_executable) = cargo_build_message["executable"].as_str() else {
            continue;
        };
        let binary_target_name = cargo_build_message["target"]["name"]
            .as_str()
            .unwrap_or_default();
        if RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS
            .iter()
            .any(|(_, target)| *target == binary_target_name)
        {
            built_binaries.retain(|built| built.binary_target_name != binary_target_name);
            built_binaries.push(BuiltRuntimeUnitBinary {
                binary_target_name: binary_target_name.to_owned(),
                built_executable: PathBuf::from(built_executable),
            });
        }
    }
    for (package, target) in RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS {
        anyhow::ensure!(
            built_binaries
                .iter()
                .any(|built| built.binary_target_name == target),
            "`cargo build` named no executable for the `{target}` binary of `{package}`"
        );
    }
    Ok(built_binaries)
}

/// Copy each built binary into `bin_directory` under its target name,
/// replacing what is there. Copied, never linked: `tatolabd` finds the lend
/// from its own canonical path.
pub fn replace_runtime_unit_binaries_in(
    bin_directory: &Path,
    built_binaries: &[BuiltRuntimeUnitBinary],
) -> Result<()> {
    std::fs::create_dir_all(bin_directory)
        .with_context(|| format!("creating {}", bin_directory.display()))?;
    for BuiltRuntimeUnitBinary {
        binary_target_name,
        built_executable,
    } in built_binaries
    {
        let placed_binary = bin_directory.join(binary_target_name);
        // Removed first, so a running copy keeps its own file rather than
        // having it rewritten under it.
        if placed_binary.symlink_metadata().is_ok() {
            std::fs::remove_file(&placed_binary)
                .with_context(|| format!("removing {}", placed_binary.display()))?;
        }
        std::fs::copy(built_executable, &placed_binary).with_context(|| {
            format!(
                "copying {} to {}",
                built_executable.display(),
                placed_binary.display()
            )
        })?;
    }
    Ok(())
}

fn stage_macos_bundled_vulkan_driver(workspace_root: &Path) -> Result<()> {
    let staging_script =
        workspace_root.join(MACOS_BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE);
    let exit_status = std::process::Command::new("bash")
        .arg(&staging_script)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("failed to run {}", staging_script.display()))?;
    anyhow::ensure!(
        exit_status.success(),
        "{} failed ({exit_status}) — the lend would carry no Vulkan driver, and a stock Mac has none",
        staging_script.display()
    );
    Ok(())
}

/// maturin writes this into the wheel's tag but does not hand it to the link,
/// so the binaries would otherwise claim whatever macOS rustc defaults to.
pub fn macos_deployment_target_from_maturin_pyproject(maturin_pyproject: &str) -> Result<String> {
    let parsed_pyproject: toml::Value = toml::from_str(maturin_pyproject)
        .context("parsing the maturin project's pyproject.toml")?;
    parsed_pyproject
        .get("tool")
        .and_then(|tool| tool.get("maturin"))
        .and_then(|maturin| maturin.get("target"))
        .and_then(|target| target.get("aarch64-apple-darwin"))
        .and_then(|apple_target| apple_target.get("macos-deployment-target"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .context(
            "the maturin project's pyproject.toml names no macos-deployment-target under \
             [tool.maturin.target.aarch64-apple-darwin]",
        )
}

fn build_runtime_unit_wheel(
    maturin_project_directory: &Path,
    wheel_directory: &Path,
    build_profile: RuntimeUnitBuildProfile,
    macos_deployment_target: Option<&str>,
    runtime_unit_engine_build_nonce: &str,
) -> Result<()> {
    if wheel_directory.exists() {
        std::fs::remove_dir_all(wheel_directory)
            .with_context(|| format!("clearing {}", wheel_directory.display()))?;
    }

    let mut maturin_build = std::process::Command::new("uvx");
    maturin_build
        .args([PINNED_MATURIN_REQUIREMENT_FOR_UVX, "build", "--out"])
        .arg(wheel_directory)
        .current_dir(maturin_project_directory)
        .env(
            RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE,
            runtime_unit_engine_build_nonce,
        );
    if build_profile == RuntimeUnitBuildProfile::Release {
        maturin_build.arg("--release");
    }
    if let Some(macos_deployment_target) = macos_deployment_target {
        maturin_build.env("MACOSX_DEPLOYMENT_TARGET", macos_deployment_target);
    }

    let exit_status = maturin_build.status().with_context(|| {
        format!("failed to run `uvx {PINNED_MATURIN_REQUIREMENT_FOR_UVX} build` — is uv on PATH?")
    })?;
    anyhow::ensure!(
        exit_status.success(),
        "`uvx {PINNED_MATURIN_REQUIREMENT_FOR_UVX} build` in {} failed ({exit_status})",
        maturin_project_directory.display()
    );
    Ok(())
}

/// The single wheel a build left in `wheel_directory`.
pub fn the_one_wheel_in(wheel_directory: &Path) -> Result<PathBuf> {
    let mut wheels: Vec<PathBuf> = std::fs::read_dir(wheel_directory)
        .with_context(|| format!("listing {}", wheel_directory.display()))?
        .map(|directory_entry| directory_entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("listing {}", wheel_directory.display()))?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "whl"))
        .collect();
    wheels.sort();
    anyhow::ensure!(
        wheels.len() == 1,
        "expected exactly one wheel in {}, found {}: {:?}",
        wheel_directory.display(),
        wheels.len(),
        wheels
    );
    Ok(wheels.remove(0))
}

/// Replace `lend_directory` with the wheel's contents, refusing a wheel that is
/// not exactly the runtime package and its `.dist-info`.
///
/// A refused lend is removed rather than left half-written, so a later run
/// never borrows it.
pub fn replace_lend_directory_with_wheel_contents(
    runtime_unit_wheel: &Path,
    lend_directory: &Path,
) -> Result<()> {
    let wheel_file = std::fs::File::open(runtime_unit_wheel)
        .with_context(|| format!("opening {}", runtime_unit_wheel.display()))?;
    let mut wheel_archive = zip::ZipArchive::new(wheel_file)
        .with_context(|| format!("reading {} as a zip", runtime_unit_wheel.display()))?;
    ensure_wheel_members_are_the_runtime_package_and_its_dist_info(
        runtime_unit_wheel,
        &mut wheel_archive,
    )?;

    if lend_directory.exists() {
        std::fs::remove_dir_all(lend_directory).with_context(|| {
            format!("removing the previous lend at {}", lend_directory.display())
        })?;
    }
    std::fs::create_dir_all(lend_directory)
        .with_context(|| format!("creating {}", lend_directory.display()))?;

    let unpacked_lend = wheel_archive
        .extract(lend_directory)
        .with_context(|| {
            format!(
                "unpacking {} into {}",
                runtime_unit_wheel.display(),
                lend_directory.display()
            )
        })
        .and_then(|()| ensure_lend_directory_keeps_tatolab_a_namespace(lend_directory));
    if let Err(lend_refusal) = unpacked_lend {
        let _ = std::fs::remove_dir_all(lend_directory);
        return Err(lend_refusal);
    }
    Ok(())
}

fn ensure_wheel_members_are_the_runtime_package_and_its_dist_info(
    runtime_unit_wheel: &Path,
    wheel_archive: &mut zip::ZipArchive<std::fs::File>,
) -> Result<()> {
    let lent_runtime_package_prefix = format!("{LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND}/");
    let mut dist_info_directory_names: Vec<String> = Vec::new();
    let mut carries_the_runtime_package_init = false;

    for member_index in 0..wheel_archive.len() {
        let wheel_member = wheel_archive.by_index(member_index).with_context(|| {
            format!(
                "reading member {member_index} of {}",
                runtime_unit_wheel.display()
            )
        })?;
        let member_name = wheel_member.name().to_owned();
        anyhow::ensure!(
            wheel_member.enclosed_name().is_some(),
            "{} carries `{member_name}`, which would unpack outside the lend",
            runtime_unit_wheel.display()
        );

        let first_component = member_name.split('/').next().unwrap_or_default();
        if first_component.ends_with(".dist-info") {
            if !dist_info_directory_names
                .iter()
                .any(|dist_info_directory_name| dist_info_directory_name == first_component)
            {
                dist_info_directory_names.push(first_component.to_owned());
            }
            continue;
        }
        if member_name == format!("{lent_runtime_package_prefix}__init__.py") {
            carries_the_runtime_package_init = true;
        }
        let is_a_directory_on_the_way_to_the_runtime_package =
            wheel_member.is_dir() && lent_runtime_package_prefix.starts_with(&member_name);
        anyhow::ensure!(
            member_name.starts_with(lent_runtime_package_prefix.as_str())
                || member_name == TATOLAB_NAMESPACE_PACKAGE_INIT_MEMBER
                || is_a_directory_on_the_way_to_the_runtime_package,
            "{} carries `{member_name}`, outside `{lent_runtime_package_prefix}` and its \
             .dist-info — a lend holds the runtime package and nothing else, and \
             `tatolab/stream/` is the stream venv's own",
            runtime_unit_wheel.display()
        );
    }

    anyhow::ensure!(
        carries_the_runtime_package_init,
        "{} carries no `{lent_runtime_package_prefix}__init__.py` — `tatolab.runtime` must be a \
         regular package for the lend to merge with the venv's `tatolab.stream`",
        runtime_unit_wheel.display()
    );
    anyhow::ensure!(
        dist_info_directory_names.len() == 1,
        "{} carries {} .dist-info directories ({:?}); a wheel carries exactly one",
        runtime_unit_wheel.display(),
        dist_info_directory_names.len(),
        dist_info_directory_names
    );
    Ok(())
}

/// Refuse a macOS lend missing any file of the bundled Vulkan driver.
pub fn ensure_lend_carries_macos_bundled_vulkan_driver(lend_directory: &Path) -> Result<()> {
    let bundled_vulkan_driver_directory =
        bundled_vulkan_driver_directory_in_the_lend(lend_directory);
    let missing_driver_files: Vec<&str> = BUNDLED_VULKAN_DRIVER_FILE_NAMES
        .iter()
        .copied()
        .filter(|driver_file_name| {
            !bundled_vulkan_driver_directory
                .join(driver_file_name)
                .is_file()
        })
        .collect();
    anyhow::ensure!(
        missing_driver_files.is_empty(),
        "the lend's {} lacks {} — a stock Mac has no Vulkan driver of its own",
        bundled_vulkan_driver_directory.display(),
        missing_driver_files.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    const SYNTHETIC_DIST_INFO_WHEEL_MEMBER: &str = "streamlib-0.0.0.dist-info/WHEEL";

    fn write_synthetic_wheel(wheel_path: &Path, members: &[(&str, u32)]) {
        let mut wheel_writer = zip::ZipWriter::new(std::fs::File::create(wheel_path).unwrap());
        for (member_name, unix_mode) in members {
            wheel_writer
                .start_file(
                    *member_name,
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored)
                        .unix_permissions(*unix_mode),
                )
                .unwrap();
            wheel_writer.write_all(member_name.as_bytes()).unwrap();
        }
        wheel_writer.finish().unwrap();
    }

    fn runtime_unit_members() -> Vec<(&'static str, u32)> {
        vec![
            ("tatolab/runtime/__init__.py", 0o644),
            ("tatolab/runtime/_engine.abi3.so", 0o755),
            (SYNTHETIC_DIST_INFO_WHEEL_MEMBER, 0o644),
        ]
    }

    fn lend_refusal_for_members(members: &[(&str, u32)]) -> (String, PathBuf) {
        let scratch = tempfile::TempDir::new().unwrap();
        let wheel_path = scratch
            .path()
            .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
        write_synthetic_wheel(&wheel_path, members);
        let lend_directory = lend_directory_in_the_runtime_unit(scratch.path());

        let refusal = replace_lend_directory_with_wheel_contents(&wheel_path, &lend_directory)
            .unwrap_err()
            .to_string();

        assert!(
            !lend_directory.exists(),
            "a refused lend is left behind at {}",
            lend_directory.display()
        );
        (refusal, lend_directory)
    }

    #[test]
    fn the_lend_is_replaced_with_the_wheels_runtime_package_and_dist_info() {
        let scratch = tempfile::TempDir::new().unwrap();
        let wheel_path = scratch
            .path()
            .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
        write_synthetic_wheel(&wheel_path, &runtime_unit_members());
        let lend_directory = lend_directory_in_the_runtime_unit(scratch.path());
        std::fs::create_dir_all(lend_directory.join("tatolab/runtime")).unwrap();
        std::fs::write(lend_directory.join("tatolab/runtime/stale_module.py"), "").unwrap();

        replace_lend_directory_with_wheel_contents(&wheel_path, &lend_directory).unwrap();

        assert!(
            !lend_directory
                .join("tatolab/runtime/stale_module.py")
                .exists()
        );
        assert_eq!(
            std::fs::read_to_string(lend_directory.join("tatolab/runtime/__init__.py")).unwrap(),
            "tatolab/runtime/__init__.py"
        );
        assert!(
            lend_directory
                .join(SYNTHETIC_DIST_INFO_WHEEL_MEMBER)
                .is_file()
        );
        assert!(!lend_directory.join("tatolab/__init__.py").exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_unpacked_native_extension_keeps_its_unix_mode() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::TempDir::new().unwrap();
        let wheel_path = scratch
            .path()
            .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
        write_synthetic_wheel(&wheel_path, &runtime_unit_members());
        let lend_directory = scratch.path().join("lend");

        replace_lend_directory_with_wheel_contents(&wheel_path, &lend_directory).unwrap();

        let unpacked_mode =
            std::fs::metadata(lend_directory.join("tatolab/runtime/_engine.abi3.so"))
                .unwrap()
                .permissions()
                .mode();
        assert_eq!(unpacked_mode & 0o777, 0o755);
    }

    #[test]
    fn a_wheel_carrying_tatolab_init_is_refused_by_the_namespace_gate_naming_the_lend_path() {
        let mut members = runtime_unit_members();
        members.push(("tatolab/__init__.py", 0o644));

        let (refusal, lend_directory) = lend_refusal_for_members(&members);

        assert!(
            refusal.contains(
                &lend_directory
                    .join("tatolab/__init__.py")
                    .display()
                    .to_string()
            ),
            "{refusal}"
        );
        assert!(refusal.contains("PEP 420"), "{refusal}");
    }

    #[test]
    fn a_wheel_carrying_the_stream_package_is_refused() {
        let mut members = runtime_unit_members();
        members.push(("tatolab/stream/__init__.py", 0o644));

        let (refusal, _) = lend_refusal_for_members(&members);

        assert!(
            refusal.contains("`tatolab/stream/__init__.py`"),
            "{refusal}"
        );
    }

    #[test]
    fn a_wheel_member_that_would_unpack_outside_the_lend_is_refused() {
        let mut members = runtime_unit_members();
        members.push(("../escaped.py", 0o644));

        let (refusal, _) = lend_refusal_for_members(&members);

        assert!(refusal.contains("`../escaped.py`"), "{refusal}");
        assert!(refusal.contains("outside the lend"), "{refusal}");
    }

    #[test]
    fn a_wheel_without_the_runtime_package_init_is_refused() {
        let (refusal, _) = lend_refusal_for_members(&[
            ("tatolab/runtime/_engine.abi3.so", 0o755),
            (SYNTHETIC_DIST_INFO_WHEEL_MEMBER, 0o644),
        ]);

        assert!(refusal.contains("tatolab/runtime/__init__.py"), "{refusal}");
    }

    #[test]
    fn a_wheel_with_no_dist_info_is_refused() {
        let (refusal, _) = lend_refusal_for_members(&[("tatolab/runtime/__init__.py", 0o644)]);

        assert!(refusal.contains("0 .dist-info directories"), "{refusal}");
    }

    #[test]
    fn exactly_one_wheel_is_taken_from_the_wheel_directory() {
        let wheel_directory = tempfile::TempDir::new().unwrap();
        let no_wheel = the_one_wheel_in(wheel_directory.path())
            .unwrap_err()
            .to_string();
        assert!(no_wheel.contains("found 0"), "{no_wheel}");

        let first_wheel = wheel_directory
            .path()
            .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
        std::fs::write(&first_wheel, "").unwrap();
        std::fs::write(wheel_directory.path().join("build.log"), "").unwrap();
        assert_eq!(
            the_one_wheel_in(wheel_directory.path()).unwrap(),
            first_wheel
        );

        std::fs::write(
            wheel_directory
                .path()
                .join("streamlib-0.0.1-cp310-abi3-linux_x86_64.whl"),
            "",
        )
        .unwrap();
        let two_wheels = the_one_wheel_in(wheel_directory.path())
            .unwrap_err()
            .to_string();
        assert!(two_wheels.contains("found 2"), "{two_wheels}");
    }

    fn a_compiler_artifact_message(target_name: &str, executable: Option<&str>) -> String {
        serde_json::json!({
            "reason": "compiler-artifact",
            "target": {"name": target_name, "kind": ["bin"]},
            "executable": executable,
        })
        .to_string()
    }

    #[test]
    fn the_runtime_unit_binaries_are_read_from_cargos_build_messages() {
        let cargo_build_messages = [
            a_compiler_artifact_message("streamlib_engine", None),
            r#"{"reason":"build-script-executed","package_id":"x"}"#.to_owned(),
            a_compiler_artifact_message("tatolabd", Some("/target/debug/tatolabd")),
            a_compiler_artifact_message("tatolab", Some("/target/debug/tatolab")),
            a_compiler_artifact_message("generate_openapi", Some("/target/debug/generate_openapi")),
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
        ]
        .join("\n");

        let built_binaries =
            runtime_unit_binaries_from_cargo_build_messages(&cargo_build_messages).unwrap();

        assert_eq!(
            built_binaries,
            vec![
                BuiltRuntimeUnitBinary {
                    binary_target_name: "tatolabd".to_owned(),
                    built_executable: PathBuf::from("/target/debug/tatolabd"),
                },
                BuiltRuntimeUnitBinary {
                    binary_target_name: "tatolab".to_owned(),
                    built_executable: PathBuf::from("/target/debug/tatolab"),
                },
            ]
        );
    }

    #[test]
    fn a_build_naming_no_executable_for_a_runtime_unit_binary_is_refused_naming_it() {
        let refusal = runtime_unit_binaries_from_cargo_build_messages(
            &a_compiler_artifact_message("tatolabd", Some("/target/debug/tatolabd")),
        )
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains("`tatolab` binary of `tatolab-cli`"),
            "{refusal}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_runtime_unit_binaries_replace_what_bin_holds_and_stay_executable() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::TempDir::new().unwrap();
        let built_tatolabd = scratch.path().join("built-tatolabd");
        std::fs::write(&built_tatolabd, "new tatolabd").unwrap();
        std::fs::set_permissions(&built_tatolabd, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bin_directory = runtime_unit_binary_directory_in_the_workspace(scratch.path());
        std::fs::create_dir_all(&bin_directory).unwrap();
        std::fs::write(bin_directory.join("tatolabd"), "old tatolabd").unwrap();

        replace_runtime_unit_binaries_in(
            &bin_directory,
            &[BuiltRuntimeUnitBinary {
                binary_target_name: "tatolabd".to_owned(),
                built_executable: built_tatolabd,
            }],
        )
        .unwrap();

        let placed_tatolabd = bin_directory.join("tatolabd");
        assert_eq!(
            std::fs::read_to_string(&placed_tatolabd).unwrap(),
            "new tatolabd"
        );
        assert_eq!(
            std::fs::metadata(&placed_tatolabd)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(
            !std::fs::symlink_metadata(&placed_tatolabd)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_unit_for_the_machine_builds_both_binary_packages_without_the_test_root_feature() {
        assert_eq!(
            runtime_unit_binaries_cargo_build_selection(
                RuntimeUnitBuildProfile::Release,
                RuntimeUnitMachineDirectories::TheMachines,
            ),
            ["-p", "tatolabd", "-p", "tatolab-cli", "--release"]
        );
    }

    #[test]
    fn a_unit_under_a_test_root_builds_both_binary_packages_with_the_feature() {
        assert_eq!(
            runtime_unit_binaries_cargo_build_selection(
                RuntimeUnitBuildProfile::Debug,
                RuntimeUnitMachineDirectories::UnderATestRoot,
            ),
            [
                "-p",
                "tatolabd",
                "-p",
                "tatolab-cli",
                "--features",
                "tatolabd/machine-directories-under-a-test-root,\
                 tatolab-cli/machine-directories-under-a-test-root",
            ]
        );
    }

    #[test]
    fn every_runtime_unit_binary_package_declares_the_test_root_feature() {
        for (package, _) in RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS {
            let package_manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../runtime")
                .join(package)
                .join("Cargo.toml");
            let package_manifest: toml::Value =
                toml::from_str(&std::fs::read_to_string(&package_manifest_path).unwrap()).unwrap();
            assert!(
                package_manifest["features"]
                    .get(MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE)
                    .is_some(),
                "{} declares no `{MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE}` feature",
                package_manifest_path.display()
            );
        }
    }

    #[test]
    fn the_marker_is_written_at_the_unit_root_and_removed_again() {
        let runtime_unit_root = tempfile::TempDir::new().unwrap();
        let marker = machine_directories_under_a_test_root_marker_in(runtime_unit_root.path());
        assert_eq!(
            marker,
            runtime_unit_root
                .path()
                .join("machine-directories-under-a-test-root")
        );

        remove_the_machine_directories_marker(&marker).unwrap();
        write_the_machine_directories_marker(&marker).unwrap();
        let marker_text = std::fs::read_to_string(&marker).unwrap();
        assert!(
            marker_text.contains("TATOLAB_TEST_MACHINE_ROOT"),
            "{marker_text}"
        );

        remove_the_machine_directories_marker(&marker).unwrap();
        assert!(!marker.exists());
    }

    #[test]
    fn the_macos_deployment_target_is_read_from_the_maturin_target_table() {
        let maturin_pyproject = "[tool.maturin]\npython-source = \"python\"\n\n\
             [tool.maturin.target.aarch64-apple-darwin]\nmacos-deployment-target = \"15.0\"\n";

        assert_eq!(
            macos_deployment_target_from_maturin_pyproject(maturin_pyproject).unwrap(),
            "15.0"
        );
        assert!(macos_deployment_target_from_maturin_pyproject("[tool.maturin]\n").is_err());
    }

    #[test]
    fn the_checked_in_maturin_project_pins_a_macos_deployment_target() {
        let maturin_pyproject = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE)
                .join("pyproject.toml"),
        )
        .unwrap();

        macos_deployment_target_from_maturin_pyproject(&maturin_pyproject).unwrap();
    }

    #[test]
    fn a_macos_lend_missing_a_driver_file_is_refused_naming_it() {
        let lend_directory = tempfile::TempDir::new().unwrap();
        let driver_directory = bundled_vulkan_driver_directory_in_the_lend(lend_directory.path());
        std::fs::create_dir_all(&driver_directory).unwrap();
        std::fs::write(
            driver_directory.join(runtime_unit_layout::VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME),
            "",
        )
        .unwrap();
        std::fs::write(
            driver_directory.join(runtime_unit_layout::BUNDLED_ICD_MANIFEST_FILE_NAME),
            "",
        )
        .unwrap();

        let refusal = ensure_lend_carries_macos_bundled_vulkan_driver(lend_directory.path())
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains(runtime_unit_layout::BUNDLED_MOLTENVK_LIBRARY_FILE_NAME),
            "{refusal}"
        );

        std::fs::write(
            driver_directory.join(runtime_unit_layout::BUNDLED_MOLTENVK_LIBRARY_FILE_NAME),
            "",
        )
        .unwrap();
        ensure_lend_carries_macos_bundled_vulkan_driver(lend_directory.path()).unwrap();
    }
}
