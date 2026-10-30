// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The uv the runtime unit carries, which the runtime makes every project's
//! `.venv` with: uv's own release archive for the host's target, at one pinned
//! version, checked against its pinned sha256.
//!
//! The archive is kept under `target/`, and a second run whose archive still
//! matches downloads nothing. `build-runtime` copies the `uv` executable into
//! the runtime unit's `bin/` beside `tatolabd`.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The one uv version the runtime unit carries.
pub const PINNED_RUNTIME_UNIT_UV_VERSION: &str = "0.13.1";

/// Where the archives and their unpacked `uv` are kept, relative to the
/// workspace.
const RUNTIME_UNIT_UV_STAGING_DIRECTORY_RELATIVE_TO_WORKSPACE: &str = "target/runtime-unit-uv";

/// One target's uv release archive and the sha256 it is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedUvReleaseArchive {
    /// The Rust target triple uv names its archive after.
    pub target_triple: &'static str,
    /// The archive's sha256, as uv publishes it beside the archive.
    pub archive_sha256_hex: &'static str,
}

/// Linux's archive: the gnu build, whose newest glibc symbol is `GLIBC_2.17`,
/// under the runtime unit's 2.28 floor.
pub const PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64: PinnedUvReleaseArchive =
    PinnedUvReleaseArchive {
        target_triple: "x86_64-unknown-linux-gnu",
        archive_sha256_hex: "eadd9d27c134ab5ac1ffd4473b97a741407f3ea238288f03121fc4deddf90220",
    };

/// macOS's archive: the Apple-silicon build, signed, with a `minos` of 11.0.
pub const PINNED_UV_RELEASE_ARCHIVE_FOR_MACOS_AARCH64: PinnedUvReleaseArchive =
    PinnedUvReleaseArchive {
        target_triple: "aarch64-apple-darwin",
        archive_sha256_hex: "cfe454573412ff7380d15fc7dae5997ab53ffce72fce0a78e91ddbc84001acd2",
    };

impl PinnedUvReleaseArchive {
    /// The archive's file name, as uv's release publishes it.
    pub fn archive_file_name(&self) -> String {
        format!("uv-{}.tar.gz", self.target_triple)
    }

    /// The archive's download URL on uv's GitHub release.
    pub fn archive_url(&self) -> String {
        format!(
            "https://github.com/astral-sh/uv/releases/download/{PINNED_RUNTIME_UNIT_UV_VERSION}/{}",
            self.archive_file_name()
        )
    }

    /// The `uv` executable's path inside the archive.
    pub fn uv_executable_path_in_the_archive(&self) -> String {
        format!("uv-{}/uv", self.target_triple)
    }
}

/// The archive for the target this xtask runs on, which is the target
/// `build-runtime` builds the runtime unit for.
pub fn pinned_uv_release_archive_for_this_host() -> Result<PinnedUvReleaseArchive> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(PINNED_UV_RELEASE_ARCHIVE_FOR_MACOS_AARCH64)
    } else {
        bail!(
            "the runtime unit carries uv for x86_64 Linux and Apple-silicon macOS only; this host \
             is neither"
        )
    }
}

/// The directory the pinned version's archives and their unpacked `uv` sit in.
pub fn runtime_unit_uv_staging_directory_in_the_workspace(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(RUNTIME_UNIT_UV_STAGING_DIRECTORY_RELATIVE_TO_WORKSPACE)
        .join(PINNED_RUNTIME_UNIT_UV_VERSION)
}

/// Download the host's pinned uv archive unless the one kept already matches
/// its sha256, unpack it, and return the `uv` executable.
pub fn stage_the_runtime_unit_uv(workspace_root: &Path) -> Result<PathBuf> {
    let pinned_archive = pinned_uv_release_archive_for_this_host()?;
    let staging_directory = runtime_unit_uv_staging_directory_in_the_workspace(workspace_root);
    std::fs::create_dir_all(&staging_directory)
        .with_context(|| format!("creating {}", staging_directory.display()))?;
    let archive = staging_directory.join(pinned_archive.archive_file_name());
    if !the_archive_matches_its_pin(&archive, &pinned_archive) {
        download_the_pinned_archive(&pinned_archive, &archive)?;
    }
    unpack_the_uv_executable(&archive, &pinned_archive, &staging_directory)
}

/// Download `pinned_archive` to `archive` through a partial file, which is
/// renamed into place only once its sha256 matches the pin.
fn download_the_pinned_archive(
    pinned_archive: &PinnedUvReleaseArchive,
    archive: &Path,
) -> Result<()> {
    let partial_archive = archive.with_extension("gz.partial");
    let url = pinned_archive.archive_url();
    tracing::info!("stage-runtime-unit-uv: downloading {url}");
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
        .arg(&partial_archive)
        .arg(&url)
        .status()
        .context("running `curl` to download the runtime unit's uv")?;
    if !status.success() {
        bail!("`curl` could not download {url}: it exited with {status}");
    }
    refuse_an_archive_whose_sha256_is_not_pinned(&partial_archive, pinned_archive)?;
    std::fs::rename(&partial_archive, archive).with_context(|| {
        format!(
            "renaming {} to {}",
            partial_archive.display(),
            archive.display()
        )
    })
}

/// Whether `archive` is there and its sha256 is the one `pinned_archive` pins.
fn the_archive_matches_its_pin(archive: &Path, pinned_archive: &PinnedUvReleaseArchive) -> bool {
    sha256_hex_of_the_file(archive)
        .is_ok_and(|actual_sha256_hex| actual_sha256_hex == pinned_archive.archive_sha256_hex)
}

/// Refuse `archive` unless its sha256 is the one `pinned_archive` pins,
/// naming both.
pub fn refuse_an_archive_whose_sha256_is_not_pinned(
    archive: &Path,
    pinned_archive: &PinnedUvReleaseArchive,
) -> Result<()> {
    let actual_sha256_hex = sha256_hex_of_the_file(archive)?;
    if actual_sha256_hex != pinned_archive.archive_sha256_hex {
        bail!(
            "{} does not match the sha256 uv {PINNED_RUNTIME_UNIT_UV_VERSION} is pinned to: got \
             {actual_sha256_hex}, expected {}",
            archive.display(),
            pinned_archive.archive_sha256_hex
        );
    }
    Ok(())
}

/// The sha256 of the file at `path` as lowercase hex, as uv publishes it,
/// read through without holding the file in memory.
fn sha256_hex_of_the_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("reading {}", path.display()))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Unpack the `uv` executable from `archive` into `staging_directory`,
/// replacing what an earlier run left, and return it.
fn unpack_the_uv_executable(
    archive: &Path,
    pinned_archive: &PinnedUvReleaseArchive,
    staging_directory: &Path,
) -> Result<PathBuf> {
    let uv_path_in_the_archive = pinned_archive.uv_executable_path_in_the_archive();
    let unpacked_uv = staging_directory.join(&uv_path_in_the_archive);
    if unpacked_uv.symlink_metadata().is_ok() {
        std::fs::remove_file(&unpacked_uv)
            .with_context(|| format!("removing {}", unpacked_uv.display()))?;
    }
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(staging_directory)
        .arg(&uv_path_in_the_archive)
        .status()
        .context("running `tar` to unpack the runtime unit's uv")?;
    if !status.success() {
        bail!(
            "`tar` could not unpack {uv_path_in_the_archive} from {}: it exited with {status}",
            archive.display()
        );
    }
    if !unpacked_uv.is_file() {
        bail!("{} holds no {uv_path_in_the_archive}", archive.display());
    }
    tracing::info!(
        "stage-runtime-unit-uv: uv {PINNED_RUNTIME_UNIT_UV_VERSION} is at {}",
        unpacked_uv.display()
    );
    Ok(unpacked_uv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_sha256_is_its_known_answer_in_lowercase_hex() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("abc");
        std::fs::write(&file, b"abc").unwrap();

        assert_eq!(
            sha256_hex_of_the_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_missing_or_unpinned_archive_does_not_match_and_is_downloaded_again() {
        let scratch = tempfile::tempdir().unwrap();
        let archive = scratch.path().join("uv-x86_64-unknown-linux-gnu.tar.gz");
        assert!(!the_archive_matches_its_pin(
            &archive,
            &PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64
        ));
        std::fs::write(&archive, b"a truncated download").unwrap();
        assert!(!the_archive_matches_its_pin(
            &archive,
            &PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64
        ));
    }

    #[test]
    fn each_archive_is_downloaded_from_uvs_release_of_the_pinned_version() {
        assert_eq!(
            PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64.archive_url(),
            "https://github.com/astral-sh/uv/releases/download/0.13.1/uv-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            PINNED_UV_RELEASE_ARCHIVE_FOR_MACOS_AARCH64.archive_url(),
            "https://github.com/astral-sh/uv/releases/download/0.13.1/uv-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn every_pin_is_a_whole_lowercase_sha256() {
        for pinned_archive in [
            PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64,
            PINNED_UV_RELEASE_ARCHIVE_FOR_MACOS_AARCH64,
        ] {
            assert_eq!(pinned_archive.archive_sha256_hex.len(), 64);
            assert!(
                pinned_archive
                    .archive_sha256_hex
                    .chars()
                    .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
            );
        }
    }

    #[test]
    fn an_archive_whose_sha256_is_not_the_pin_is_refused_naming_both() {
        let scratch = tempfile::tempdir().unwrap();
        let archive = scratch.path().join("uv-x86_64-unknown-linux-gnu.tar.gz");
        std::fs::write(&archive, b"not uv").unwrap();

        let refusal = refuse_an_archive_whose_sha256_is_not_pinned(
            &archive,
            &PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64,
        )
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains(PINNED_UV_RELEASE_ARCHIVE_FOR_LINUX_X86_64.archive_sha256_hex),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!(
                "got {}",
                sha256_hex_of_the_file(&archive).unwrap()
            )),
            "{refusal}"
        );
    }

    #[test]
    fn an_archive_whose_sha256_is_the_pin_is_taken() {
        let scratch = tempfile::tempdir().unwrap();
        let archive = scratch.path().join("archive.tar.gz");
        std::fs::write(&archive, b"pinned bytes").unwrap();
        let pinned_bytes_sha256_hex = sha256_hex_of_the_file(&archive).unwrap();
        let pinned_archive = PinnedUvReleaseArchive {
            target_triple: "x86_64-unknown-linux-gnu",
            archive_sha256_hex: Box::leak(pinned_bytes_sha256_hex.into_boxed_str()),
        };

        refuse_an_archive_whose_sha256_is_not_pinned(&archive, &pinned_archive).unwrap();
    }

    #[test]
    fn the_uv_executable_is_unpacked_from_the_targets_own_directory_in_the_archive() {
        assert_eq!(
            PINNED_UV_RELEASE_ARCHIVE_FOR_MACOS_AARCH64.uv_executable_path_in_the_archive(),
            "uv-aarch64-apple-darwin/uv"
        );
    }
}
