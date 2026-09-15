// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How the engine's build id is put together.
//!
//! `build.rs` compiles this file in through `#[path]` and the engine compiles
//! it only for its tests, so what the tests exercise is what the build script
//! runs. The standard library and `toml` only: a build script has no engine to
//! lean on.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The sha a build names when it has no git checkout to read one from — an
/// sdist, or a container where git refuses the tree.
pub(crate) const GIT_SHA_OF_A_BUILD_WITHOUT_A_CHECKOUT: &str = "unknown";

/// Variables that point git at a repository other than the one containing the
/// directory it runs in. Git exports them to hooks, so a build or test run from
/// one would otherwise read — and a test would commit into — that repository.
const GIT_REPOSITORY_OVERRIDE_ENVIRONMENT_VARIABLES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

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
    let mut rev_parse = git_command_resolving_the_checkout_of(directory);
    rev_parse.args(["rev-parse", "HEAD"]).stderr(Stdio::null());
    let output = rev_parse.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let git_sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    let is_a_full_object_name =
        matches!(git_sha.len(), 40 | 64) && git_sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    is_a_full_object_name.then_some(git_sha)
}

/// The directory of every crate the manifest in `crate_directory` links through
/// a path dependency, directly or through another path dependency — the code
/// that changes the compiled crate without touching its own sources.
///
/// Normal and target-specific dependencies only: dev- and build-dependencies
/// are not linked into the crate. A `workspace = true` entry resolves through
/// `workspace_manifest_directory`'s `[workspace.dependencies]`. A manifest that
/// cannot be read or parsed contributes nothing.
pub(crate) fn path_dependency_directories_linked_into(
    crate_directory: &Path,
    workspace_manifest_directory: &Path,
) -> BTreeSet<PathBuf> {
    let workspace_manifest = read_manifest(workspace_manifest_directory);
    let workspace_dependencies = workspace_manifest
        .as_ref()
        .and_then(|workspace_manifest| workspace_manifest.get("workspace")?.get("dependencies"))
        .and_then(toml::Value::as_table);

    let mut linked_directories = BTreeSet::new();
    let mut manifests_to_read = vec![crate_directory.to_path_buf()];
    while let Some(manifest_directory) = manifests_to_read.pop() {
        let Some(manifest) = read_manifest(&manifest_directory) else {
            continue;
        };
        for (dependency_name, dependency) in linked_dependency_entries(&manifest) {
            let dependency_directory =
                if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
                    manifest_directory.join(path)
                } else if dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                    let Some(path) = workspace_dependencies
                        .and_then(|dependencies| dependencies.get(dependency_name))
                        .and_then(|entry| entry.get("path"))
                        .and_then(toml::Value::as_str)
                    else {
                        continue;
                    };
                    workspace_manifest_directory.join(path)
                } else {
                    continue;
                };
            let Ok(dependency_directory) = dependency_directory.canonicalize() else {
                continue;
            };
            if linked_directories.insert(dependency_directory.clone()) {
                manifests_to_read.push(dependency_directory);
            }
        }
    }
    linked_directories
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

fn git_command_resolving_the_checkout_of(directory: &Path) -> Command {
    let mut git = Command::new("git");
    git.current_dir(directory).stdin(Stdio::null());
    for variable in GIT_REPOSITORY_OVERRIDE_ENVIRONMENT_VARIABLES {
        git.env_remove(variable);
    }
    git
}

fn read_manifest(manifest_directory: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(manifest_directory.join("Cargo.toml"))
        .ok()?
        .parse::<toml::Table>()
        .ok()
}

/// `(name, entry)` for every `[dependencies]` and
/// `[target.<cfg>.dependencies]` entry written as a table.
fn linked_dependency_entries(
    manifest: &toml::Table,
) -> impl Iterator<Item = (&str, &toml::Table)> + '_ {
    let target_dependency_tables = manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values())
        .filter_map(|target| target.get("dependencies"));
    manifest
        .get("dependencies")
        .into_iter()
        .chain(target_dependency_tables)
        .filter_map(toml::Value::as_table)
        .flat_map(|dependencies| dependencies.iter())
        .filter_map(|(name, entry)| Some((name.as_str(), entry.as_table()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git_or_panic(directory: &Path, arguments: &[&str]) -> String {
        let output = git_command_resolving_the_checkout_of(directory)
            .args(arguments)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn write_manifest(crate_directory: &Path, manifest: &str) {
        std::fs::create_dir_all(crate_directory).unwrap();
        std::fs::write(crate_directory.join("Cargo.toml"), manifest).unwrap();
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
    }

    #[test]
    fn a_checkout_names_the_commit_it_has_out() {
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
    }

    /// A crate edited only through a path dependency — or a dependency of
    /// one, or one the workspace names — still changes the engine, so its
    /// directory is one the build script watches.
    ///
    /// Fail-without-fix: read only the crate's own `[dependencies]` and the
    /// transitive and workspace-inherited crates go unwatched, so a helper
    /// built before an edit to them keeps passing the check.
    #[test]
    fn every_crate_linked_through_a_path_dependency_is_found_and_no_other() {
        let workspace = tempfile::tempdir().unwrap();
        let workspace_root = workspace.path();
        write_manifest(
            workspace_root,
            r#"
            [workspace.dependencies]
            vendored = { path = "vendor/vendored" }
            registry-only = "1"
            "#,
        );
        write_manifest(
            &workspace_root.join("runtime/engine"),
            r#"
            [dependencies]
            direct = { path = "../direct" }
            vendored.workspace = true
            registry-only.workspace = true
            serde = "1"

            [target.'cfg(target_os = "linux")'.dependencies]
            linux-only = { path = "../linux-only" }

            [dev-dependencies]
            test-only = { path = "../test-only" }

            [build-dependencies]
            build-only = { path = "../build-only" }
            "#,
        );
        write_manifest(
            &workspace_root.join("runtime/direct"),
            r#"
            [dependencies]
            transitive = { path = "../../sdk/transitive" }
            "#,
        );
        for leaf in [
            "vendor/vendored",
            "runtime/linux-only",
            "runtime/test-only",
            "runtime/build-only",
            "sdk/transitive",
        ] {
            write_manifest(&workspace_root.join(leaf), "[package]\nname = \"leaf\"\n");
        }

        let found = path_dependency_directories_linked_into(
            &workspace_root.join("runtime/engine"),
            workspace_root,
        );

        let expected: BTreeSet<PathBuf> = [
            "runtime/direct",
            "sdk/transitive",
            "vendor/vendored",
            "runtime/linux-only",
        ]
        .into_iter()
        .map(|linked| workspace_root.join(linked).canonicalize().unwrap())
        .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn the_engines_own_manifest_reaches_its_first_party_and_vendored_crates() {
        let engine_directory = Path::new(env!("CARGO_MANIFEST_DIR"));

        let found = path_dependency_directories_linked_into(
            engine_directory,
            &engine_directory.join("../.."),
        );

        for linked in [
            "../streamlib-ipc-types",
            "../streamlib-surface-client",
            "../../sdk/streamlib-error",
            "../../vendor/tatolab-vulkanalia",
        ] {
            let linked = engine_directory.join(linked).canonicalize().unwrap();
            assert!(found.contains(&linked), "{linked:?} missing from {found:?}");
        }
        assert!(
            !found
                .iter()
                .any(|directory| directory.ends_with("streamlib-api-server")),
            "a dev-dependency is not linked into the engine: {found:?}"
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
