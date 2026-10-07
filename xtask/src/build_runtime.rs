// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask build-runtime`: the runtime unit's one build.
//!
//! Builds the maturin project as a wheel, then lays the wheel's contents out as
//! the lend — the directory holding `tatolab/runtime/` that a processor
//! interpreter puts first on `PYTHONPATH`. The wheel is kept beside the lend
//! because CI hands that same file to the jobs that install the engine.

use crate::check_no_tatolab_namespace_package_init::{
    RUNTIME_UNIT_LEND_DIRECTORY_RELATIVE_TO_WORKSPACE,
    ensure_lend_directory_keeps_tatolab_a_namespace,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Where the runtime unit's wheel is written, relative to the workspace.
pub const RUNTIME_UNIT_WHEEL_DIRECTORY_RELATIVE_TO_WORKSPACE: &str = "target/tatolab-runtime/wheel";

/// The maturin project the runtime unit is built from.
const RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE: &str = "sdk/streamlib-python-wheel";

/// The one maturin pin for building the runtime unit, as `uvx` resolves it.
pub const PINNED_MATURIN_REQUIREMENT_FOR_UVX: &str = "maturin@1.9.6";

const MACOS_BUNDLED_VULKAN_DRIVER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE: &str =
    "scripts/stage_macos_bundled_vulkan_driver.sh";

/// What the staging script writes; the engine dlopens the loader beside `_engine`.
const MACOS_BUNDLED_VULKAN_DRIVER_FILE_NAMES: &[&str] = &[
    "libvulkan.1.dylib",
    "libMoltenVK.dylib",
    "MoltenVK_icd.json",
];

/// The package every lend exists to carry.
const LENT_RUNTIME_PACKAGE_PREFIX: &str = "tatolab/runtime/";

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

/// Build the runtime unit's wheel and replace the lend with its contents.
pub fn run(workspace_root: &Path, build_profile: RuntimeUnitBuildProfile) -> Result<()> {
    let maturin_project_directory =
        workspace_root.join(RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE);
    let wheel_directory = workspace_root.join(RUNTIME_UNIT_WHEEL_DIRECTORY_RELATIVE_TO_WORKSPACE);
    let lend_directory = workspace_root.join(RUNTIME_UNIT_LEND_DIRECTORY_RELATIVE_TO_WORKSPACE);
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

    build_runtime_unit_wheel(
        &maturin_project_directory,
        &wheel_directory,
        build_profile,
        macos_deployment_target.as_deref(),
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
) -> Result<()> {
    if wheel_directory.exists() {
        std::fs::remove_dir_all(wheel_directory)
            .with_context(|| format!("clearing {}", wheel_directory.display()))?;
    }

    let mut maturin_build = std::process::Command::new("uvx");
    maturin_build
        .args([PINNED_MATURIN_REQUIREMENT_FOR_UVX, "build", "--out"])
        .arg(wheel_directory)
        .current_dir(maturin_project_directory);
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
        if member_name == format!("{LENT_RUNTIME_PACKAGE_PREFIX}__init__.py") {
            carries_the_runtime_package_init = true;
        }
        let is_a_directory_on_the_way_to_the_runtime_package =
            wheel_member.is_dir() && LENT_RUNTIME_PACKAGE_PREFIX.starts_with(&member_name);
        anyhow::ensure!(
            member_name.starts_with(LENT_RUNTIME_PACKAGE_PREFIX)
                || member_name == TATOLAB_NAMESPACE_PACKAGE_INIT_MEMBER
                || is_a_directory_on_the_way_to_the_runtime_package,
            "{} carries `{member_name}`, outside `{LENT_RUNTIME_PACKAGE_PREFIX}` and its \
             .dist-info — a lend holds the runtime package and nothing else, and \
             `tatolab/stream/` is the stream venv's own",
            runtime_unit_wheel.display()
        );
    }

    anyhow::ensure!(
        carries_the_runtime_package_init,
        "{} carries no `{LENT_RUNTIME_PACKAGE_PREFIX}__init__.py` — `tatolab.runtime` must be a \
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
    let bundled_vulkan_driver_directory = lend_directory.join("tatolab/runtime/_vulkan_driver");
    let missing_driver_files: Vec<&str> = MACOS_BUNDLED_VULKAN_DRIVER_FILE_NAMES
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
        let lend_directory = scratch.path().join("lib/tatolab/lend");

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
        let lend_directory = scratch.path().join("lib/tatolab/lend");
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
        let driver_directory = lend_directory.path().join("tatolab/runtime/_vulkan_driver");
        std::fs::create_dir_all(&driver_directory).unwrap();
        std::fs::write(driver_directory.join("libvulkan.1.dylib"), "").unwrap();
        std::fs::write(driver_directory.join("MoltenVK_icd.json"), "").unwrap();

        let refusal = ensure_lend_carries_macos_bundled_vulkan_driver(lend_directory.path())
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("libMoltenVK.dylib"), "{refusal}");

        std::fs::write(driver_directory.join("libMoltenVK.dylib"), "").unwrap();
        ensure_lend_carries_macos_bundled_vulkan_driver(lend_directory.path()).unwrap();
    }
}
