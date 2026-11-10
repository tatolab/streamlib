// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The third-party packages the runtime unit carries so a new project syncs with no network:
//! a wheel of every package the core modules require, and the CPython build the unit's uv
//! downloads for the scaffold's `.python-version`, for the one platform the unit is built for.
//!
//! Each is pinned by URL and sha256 in [`RUNTIME_UNIT_CARRIED_PACKAGES_PIN_FILE_RELATIVE_TO_WORKSPACE`],
//! which `pin-runtime-unit-carried-packages` writes and `build-runtime` reads. Downloads are kept
//! under `target/`, and one whose sha256 still matches is not downloaded again.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::build_runtime::{
    carried_python_downloads_directory_beside_the_lend, carried_wheels_directory_beside_the_lend,
    runtime_unit_lend_directory_in_the_workspace,
};
use crate::stage_first_party_modules::FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE;
use crate::stage_runtime_unit_uv::{
    pinned_uv_release_archive_for_this_host, sha256_hex_of_the_file,
};

/// The pin file, relative to the workspace.
pub const RUNTIME_UNIT_CARRIED_PACKAGES_PIN_FILE_RELATIVE_TO_WORKSPACE: &str =
    "xtask/runtime_unit_carried_packages.toml";

/// Where the pinned downloads are kept, one directory per sha256, relative to the workspace.
const CARRIED_PACKAGE_DOWNLOADS_RELATIVE_TO_WORKSPACE: &str =
    "target/runtime-unit-carried-packages";

/// The scaffold's `.python-version`, the Python every carried wheel is resolved for.
const SCAFFOLD_PYTHON_VERSION_RELATIVE_TO_WORKSPACE: &str =
    "sdk/tatolab-stream/scaffold_template/python-version";

/// The macOS every macOS artefact is built for, which picks the most specific wheel.
const MACOS_DEPLOYMENT_TARGET_OF_THE_CARRIED_WHEELS: &str = "26.0";

/// Where uv's own Python download metadata points, and where python-build-standalone publishes
/// the same files.
const UV_PYTHON_DOWNLOAD_MIRROR_PREFIX: &str = "https://releases.astral.sh/github/";
const PYTHON_BUILD_STANDALONE_GITHUB_PREFIX: &str = "https://github.com/astral-sh/";

/// One platform the runtime unit ships for, as each tool names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeUnitPlatform {
    /// The Rust target triple, as the uv pin and the pin file name it.
    pub target_triple: &'static str,
    /// What `uv pip compile --python-platform` names it.
    pub uv_python_platform: &'static str,
    /// The start of uv's Python download key after `cpython-<version>-`.
    pub uv_python_download_key_platform: &'static str,
}

/// Every platform the runtime unit ships for.
pub const RUNTIME_UNIT_PLATFORMS: [RuntimeUnitPlatform; 2] = [
    RuntimeUnitPlatform {
        target_triple: "aarch64-apple-darwin",
        uv_python_platform: "aarch64-apple-darwin",
        uv_python_download_key_platform: "macos-aarch64-none",
    },
    RuntimeUnitPlatform {
        target_triple: "x86_64-unknown-linux-gnu",
        uv_python_platform: "x86_64-manylinux_2_28",
        uv_python_download_key_platform: "linux-x86_64-gnu",
    },
];

/// The pin file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeUnitCarriedPackagesPins {
    /// The CPython build the unit carries, one archive per platform.
    pub python: CarriedPythonPin,
    /// Every carried wheel, for every platform.
    pub wheels: Vec<CarriedWheelPin>,
}

/// The CPython build the unit carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedPythonPin {
    /// The full version, `3.12.15`.
    pub version: String,
    /// One archive per platform.
    pub archives: Vec<CarriedDownloadPin>,
}

/// One pinned download for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedDownloadPin {
    /// The platform's Rust target triple.
    pub target_triple: String,
    /// Where it is placed in the unit, relative to the carried directory: the wheel's file name,
    /// or `<build>/<archive file name>` for Python, the path uv reads below a mirror.
    pub path_in_the_unit: String,
    /// Where it is downloaded from.
    pub url: String,
    /// Its sha256, as lowercase hex.
    pub sha256: String,
}

/// One pinned wheel for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedWheelPin {
    /// Its distribution name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// The download, placed in the unit under its file name.
    #[serde(flatten)]
    pub download: CarriedDownloadPin,
}

/// The pins, read from the pin file in `workspace_root`.
pub fn read_the_carried_package_pins(
    workspace_root: &Path,
) -> Result<RuntimeUnitCarriedPackagesPins> {
    let pin_file =
        workspace_root.join(RUNTIME_UNIT_CARRIED_PACKAGES_PIN_FILE_RELATIVE_TO_WORKSPACE);
    let pin_text = std::fs::read_to_string(&pin_file)
        .with_context(|| format!("reading {}", pin_file.display()))?;
    toml::from_str(&pin_text).with_context(|| format!("parsing {}", pin_file.display()))
}

/// Replace the unit's carried wheels and Python downloads directories with this host's pinned
/// downloads, each checked against its sha256.
pub fn stage_the_carried_packages(workspace_root: &Path) -> Result<()> {
    let target_triple = pinned_uv_release_archive_for_this_host()?.target_triple;
    let pins = read_the_carried_package_pins(workspace_root)?;
    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    let carried_wheels_directory = carried_wheels_directory_beside_the_lend(&lend_directory)
        .with_context(|| {
            format!(
                "the lend at {} has no directory beside it",
                lend_directory.display()
            )
        })?;
    let carried_python_downloads_directory =
        carried_python_downloads_directory_beside_the_lend(&lend_directory).with_context(|| {
            format!(
                "the lend at {} has no directory beside it",
                lend_directory.display()
            )
        })?;
    let wheels_for_this_host: Vec<&CarriedDownloadPin> = pins
        .wheels
        .iter()
        .map(|wheel| &wheel.download)
        .filter(|download| download.target_triple == target_triple)
        .collect();
    let python_for_this_host: Vec<&CarriedDownloadPin> = pins
        .python
        .archives
        .iter()
        .filter(|download| download.target_triple == target_triple)
        .collect();
    if wheels_for_this_host.is_empty() || python_for_this_host.len() != 1 {
        bail!(
            "{RUNTIME_UNIT_CARRIED_PACKAGES_PIN_FILE_RELATIVE_TO_WORKSPACE} pins {} wheels and {} \
             Python archives for {target_triple}; it needs at least one wheel and one archive — run \
             `cargo xtask pin-runtime-unit-carried-packages`",
            wheels_for_this_host.len(),
            python_for_this_host.len()
        );
    }
    place_the_pinned_downloads_in(
        workspace_root,
        &wheels_for_this_host,
        &carried_wheels_directory,
    )?;
    place_the_pinned_downloads_in(
        workspace_root,
        &python_for_this_host,
        &carried_python_downloads_directory,
    )?;
    tracing::info!(
        "build-runtime: {} carried wheels in {} and Python {} in {}",
        wheels_for_this_host.len(),
        carried_wheels_directory.display(),
        pins.python.version,
        carried_python_downloads_directory.display()
    );
    Ok(())
}

/// Empty `unit_directory`, then copy each of `downloads` into it at its path in the unit.
fn place_the_pinned_downloads_in(
    workspace_root: &Path,
    downloads: &[&CarriedDownloadPin],
    unit_directory: &Path,
) -> Result<()> {
    if unit_directory.exists() {
        std::fs::remove_dir_all(unit_directory)
            .with_context(|| format!("removing {}", unit_directory.display()))?;
    }
    for download in downloads {
        let kept_download = the_kept_download_of(workspace_root, download)?;
        let placed = unit_directory.join(&download.path_in_the_unit);
        let placed_directory = placed
            .parent()
            .with_context(|| format!("{} has no parent", placed.display()))?;
        std::fs::create_dir_all(placed_directory)
            .with_context(|| format!("creating {}", placed_directory.display()))?;
        std::fs::copy(&kept_download, &placed).with_context(|| {
            format!(
                "copying {} to {}",
                kept_download.display(),
                placed.display()
            )
        })?;
    }
    Ok(())
}

/// The download `download` pins, kept under `target/`, downloaded now when no kept copy matches.
fn the_kept_download_of(workspace_root: &Path, download: &CarriedDownloadPin) -> Result<PathBuf> {
    let file_name = Path::new(&download.path_in_the_unit)
        .file_name()
        .with_context(|| format!("{} names no file", download.path_in_the_unit))?;
    let kept_directory = workspace_root
        .join(CARRIED_PACKAGE_DOWNLOADS_RELATIVE_TO_WORKSPACE)
        .join(&download.sha256);
    let kept_download = kept_directory.join(file_name);
    if sha256_hex_of_the_file(&kept_download).is_ok_and(|sha256| sha256 == download.sha256) {
        return Ok(kept_download);
    }
    std::fs::create_dir_all(&kept_directory)
        .with_context(|| format!("creating {}", kept_directory.display()))?;
    let partial_download = kept_directory.join(format!("{}.partial", file_name.to_string_lossy()));
    download_with_curl(&download.url, &partial_download)?;
    let downloaded_sha256 = sha256_hex_of_the_file(&partial_download)?;
    if downloaded_sha256 != download.sha256 {
        bail!(
            "{} does not match the sha256 it is pinned to: got {downloaded_sha256}, expected {}",
            download.url,
            download.sha256
        );
    }
    std::fs::rename(&partial_download, &kept_download).with_context(|| {
        format!(
            "renaming {} to {}",
            partial_download.display(),
            kept_download.display()
        )
    })?;
    Ok(kept_download)
}

/// Download `url` to `destination` with `curl`.
fn download_with_curl(url: &str, destination: &Path) -> Result<()> {
    tracing::info!("stage-runtime-unit-carried-packages: downloading {url}");
    let status = std::process::Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--retry",
            "4",
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .status()
        .context("running `curl` to download a package the runtime unit carries")?;
    if !status.success() {
        bail!("`curl` could not download {url}: it exited with {status}");
    }
    Ok(())
}

/// Resolve the core modules' third-party requirements and the scaffold's Python for every
/// platform the unit ships, with `uv`, download each to learn its sha256, and write the pin file.
pub fn pin_the_carried_packages(workspace_root: &Path, uv: &Path) -> Result<()> {
    let third_party_requirements =
        the_third_party_requirements_of_the_core_modules(workspace_root)?;
    let python_version_text =
        std::fs::read_to_string(workspace_root.join(SCAFFOLD_PYTHON_VERSION_RELATIVE_TO_WORKSPACE))
            .context("reading the scaffold's python-version")?;
    let python_minor_version = python_version_text.trim();
    let scratch = tempfile::tempdir().context("making a scratch directory")?;
    let requirements_file = scratch.path().join("requirements.in");
    std::fs::write(
        &requirements_file,
        third_party_requirements.join("\n") + "\n",
    )
    .context("writing the requirements to resolve")?;

    let mut wheels = Vec::new();
    for platform in RUNTIME_UNIT_PLATFORMS {
        let resolved =
            resolved_wheel_urls_for(uv, &requirements_file, python_minor_version, platform)?;
        for (name, version, url) in resolved {
            let download = pinned_download_learning_its_sha256(
                workspace_root,
                platform.target_triple,
                the_file_name_in(&url)?,
                &url,
            )?;
            wheels.push(CarriedWheelPin {
                name,
                version,
                download,
            });
        }
    }

    let (python_version, python_urls) = python_download_urls_for(uv, python_minor_version)?;
    let mut archives = Vec::new();
    for (platform, url) in python_urls {
        let path_in_the_unit = the_mirror_path_of(&url)?;
        archives.push(pinned_download_learning_its_sha256(
            workspace_root,
            platform.target_triple,
            path_in_the_unit,
            &url,
        )?);
    }

    let pins = RuntimeUnitCarriedPackagesPins {
        python: CarriedPythonPin {
            version: python_version,
            archives,
        },
        wheels,
    };
    let pin_file =
        workspace_root.join(RUNTIME_UNIT_CARRIED_PACKAGES_PIN_FILE_RELATIVE_TO_WORKSPACE);
    let pin_text = format!(
        "# Written by `cargo xtask pin-runtime-unit-carried-packages`: the third-party wheels the\n\
         # core modules require and the CPython build the runtime unit carries, per platform.\n\n{}",
        toml::to_string(&pins).context("writing the pins")?
    );
    std::fs::write(&pin_file, pin_text)
        .with_context(|| format!("writing {}", pin_file.display()))?;
    tracing::info!(
        "pin-runtime-unit-carried-packages: wrote {}",
        pin_file.display()
    );
    Ok(())
}

/// The requirements the core modules declare on packages that are not Tatolab's own, deduplicated.
fn the_third_party_requirements_of_the_core_modules(workspace_root: &Path) -> Result<Vec<String>> {
    let mut third_party_requirements = BTreeSet::new();
    for package in FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE {
        let pyproject_path = workspace_root.join(package).join("pyproject.toml");
        let pyproject: toml::Table = std::fs::read_to_string(&pyproject_path)
            .with_context(|| format!("reading {}", pyproject_path.display()))?
            .parse()
            .with_context(|| format!("parsing {}", pyproject_path.display()))?;
        let requirements = pyproject
            .get("project")
            .and_then(|project| project.get("dependencies"))
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for requirement in requirements.iter().filter_map(toml::Value::as_str) {
            if !requirement.trim_start().starts_with("tatolab-") {
                third_party_requirements.insert(requirement.to_owned());
            }
        }
    }
    Ok(third_party_requirements.into_iter().collect())
}

/// The `(name, version, wheel URL)` of each package `uv pip compile` resolves for `platform`,
/// one wheel each: the most specific one uv lists for the platform.
fn resolved_wheel_urls_for(
    uv: &Path,
    requirements_file: &Path,
    python_minor_version: &str,
    platform: RuntimeUnitPlatform,
) -> Result<Vec<(String, String, String)>> {
    let output = std::process::Command::new(uv)
        .args([
            "pip",
            "compile",
            "--format",
            "pylock.toml",
            "--no-header",
            "--quiet",
        ])
        .args(["--python-platform", platform.uv_python_platform])
        .args(["--python-version", python_minor_version])
        .arg(requirements_file)
        .env(
            "MACOSX_DEPLOYMENT_TARGET",
            MACOS_DEPLOYMENT_TARGET_OF_THE_CARRIED_WHEELS,
        )
        .output()
        .context("running `uv pip compile`")?;
    if !output.status.success() {
        bail!(
            "`uv pip compile` for {} failed: {}",
            platform.target_triple,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let pylock: toml::Table = String::from_utf8(output.stdout)
        .context("`uv pip compile` wrote no UTF-8")?
        .parse()
        .context("parsing `uv pip compile`'s pylock.toml")?;
    let mut resolved = Vec::new();
    for package in pylock
        .get("packages")
        .and_then(toml::Value::as_array)
        .context("the pylock.toml lists no packages")?
    {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .context("a package with no name")?;
        let version = package
            .get("version")
            .and_then(toml::Value::as_str)
            .with_context(|| format!("{name} has no version"))?;
        let wheel_urls: Vec<&str> = package
            .get("wheels")
            .and_then(toml::Value::as_array)
            .with_context(|| {
                format!(
                    "{name} {version} has no wheel for {}",
                    platform.target_triple
                )
            })?
            .iter()
            .filter_map(|wheel| wheel.get("url").and_then(toml::Value::as_str))
            .collect();
        let most_specific_wheel_url =
            the_most_specific_wheel_url(&wheel_urls).with_context(|| {
                format!(
                    "{name} {version} has no wheel for {}",
                    platform.target_triple
                )
            })?;
        resolved.push((
            name.to_owned(),
            version.to_owned(),
            most_specific_wheel_url.to_owned(),
        ));
    }
    Ok(resolved)
}

/// The wheel among `wheel_urls` whose macOS platform tag names the newest macOS, or the one
/// wheel when none names macOS.
fn the_most_specific_wheel_url<'url>(wheel_urls: &[&'url str]) -> Option<&'url str> {
    wheel_urls
        .iter()
        .copied()
        .max_by_key(|wheel_url| the_macos_version_a_wheel_url_names(wheel_url))
}

/// The `(major, minor)` of the `macosx_<major>_<minor>_` tag in `wheel_url`, `(0, 0)` with none.
fn the_macos_version_a_wheel_url_names(wheel_url: &str) -> (u32, u32) {
    let Some((_, after_the_tag)) = wheel_url.rsplit_once("macosx_") else {
        return (0, 0);
    };
    let mut numbers = after_the_tag
        .split('_')
        .map(|number| number.parse::<u32>().unwrap_or(0));
    (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0))
}

/// The full CPython version uv downloads for `python_minor_version`, and its archive URL on
/// python-build-standalone's GitHub releases for each platform the unit ships.
fn python_download_urls_for(
    uv: &Path,
    python_minor_version: &str,
) -> Result<(String, Vec<(RuntimeUnitPlatform, String)>)> {
    let output = std::process::Command::new(uv)
        .args([
            "python",
            "list",
            "--only-downloads",
            "--all-platforms",
            "--all-arches",
        ])
        .args(["--output-format", "json"])
        .arg(python_minor_version)
        .output()
        .context("running `uv python list`")?;
    if !output.status.success() {
        bail!(
            "`uv python list` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let downloads: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).context("parsing `uv python list`'s JSON")?;
    let newest_version = downloads
        .iter()
        .filter(|download| {
            download["implementation"] == "cpython" && download["variant"] == "default"
        })
        .filter_map(|download| download["version"].as_str())
        .max_by_key(|version| {
            version
                .split('.')
                .map(|number| number.parse::<u32>().unwrap_or(0))
                .collect::<Vec<_>>()
        })
        .with_context(|| format!("uv lists no CPython download for {python_minor_version}"))?
        .to_owned();
    let mut urls = Vec::new();
    for platform in RUNTIME_UNIT_PLATFORMS {
        let key = format!(
            "cpython-{newest_version}-{}",
            platform.uv_python_download_key_platform
        );
        let url = downloads
            .iter()
            .find(|download| download["key"] == key.as_str())
            .and_then(|download| download["url"].as_str())
            .with_context(|| format!("uv lists no download named {key}"))?;
        urls.push((
            platform,
            url.replacen(
                UV_PYTHON_DOWNLOAD_MIRROR_PREFIX,
                PYTHON_BUILD_STANDALONE_GITHUB_PREFIX,
                1,
            ),
        ));
    }
    Ok((newest_version, urls))
}

/// The path below a uv Python mirror that `url` is read from: `<build>/<archive file name>`,
/// the two segments after `releases/download/`, the file name unescaped.
fn the_mirror_path_of(url: &str) -> Result<String> {
    let (_, build_and_file) = url
        .split_once("/releases/download/")
        .with_context(|| format!("{url} is no release download"))?;
    Ok(build_and_file.replace("%2B", "+"))
}

/// The file name `url` ends in.
fn the_file_name_in(url: &str) -> Result<String> {
    url.rsplit('/')
        .next()
        .filter(|file_name| !file_name.is_empty())
        .map(|file_name| file_name.replace("%2B", "+"))
        .with_context(|| format!("{url} names no file"))
}

/// Download `url` once into the kept downloads to learn its sha256, and pin it.
fn pinned_download_learning_its_sha256(
    workspace_root: &Path,
    target_triple: &str,
    path_in_the_unit: String,
    url: &str,
) -> Result<CarriedDownloadPin> {
    let scratch = tempfile::tempdir().context("making a scratch directory")?;
    let downloaded = scratch.path().join("download");
    download_with_curl(url, &downloaded)?;
    let sha256 = sha256_hex_of_the_file(&downloaded)?;
    let pin = CarriedDownloadPin {
        target_triple: target_triple.to_owned(),
        path_in_the_unit,
        url: url.to_owned(),
        sha256,
    };
    let kept_directory = workspace_root
        .join(CARRIED_PACKAGE_DOWNLOADS_RELATIVE_TO_WORKSPACE)
        .join(&pin.sha256);
    std::fs::create_dir_all(&kept_directory)
        .with_context(|| format!("creating {}", kept_directory.display()))?;
    let file_name = Path::new(&pin.path_in_the_unit)
        .file_name()
        .with_context(|| format!("{} names no file", pin.path_in_the_unit))?;
    std::fs::copy(&downloaded, kept_directory.join(file_name))
        .with_context(|| format!("keeping {url}"))?;
    Ok(pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn the_pin_file_pins_python_and_every_third_party_requirement_for_every_platform() {
        let pins = read_the_carried_package_pins(&the_workspace_root()).unwrap();
        for platform in RUNTIME_UNIT_PLATFORMS {
            let pinned_names: BTreeSet<&str> = pins
                .wheels
                .iter()
                .filter(|wheel| wheel.download.target_triple == platform.target_triple)
                .map(|wheel| wheel.name.as_str())
                .collect();
            for requirement in
                the_third_party_requirements_of_the_core_modules(&the_workspace_root()).unwrap()
            {
                let name = requirement
                    .split(|character: char| {
                        !(character.is_alphanumeric() || character == '-' || character == '_')
                    })
                    .next()
                    .unwrap();
                assert!(
                    pinned_names.contains(name),
                    "{name} is required by a core module but not pinned for {}",
                    platform.target_triple
                );
            }
            let archives_for_the_platform = pins
                .python
                .archives
                .iter()
                .filter(|archive| archive.target_triple == platform.target_triple)
                .count();
            assert_eq!(archives_for_the_platform, 1, "{}", platform.target_triple);
        }
        let scaffold_python = std::fs::read_to_string(
            the_workspace_root().join(SCAFFOLD_PYTHON_VERSION_RELATIVE_TO_WORKSPACE),
        )
        .unwrap();
        assert!(
            pins.python
                .version
                .starts_with(&format!("{}.", scaffold_python.trim())),
            "the carried Python {} is not the scaffold's {}",
            pins.python.version,
            scaffold_python.trim()
        );
    }

    #[test]
    fn every_pin_names_a_lowercase_sha256_and_a_path_that_stays_in_its_directory() {
        let pins = read_the_carried_package_pins(&the_workspace_root()).unwrap();
        for download in pins
            .wheels
            .iter()
            .map(|wheel| &wheel.download)
            .chain(pins.python.archives.iter())
        {
            assert_eq!(download.sha256.len(), 64, "{download:?}");
            assert!(
                download
                    .sha256
                    .chars()
                    .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
            );
            assert!(!download.path_in_the_unit.starts_with('/'), "{download:?}");
            assert!(
                !download
                    .path_in_the_unit
                    .split('/')
                    .any(|segment| segment == ".."),
                "{download:?}"
            );
        }
    }

    #[test]
    fn the_newest_macos_wheel_is_the_most_specific_and_a_linux_wheel_is_the_only_one() {
        assert_eq!(
            the_most_specific_wheel_url(&[
                "https://x/numpy-2.5.4-cp312-cp312-macosx_11_0_arm64.whl",
                "https://x/numpy-2.5.4-cp312-cp312-macosx_14_0_arm64.whl",
            ]),
            Some("https://x/numpy-2.5.4-cp312-cp312-macosx_14_0_arm64.whl")
        );
        assert_eq!(
            the_most_specific_wheel_url(&[
                "https://x/numpy-2.5.4-cp312-cp312-manylinux_2_28_x86_64.whl"
            ]),
            Some("https://x/numpy-2.5.4-cp312-cp312-manylinux_2_28_x86_64.whl")
        );
    }

    #[test]
    fn a_kept_download_whose_sha256_matches_is_placed_at_its_path_without_downloading() {
        let workspace = tempfile::tempdir().unwrap();
        let file_contents = b"a carried wheel";
        let written = workspace.path().join("written");
        std::fs::write(&written, file_contents).unwrap();
        let actual_sha256 = sha256_hex_of_the_file(&written).unwrap();
        let kept_directory = workspace
            .path()
            .join(CARRIED_PACKAGE_DOWNLOADS_RELATIVE_TO_WORKSPACE)
            .join(&actual_sha256);
        std::fs::create_dir_all(&kept_directory).unwrap();
        std::fs::rename(
            &written,
            kept_directory.join("numpy-2.5.4-cp312-cp312-manylinux_2_28_x86_64.whl"),
        )
        .unwrap();
        let download = CarriedDownloadPin {
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            path_in_the_unit: "20261013/numpy-2.5.4-cp312-cp312-manylinux_2_28_x86_64.whl"
                .to_owned(),
            url: "https://unreachable.invalid/numpy.whl".to_owned(),
            sha256: actual_sha256,
        };
        let unit_directory = workspace.path().join("unit");
        std::fs::create_dir_all(unit_directory.join("left-by-an-earlier-build")).unwrap();

        place_the_pinned_downloads_in(workspace.path(), &[&download], &unit_directory).unwrap();

        assert_eq!(
            std::fs::read(unit_directory.join(&download.path_in_the_unit)).unwrap(),
            file_contents
        );
        assert!(!unit_directory.join("left-by-an-earlier-build").exists());
    }

    #[test]
    fn a_python_archive_url_maps_to_the_path_uv_reads_below_a_mirror() {
        assert_eq!(
            the_mirror_path_of(
                "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/\
                 cpython-3.12.15%2B20261013-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz"
            )
            .unwrap(),
            "20261013/cpython-3.12.15+20261013-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz"
        );
    }
}
