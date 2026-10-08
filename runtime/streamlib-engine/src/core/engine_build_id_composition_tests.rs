// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::engine_build_id_composition::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

fn run_git_or_panic(directory: &Path, arguments: &[&str]) -> String {
    let output = crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind(
        git_command_resolving_the_checkout_of(directory)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .and_then(std::process::Child::wait_with_output)
    .expect("git runs");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn write_manifest(crate_directory: &Path, manifest: &str) {
    crate::core::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
        crate_directory,
        crate::core::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
    )
    .unwrap();
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
    let directory_outside_any_checkout =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();

    let git_sha = git_sha_of_the_checkout_containing(directory_outside_any_checkout.path());

    assert_eq!(git_sha, None);
    assert_eq!(
        compose_engine_build_id("0.9.3", git_sha.as_deref(), "nonce"),
        "0.9.3+unknown.nonce"
    );
}

#[test]
fn a_checkout_names_the_commit_it_has_out() {
    let checkout = crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
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
    crate::core::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
        &nested_directory,
        crate::core::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
    )
    .unwrap();

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
    let workspace = crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
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

    let found =
        path_dependency_directories_linked_into(engine_directory, &engine_directory.join("../.."));

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
fn a_runtime_unit_nonce_is_taken_as_handed_and_a_malformed_one_is_refused() {
    let handed_nonce = mint_per_build_nonce().unwrap();
    assert_eq!(
        runtime_unit_build_nonce_from(Some(&handed_nonce)),
        Ok(Some(handed_nonce.clone()))
    );
    assert_eq!(runtime_unit_build_nonce_from(None), Ok(None));
    assert_eq!(runtime_unit_build_nonce_from(Some("")), Ok(None));
    for malformed_nonce in [
        "abc",
        &handed_nonce.to_uppercase(),
        &format!("{handed_nonce}0"),
        &"g".repeat(32),
    ] {
        let refusal = runtime_unit_build_nonce_from(Some(malformed_nonce)).unwrap_err();
        assert!(
            refusal.contains(RUNTIME_UNIT_ENGINE_BUILD_NONCE_ENVIRONMENT_VARIABLE),
            "{refusal}"
        );
    }
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
