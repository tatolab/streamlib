// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! How the engine's build id is put together.
//!
//! `build.rs` and xtask compile this file in through `#[path]` and the engine
//! compiles it only for its tests, so what the tests exercise is what the build
//! script and `cargo xtask build-runtime` run. The standard library and `toml`
//! only, and its tests live beside it: neither a build script nor xtask has an
//! engine to lean on.

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

/// The variable through which one build of the runtime unit hands both of its
/// engine compiles — the lend's `_engine` and `tatolabd` — the same nonce, so
/// the unit's two halves carry one build id. `cargo xtask build-runtime` sets
/// it; a build without it mints its own.
pub(crate) const RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_RUNTIME_UNIT_ENGINE_BUILD_NONCE";

/// The nonce a runtime unit's build handed this compile, `None` when it handed
/// none, or a refusal naming a value that is not 32 lowercase hex digits.
pub(crate) fn runtime_unit_build_nonce_from(
    handed_nonce: Option<&str>,
) -> Result<Option<String>, String> {
    match handed_nonce {
        None | Some("") => Ok(None),
        Some(nonce) if is_a_per_build_nonce(nonce) => Ok(Some(nonce.to_owned())),
        Some(nonce) => Err(format!(
            "{RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE}={nonce:?} is not 32 lowercase \
             hex digits"
        )),
    }
}

fn is_a_per_build_nonce(candidate: &str) -> bool {
    candidate.len() == 32
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

pub(crate) fn git_command_resolving_the_checkout_of(directory: &Path) -> Command {
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
