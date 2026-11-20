// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::check_no_tatolab_namespace_package_init::ensure_lend_directory_keeps_tatolab_a_namespace;
use crate::stage_browser_host::stage_the_browser_host;
use crate::stage_core_projects::stage_the_core_nodes_project;
use crate::stage_first_party_modules::stage_the_first_party_modules;
use crate::stage_runtime_client_package::stage_the_runtime_client_package;
use crate::stage_runtime_unit_carried_packages::stage_the_carried_packages;
use crate::stage_runtime_unit_code_checkers::stage_the_runtime_unit_code_checkers;
use crate::stage_runtime_unit_uv::stage_the_runtime_unit_uv;
use crate::stage_sample_projects::stage_the_sample_projects;
use anyhow::{Context, Result, bail};
use engine_build_id_composition::{
    RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE, mint_per_build_nonce,
};
use runtime_unit_layout::{
    BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND,
    RUNTIME_UNIT_LINTER_EXECUTABLE_FILE_NAME,
    RUNTIME_UNIT_TYPESCRIPT_COMPILER_RELATIVE_TO_THE_BINARY_DIRECTORY,
    RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME, lend_directory_in_the_runtime_unit,
};
#[cfg(test)]
pub(crate) use runtime_unit_layout::{
    BROWSER_HOST_EXECUTABLE_FILE_NAME, BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME,
};
pub(crate) use runtime_unit_layout::{
    browser_host_directory_beside_the_lend, carried_python_downloads_directory_beside_the_lend,
    carried_python_downloads_metadata_file_in, carried_wheels_directory_beside_the_lend,
    core_nodes_project_directory_beside_the_lend, first_party_modules_directory_beside_the_lend,
    runtime_client_package_directory_beside_the_lend, sample_projects_directory_beside_the_lend,
};
use std::path::{Path, PathBuf};

#[path = "../../../runtime/streamlib-engine/src/core/engine_build_id_composition.rs"]
#[allow(
    dead_code,
    reason = "xtask shares the engine's source file and uses part of it"
)]
mod engine_build_id_composition;

#[path = "../../../runtime/streamlib-consumer-rhi/src/runtime_unit_layout.rs"]
#[allow(
    dead_code,
    unreachable_pub,
    reason = "xtask shares the consumer RHI's source file, whose items are public there, and uses part of it"
)]
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

/// Where the runtime unit in `workspace_root` keeps its executables.
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
const RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS: [(&str, &str); 4] = [
    ("tatolabd", "tatolabd"),
    ("tatolab-cli", "tatolab"),
    ("tatolab-video-poster-frame", "tatolab-video-poster-frame"),
    ("tatolab-video-encode-host", "tatolab-video-encode-host"),
];

/// The cargo feature that moves the machine runtime lock, the state directory
/// and the runtime directory under `TATOLAB_TEST_MACHINE_ROOT`; each runtime-unit
/// binary package declares it under this one name.
pub const MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE: &str =
    "machine-directories-under-a-test-root";

/// The file at the runtime unit's root whose presence says every binary was
/// built with [`MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE`]; a harness
/// refuses a unit without it, so a test never touches the real machine's lock.
pub const MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_MARKER_FILE_NAME: &str =
    "machine-directories-under-a-test-root";

/// The maturin project the runtime unit is built from.
const RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE: &str = "sdk/streamlib-python-wheel";

/// The one maturin pin for building the runtime unit, as `uvx` resolves it.
pub const PINNED_MATURIN_REQUIREMENT_FOR_UVX: &str = "maturin@1.9.6";

#[cfg(target_os = "macos")]
mod bundled_vulkan_driver_on_this_host {
    /// The script that writes MoltenVK and the Vulkan loader into the maturin
    /// project's package tree, relative to the workspace.
    pub(super) const BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE: &str =
        "scripts/stage_macos_bundled_vulkan_driver.sh";

    /// Why a lend without the bundled Vulkan driver is refused.
    pub(super) const WHO_HAS_NO_VULKAN_DRIVER_BUT_THE_BUNDLED_ONE: &str =
        "a stock Mac has no Vulkan driver of its own";
}

#[cfg(target_os = "linux")]
mod bundled_vulkan_driver_on_this_host {
    /// The script that writes lavapipe and the Vulkan loader into the maturin
    /// project's package tree, relative to the workspace.
    pub(super) const BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE: &str =
        "scripts/stage_linux_bundled_vulkan_driver.sh";

    /// Why a lend without the bundled Vulkan driver is refused.
    pub(super) const WHO_HAS_NO_VULKAN_DRIVER_BUT_THE_BUNDLED_ONE: &str = "a machine with no GPU driver installed — a bare container, a headless server — has \
         no Vulkan driver of its own";
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
use bundled_vulkan_driver_on_this_host::{
    BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE,
    WHO_HAS_NO_VULKAN_DRIVER_BUT_THE_BUNDLED_ONE,
};

/// The script that writes the Slang shader compiler into the maturin
/// project's package tree and the workspace's `target/`, relative to the
/// workspace.
const SLANG_COMPILER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE: &str =
    "scripts/stage_slang_compiler_libraries.sh";

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
    macos_developer_id_application_identity: Option<&str>,
) -> Result<()> {
    let maturin_project_directory =
        workspace_root.join(RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE);
    let wheel_directory = runtime_unit_wheel_directory_in_the_workspace(workspace_root);
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    stage_bundled_vulkan_driver(workspace_root)?;
    stage_slang_compiler_libraries(workspace_root)?;
    let macos_deployment_target = if cfg!(target_os = "macos") {
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
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    ensure_lend_carries_the_bundled_vulkan_driver(&lend_directory)?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    ensure_lend_carries_the_slang_compiler(&lend_directory)?;
    tracing::info!(
        "build-runtime: {} unpacked into the lend at {}",
        runtime_unit_wheel.display(),
        lend_directory.display()
    );

    stage_the_browser_host(workspace_root)?;
    stage_the_first_party_modules(workspace_root)?;
    stage_the_core_nodes_project(workspace_root)?;
    stage_the_sample_projects(workspace_root)?;
    stage_the_runtime_client_package(workspace_root)?;
    stage_the_carried_packages(workspace_root, macos_developer_id_application_identity)?;

    let bin_directory = runtime_unit_binary_directory_in_the_workspace(workspace_root);
    let mut built_binaries = build_runtime_unit_binaries(
        workspace_root,
        build_profile,
        machine_directories,
        macos_deployment_target.as_deref(),
        &runtime_unit_engine_build_nonce,
    )?;
    built_binaries.push(BuiltRuntimeUnitBinary {
        binary_target_name: RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME.to_string(),
        built_executable: stage_the_runtime_unit_uv(workspace_root)?,
    });
    let code_checkers = stage_the_runtime_unit_code_checkers(workspace_root)?;
    built_binaries.push(BuiltRuntimeUnitBinary {
        binary_target_name: RUNTIME_UNIT_LINTER_EXECUTABLE_FILE_NAME.to_string(),
        built_executable: code_checkers.linter,
    });
    replace_runtime_unit_binaries_in(&bin_directory, &built_binaries)?;
    replace_the_typescript_compiler_in(
        &bin_directory,
        &code_checkers.typescript_compiler_directory,
    )?;
    tracing::info!(
        "build-runtime: tatolabd, tatolab, tatolab-video-poster-frame, uv, oxlint and \
         {RUNTIME_UNIT_TYPESCRIPT_COMPILER_RELATIVE_TO_THE_BINARY_DIRECTORY} placed in {}",
        bin_directory.display()
    );
    if machine_directories == RuntimeUnitMachineDirectories::UnderATestRoot {
        write_the_machine_directories_marker(&machine_directories_marker)?;
        tracing::info!(
            "build-runtime: every binary keeps the machine's directories under \
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
    macos_deployment_target: Option<&str>,
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
    if let Some(macos_deployment_target) = macos_deployment_target {
        cargo_build.env("MACOSX_DEPLOYMENT_TARGET", macos_deployment_target);
    }

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

/// Replace the TypeScript compiler's directory in `bin_directory` with a copy
/// of `compiler_directory`: `tsc` and the library files it reads from beside
/// its own real path. Copied, never linked, so that path is the unit's own.
pub fn replace_the_typescript_compiler_in(
    bin_directory: &Path,
    compiler_directory: &Path,
) -> Result<()> {
    let placed_compiler =
        bin_directory.join(RUNTIME_UNIT_TYPESCRIPT_COMPILER_RELATIVE_TO_THE_BINARY_DIRECTORY);
    let placed_directory = placed_compiler
        .parent()
        .with_context(|| format!("{} sits in no directory", placed_compiler.display()))?;
    if placed_directory.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(placed_directory)
            .with_context(|| format!("removing {}", placed_directory.display()))?;
    }
    std::fs::create_dir_all(placed_directory)
        .with_context(|| format!("creating {}", placed_directory.display()))?;
    for entry in std::fs::read_dir(compiler_directory)
        .with_context(|| format!("reading {}", compiler_directory.display()))?
    {
        let entry = entry.with_context(|| format!("reading {}", compiler_directory.display()))?;
        let placed_file = placed_directory.join(entry.file_name());
        std::fs::copy(entry.path(), &placed_file).with_context(|| {
            format!(
                "copying {} to {}",
                entry.path().display(),
                placed_file.display()
            )
        })?;
    }
    if !placed_compiler.is_file() {
        bail!(
            "{} holds no {}",
            compiler_directory.display(),
            placed_compiler
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
    }
    Ok(())
}

/// Run the platform's staging script, which writes the Vulkan loader, the
/// driver and its ICD manifest into the maturin project's package tree.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stage_bundled_vulkan_driver(workspace_root: &Path) -> Result<()> {
    let staging_script =
        workspace_root.join(BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE);
    let exit_status = std::process::Command::new("bash")
        .arg(&staging_script)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("failed to run {}", staging_script.display()))?;
    anyhow::ensure!(
        exit_status.success(),
        "{} failed ({exit_status}) — the lend would carry no Vulkan driver, and \
         {WHO_HAS_NO_VULKAN_DRIVER_BUT_THE_BUNDLED_ONE}",
        staging_script.display()
    );
    Ok(())
}

/// The runtime unit carries a bundled Vulkan driver on macOS and Linux only,
/// so no other host builds one.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn stage_bundled_vulkan_driver(_workspace_root: &Path) -> Result<()> {
    anyhow::bail!("`cargo xtask build-runtime` builds the runtime unit on macOS and Linux only")
}

/// Run the script that stages the Slang compiler the engine opens at run time.
fn stage_slang_compiler_libraries(workspace_root: &Path) -> Result<()> {
    let staging_script = workspace_root.join(SLANG_COMPILER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE);
    let exit_status = std::process::Command::new("bash")
        .arg(&staging_script)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("failed to run {}", staging_script.display()))?;
    anyhow::ensure!(
        exit_status.success(),
        "{} failed ({exit_status}) — the lend would carry no shader compiler, and no kernel \
         written in Slang could be built",
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

/// Refuse a lend missing any file of the bundled Vulkan driver.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ensure_lend_carries_the_bundled_vulkan_driver(lend_directory: &Path) -> Result<()> {
    let bundled_vulkan_driver_directory =
        runtime_unit_layout::bundled_vulkan_driver_directory_in_the_lend(lend_directory);
    let missing_driver_files: Vec<&str> = runtime_unit_layout::BUNDLED_VULKAN_DRIVER_FILE_NAMES
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
        "the lend's {} lacks {} — {WHO_HAS_NO_VULKAN_DRIVER_BUT_THE_BUNDLED_ONE}",
        bundled_vulkan_driver_directory.display(),
        missing_driver_files.join(", ")
    );
    Ok(())
}

/// Refuse a lend missing either Slang compiler library.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ensure_lend_carries_the_slang_compiler(lend_directory: &Path) -> Result<()> {
    let bundled_slang_compiler_directory =
        runtime_unit_layout::bundled_slang_compiler_directory_in_the_lend(lend_directory);
    let missing_compiler_files: Vec<&str> = runtime_unit_layout::BUNDLED_SLANG_COMPILER_FILE_NAMES
        .iter()
        .copied()
        .filter(|compiler_file_name| {
            !bundled_slang_compiler_directory
                .join(compiler_file_name)
                .is_file()
        })
        .collect();
    anyhow::ensure!(
        missing_compiler_files.is_empty(),
        "the lend's {} lacks {} — no kernel written in Slang could be built",
        bundled_slang_compiler_directory.display(),
        missing_compiler_files.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests;
