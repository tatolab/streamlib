// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How the engine's build id is put together.
//!
//! `build.rs` compiles this file in through `#[path]` and the engine compiles
//! it only for its tests, so what the tests exercise is what the build script
//! runs. Standard library only: a build script has no engine to lean on.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The sha a build names when it has no git checkout to read one from — an
/// sdist, or a container where git refuses the tree.
pub(crate) const GIT_SHA_OF_A_BUILD_WITHOUT_A_CHECKOUT: &str = "unknown";

/// `<crate version>+<git sha>.<per-build nonce>`.
pub(crate) fn compose_engine_build_id(
    crate_version: &str,
    git_sha: Option<&str>,
    per_build_nonce: &str,
) -> String {
    let git_sha = git_sha.unwrap_or(GIT_SHA_OF_A_BUILD_WITHOUT_A_CHECKOUT);
    format!("{crate_version}+{git_sha}.{per_build_nonce}")
}

/// The commit checked out in the git checkout containing `directory`, or `None`
/// where there is no checkout or git cannot say.
pub(crate) fn git_sha_of_the_checkout_containing(directory: &Path) -> Option<String> {
    let git_sha = run_git_in(directory, &["rev-parse", "HEAD"])?;
    let is_a_full_object_name =
        matches!(git_sha.len(), 40 | 64) && git_sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    is_a_full_object_name.then_some(git_sha)
}

/// The files git rewrites whenever the checkout containing `directory` moves
/// to another commit — a checkout, a commit, a reset — that exist.
///
/// Per worktree: a linked worktree's `HEAD` and reflog live in its own git
/// directory, not the shared one.
pub(crate) fn git_files_rewritten_when_the_checked_out_commit_changes(
    directory: &Path,
) -> Vec<PathBuf> {
    let Some(git_directory) = run_git_in(directory, &["rev-parse", "--absolute-git-dir"]) else {
        return Vec::new();
    };
    let git_directory = PathBuf::from(git_directory);
    [git_directory.join("HEAD"), git_directory.join("logs/HEAD")]
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

/// 128 bits from the operating system's random source, as 32 lowercase hex
/// digits.
pub(crate) fn mint_per_build_nonce() -> std::io::Result<String> {
    let mut nonce_bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce_bytes)?;
    Ok(nonce_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn run_git_in(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git_or_panic(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn the_id_joins_the_crate_version_the_git_sha_and_the_nonce() {
        let git_sha = "9f1c2ab4e0d7a3b6c5e8f9012345678901234567";
        assert_eq!(
            compose_engine_build_id("0.9.3", Some(git_sha), "00112233445566778899aabbccddeeff"),
            format!("0.9.3+{git_sha}.00112233445566778899aabbccddeeff"),
        );
    }

    #[test]
    fn a_build_with_no_git_checkout_names_its_sha_unknown() {
        let directory_outside_any_checkout = tempfile::tempdir().unwrap();

        let git_sha = git_sha_of_the_checkout_containing(directory_outside_any_checkout.path());

        assert_eq!(git_sha, None);
        assert_eq!(
            compose_engine_build_id("0.9.3", git_sha.as_deref(), "nonce"),
            "0.9.3+unknown.nonce"
        );
        assert!(
            git_files_rewritten_when_the_checked_out_commit_changes(
                directory_outside_any_checkout.path()
            )
            .is_empty()
        );
    }

    #[test]
    fn a_checkout_names_the_commit_it_has_out_and_the_head_that_moves_with_it() {
        let checkout = tempfile::tempdir().unwrap();
        run_git_or_panic(checkout.path(), &["init", "--quiet"]);
        run_git_or_panic(
            checkout.path(),
            &[
                "-c",
                "user.name=streamlib",
                "-c",
                "user.email=streamlib@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--allow-empty",
                "--message=the one commit",
            ],
        );
        let nested_directory = checkout.path().join("runtime/streamlib-engine");
        std::fs::create_dir_all(&nested_directory).unwrap();

        assert_eq!(
            git_sha_of_the_checkout_containing(&nested_directory),
            Some(run_git_or_panic(checkout.path(), &["rev-parse", "HEAD"])),
        );
        let watched_files =
            git_files_rewritten_when_the_checked_out_commit_changes(&nested_directory);
        assert!(
            watched_files
                .iter()
                .any(|path| path.ends_with(".git/HEAD") && path.is_absolute()),
            "{watched_files:?}"
        );
    }

    #[test]
    fn two_nonces_never_match_and_each_is_32_hex_digits() {
        let first = mint_per_build_nonce().unwrap();
        let second = mint_per_build_nonce().unwrap();

        assert_ne!(first, second);
        for nonce in [&first, &second] {
            assert_eq!(nonce.len(), 32, "{nonce}");
            assert!(
                nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{nonce}"
            );
        }
    }
}
