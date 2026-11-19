// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The two programs the runtime checks a project's views and controls with,
//! so it needs no Node on the machine: the native `tsc` of TypeScript 7, from
//! its platform package on npm, with the library files it reads from beside its
//! own real path, and the standalone `oxlint` from oxc's release. Each is one
//! archive for the host's target at one pinned version, checked against its
//! pinned sha256 and kept under `target/`, so a second run whose archive still
//! matches downloads nothing. `build-runtime` places `oxlint` in the unit's
//! `bin/` beside `tatolabd`, and `tsc` with its library files in
//! `bin/typescript/`.

use crate::stage_runtime_unit_carried_packages::the_kept_download_pinned_by;
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The TypeScript version whose compiler the runtime unit carries: the one
/// Tato Studio type-checks its own code with.
pub const PINNED_RUNTIME_UNIT_TYPESCRIPT_VERSION: &str = "7.0.2";

/// The oxlint version the runtime unit carries: the one Tato Studio lints its
/// own code with.
pub const PINNED_RUNTIME_UNIT_OXLINT_VERSION: &str = "1.91.0";

/// Where the unpacked programs are kept, relative to the workspace.
const RUNTIME_UNIT_CODE_CHECKERS_STAGING_DIRECTORY_RELATIVE_TO_WORKSPACE: &str =
    "target/runtime-unit-code-checkers";

/// The folder of TypeScript's platform package that holds `tsc` and every
/// library file it reads, inside the package's npm archive.
const TYPESCRIPT_PACKAGE_COMPILER_DIRECTORY_IN_THE_ARCHIVE: &str = "package/lib";

/// The compiler's file name in its folder.
const TYPESCRIPT_COMPILER_FILE_NAME: &str = "tsc";

/// One target's archives of the two programs and the sha256 each is checked
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedCodeCheckerArchives {
    /// The Rust target triple oxlint names its archive after.
    pub target_triple: &'static str,
    /// The npm name of TypeScript's platform package for the target.
    pub typescript_platform_package: &'static str,
    /// The sha256 of that package's npm archive, which matches the
    /// `integrity` the npm registry states for it.
    pub typescript_archive_sha256_hex: &'static str,
    /// The sha256 of oxlint's release archive for the target.
    pub oxlint_archive_sha256_hex: &'static str,
}

/// Linux's archives. oxlint's newest glibc symbol is `GLIBC_2.18` and `tsc` is
/// linked statically, both under the runtime unit's 2.28 floor.
pub const PINNED_CODE_CHECKER_ARCHIVES_FOR_LINUX_X86_64: PinnedCodeCheckerArchives =
    PinnedCodeCheckerArchives {
        target_triple: "x86_64-unknown-linux-gnu",
        typescript_platform_package: "typescript-linux-x64",
        typescript_archive_sha256_hex: "7ecad6f67377e831856367ab062ef394f21506a611405bf8ac0ff039348637d3",
        oxlint_archive_sha256_hex: "de059941526abeaab3c5aafa78caef80e5b4e7197274391d80cde38a1960a916",
    };

/// macOS's archives, for Apple silicon.
pub const PINNED_CODE_CHECKER_ARCHIVES_FOR_MACOS_AARCH64: PinnedCodeCheckerArchives =
    PinnedCodeCheckerArchives {
        target_triple: "aarch64-apple-darwin",
        typescript_platform_package: "typescript-darwin-arm64",
        typescript_archive_sha256_hex: "902e2fe1cf0799198ef902c6b8c310a450fef629a6baba41d45641ef75c04ebd",
        oxlint_archive_sha256_hex: "d1f6037780a4d5e14a32d810083859ae691a5032f789381a5ff9b75f901f90e9",
    };

impl PinnedCodeCheckerArchives {
    /// The npm archive's file name, as the registry publishes it.
    pub fn typescript_archive_file_name(&self) -> String {
        format!(
            "{}-{PINNED_RUNTIME_UNIT_TYPESCRIPT_VERSION}.tgz",
            self.typescript_platform_package
        )
    }

    /// The npm archive's download URL on the npm registry.
    pub fn typescript_archive_url(&self) -> String {
        format!(
            "https://registry.npmjs.org/@typescript/{}/-/{}",
            self.typescript_platform_package,
            self.typescript_archive_file_name()
        )
    }

    /// oxlint's archive file name, as its release publishes it.
    pub fn oxlint_archive_file_name(&self) -> String {
        format!("oxlint-{}.tar.gz", self.target_triple)
    }

    /// oxlint's archive download URL on oxc's GitHub release.
    pub fn oxlint_archive_url(&self) -> String {
        format!(
            "https://github.com/oxc-project/oxc/releases/download/oxlint_v{PINNED_RUNTIME_UNIT_OXLINT_VERSION}/{}",
            self.oxlint_archive_file_name()
        )
    }

    /// The `oxlint` executable's path inside its archive.
    pub fn oxlint_executable_path_in_the_archive(&self) -> String {
        format!("oxlint-{}", self.target_triple)
    }
}

/// The archives for the target this xtask runs on, which is the target
/// `build-runtime` builds the runtime unit for.
pub fn pinned_code_checker_archives_for_this_host() -> Result<PinnedCodeCheckerArchives> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(PINNED_CODE_CHECKER_ARCHIVES_FOR_LINUX_X86_64)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(PINNED_CODE_CHECKER_ARCHIVES_FOR_MACOS_AARCH64)
    } else {
        bail!(
            "the runtime unit carries tsc and oxlint for x86_64 Linux and Apple-silicon macOS \
             only; this host is neither"
        )
    }
}

/// The two programs, as `build-runtime` places them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedCodeCheckers {
    /// The directory holding `tsc` and every library file it reads.
    pub typescript_compiler_directory: PathBuf,
    /// The `oxlint` executable.
    pub linter: PathBuf,
}

/// Download the host's pinned archives unless the kept ones already match
/// their sha256, unpack both programs, and return where they are.
pub fn stage_the_runtime_unit_code_checkers(workspace_root: &Path) -> Result<StagedCodeCheckers> {
    let pinned_archives = pinned_code_checker_archives_for_this_host()?;
    let staging_directory = workspace_root
        .join(RUNTIME_UNIT_CODE_CHECKERS_STAGING_DIRECTORY_RELATIVE_TO_WORKSPACE)
        .join(pinned_archives.target_triple);
    let typescript_archive = the_kept_download_pinned_by(
        workspace_root,
        &pinned_archives.typescript_archive_url(),
        pinned_archives.typescript_archive_sha256_hex,
        OsStr::new(&pinned_archives.typescript_archive_file_name()),
    )?;
    let oxlint_archive = the_kept_download_pinned_by(
        workspace_root,
        &pinned_archives.oxlint_archive_url(),
        pinned_archives.oxlint_archive_sha256_hex,
        OsStr::new(&pinned_archives.oxlint_archive_file_name()),
    )?;
    let staged = StagedCodeCheckers {
        typescript_compiler_directory: unpack_the_typescript_compiler(
            &typescript_archive,
            &staging_directory.join(format!(
                "typescript-{PINNED_RUNTIME_UNIT_TYPESCRIPT_VERSION}"
            )),
        )?,
        linter: unpack_the_linter(
            &oxlint_archive,
            &pinned_archives.oxlint_executable_path_in_the_archive(),
            &staging_directory.join(format!("oxlint-{PINNED_RUNTIME_UNIT_OXLINT_VERSION}")),
        )?,
    };
    tracing::info!(
        "stage-runtime-unit-code-checkers: tsc {PINNED_RUNTIME_UNIT_TYPESCRIPT_VERSION} is in {} \
         and oxlint {PINNED_RUNTIME_UNIT_OXLINT_VERSION} is at {}",
        staged.typescript_compiler_directory.display(),
        staged.linter.display()
    );
    Ok(staged)
}

/// Empty `directory`, then unpack `members` of `archive` into it with `tar`.
fn unpack_into_an_empty_directory(
    archive: &Path,
    directory: &Path,
    members: &[&str],
) -> Result<()> {
    if directory.exists() {
        std::fs::remove_dir_all(directory)
            .with_context(|| format!("removing {}", directory.display()))?;
    }
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .args(members)
        .status()
        .context("running `tar` to unpack a program the runtime unit checks code with")?;
    if !status.success() {
        bail!(
            "`tar` could not unpack {} from {}: it exited with {status}",
            members.join(", "),
            archive.display()
        );
    }
    Ok(())
}

/// Unpack the compiler's folder of TypeScript's platform package from
/// `archive` into `unpacked_directory` and return the folder, which holds
/// `tsc` and its library files side by side.
pub fn unpack_the_typescript_compiler(
    archive: &Path,
    unpacked_directory: &Path,
) -> Result<PathBuf> {
    unpack_into_an_empty_directory(
        archive,
        unpacked_directory,
        &[TYPESCRIPT_PACKAGE_COMPILER_DIRECTORY_IN_THE_ARCHIVE],
    )?;
    let compiler_directory =
        unpacked_directory.join(TYPESCRIPT_PACKAGE_COMPILER_DIRECTORY_IN_THE_ARCHIVE);
    if !compiler_directory
        .join(TYPESCRIPT_COMPILER_FILE_NAME)
        .is_file()
    {
        bail!(
            "{} holds no {TYPESCRIPT_PACKAGE_COMPILER_DIRECTORY_IN_THE_ARCHIVE}/{TYPESCRIPT_COMPILER_FILE_NAME}",
            archive.display()
        );
    }
    if !compiler_directory.join("lib.d.ts").is_file() {
        bail!(
            "{} holds no library files beside {TYPESCRIPT_COMPILER_FILE_NAME}, which stops \
             without them",
            archive.display()
        );
    }
    Ok(compiler_directory)
}

/// Unpack `executable_path_in_the_archive` from oxlint's `archive` into
/// `unpacked_directory` and return it.
pub fn unpack_the_linter(
    archive: &Path,
    executable_path_in_the_archive: &str,
    unpacked_directory: &Path,
) -> Result<PathBuf> {
    unpack_into_an_empty_directory(
        archive,
        unpacked_directory,
        &[executable_path_in_the_archive],
    )?;
    let linter = unpacked_directory.join(executable_path_in_the_archive);
    if !linter.is_file() {
        bail!(
            "{} holds no {executable_path_in_the_archive}",
            archive.display()
        );
    }
    Ok(linter)
}

#[cfg(test)]
mod tests;
