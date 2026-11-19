// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A gzipped tar archive at `archive` holding `files`, by their path in it.
fn archive_holding(archive: &Path, files: &[(&str, &str)]) {
    let contents = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let file = contents.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(contents.path())
        .args(files.iter().map(|(path, _)| *path))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn each_typescript_archive_is_its_platform_package_on_npm_at_the_pinned_version() {
    assert_eq!(
        PINNED_CODE_CHECKER_ARCHIVES_FOR_LINUX_X86_64.typescript_archive_url(),
        "https://registry.npmjs.org/@typescript/typescript-linux-x64/-/typescript-linux-x64-7.0.2.tgz"
    );
    assert_eq!(
        PINNED_CODE_CHECKER_ARCHIVES_FOR_MACOS_AARCH64.typescript_archive_url(),
        "https://registry.npmjs.org/@typescript/typescript-darwin-arm64/-/typescript-darwin-arm64-7.0.2.tgz"
    );
}

#[test]
fn each_oxlint_archive_is_oxcs_release_of_the_pinned_version() {
    assert_eq!(
        PINNED_CODE_CHECKER_ARCHIVES_FOR_LINUX_X86_64.oxlint_archive_url(),
        "https://github.com/oxc-project/oxc/releases/download/oxlint_v1.91.0/oxlint-x86_64-unknown-linux-gnu.tar.gz"
    );
    assert_eq!(
        PINNED_CODE_CHECKER_ARCHIVES_FOR_MACOS_AARCH64.oxlint_archive_url(),
        "https://github.com/oxc-project/oxc/releases/download/oxlint_v1.91.0/oxlint-aarch64-apple-darwin.tar.gz"
    );
    assert_eq!(
        PINNED_CODE_CHECKER_ARCHIVES_FOR_MACOS_AARCH64.oxlint_executable_path_in_the_archive(),
        "oxlint-aarch64-apple-darwin"
    );
}

#[test]
fn every_pin_is_a_whole_lowercase_sha256() {
    for pinned_archives in [
        PINNED_CODE_CHECKER_ARCHIVES_FOR_LINUX_X86_64,
        PINNED_CODE_CHECKER_ARCHIVES_FOR_MACOS_AARCH64,
    ] {
        for pin in [
            pinned_archives.typescript_archive_sha256_hex,
            pinned_archives.oxlint_archive_sha256_hex,
        ] {
            assert_eq!(pin.len(), 64, "{pin}");
            assert!(
                pin.chars()
                    .all(|character| matches!(character, '0'..='9' | 'a'..='f')),
                "{pin}"
            );
        }
    }
}

#[test]
fn the_compiler_is_unpacked_with_its_library_files_beside_it_and_nothing_else_of_its_package() {
    let scratch = tempfile::tempdir().unwrap();
    let archive = scratch.path().join("typescript-linux-x64-7.0.2.tgz");
    archive_holding(
        &archive,
        &[
            ("package/package.json", "{}"),
            ("package/lib/tsc", "the compiler"),
            ("package/lib/lib.d.ts", "/// lib"),
            ("package/lib/lib.dom.d.ts", "/// dom"),
        ],
    );
    let unpacked = scratch.path().join("unpacked");
    std::fs::create_dir_all(&unpacked).unwrap();
    std::fs::write(unpacked.join("left-by-an-earlier-run"), "").unwrap();

    let compiler_directory = unpack_the_typescript_compiler(&archive, &unpacked).unwrap();

    let mut placed: Vec<String> = std::fs::read_dir(&compiler_directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    placed.sort();
    assert_eq!(placed, ["lib.d.ts", "lib.dom.d.ts", "tsc"]);
    assert!(!unpacked.join("left-by-an-earlier-run").exists());
    assert!(!unpacked.join("package/package.json").exists());
}

#[test]
fn a_compiler_archive_without_its_library_files_is_refused() {
    let scratch = tempfile::tempdir().unwrap();
    let archive = scratch.path().join("typescript.tgz");
    archive_holding(&archive, &[("package/lib/tsc", "the compiler")]);

    let refusal = unpack_the_typescript_compiler(&archive, &scratch.path().join("unpacked"))
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("no library files"), "{refusal}");
}

#[test]
fn the_linter_is_unpacked_from_its_archive_by_its_targets_name() {
    let scratch = tempfile::tempdir().unwrap();
    let archive = scratch.path().join("oxlint.tar.gz");
    archive_holding(
        &archive,
        &[("oxlint-x86_64-unknown-linux-gnu", "the linter")],
    );

    let linter = unpack_the_linter(
        &archive,
        "oxlint-x86_64-unknown-linux-gnu",
        &scratch.path().join("unpacked"),
    )
    .unwrap();

    assert_eq!(std::fs::read_to_string(linter).unwrap(), "the linter");
}
