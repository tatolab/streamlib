// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end `streamlib link` / `streamlib unlink` against the per-app
//! `streamlib_modules/` folder, driving the real `streamlib` binary.
//!
//! Local-only integration counterpart to the
//! `streamlib-idents::app_modules` link/unlink unit tests. Locks the CLI
//! wiring (arg parsing, `--dir` anchoring, the `--engine` split, report
//! printing) and the npm-link dev loop: symlink a checkout, edit it, observe
//! the edit live through the slot, then unlink.

mod cli_integration_support;

use cli_integration_support::{run_streamlib_binary, write_single_processor_foo_package_source};

#[test]
fn link_then_edit_reflected_live_then_unlink() {
    let checkout = tempfile::tempdir().unwrap();
    write_single_processor_foo_package_source(checkout.path(), "a demo link package");
    let app_root = tempfile::tempdir().unwrap();
    let app_dir = app_root.path().to_str().unwrap();

    // --- link -----------------------------------------------------------
    let out = run_streamlib_binary(&["link", checkout.path().to_str().unwrap(), "--dir", app_dir]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "link failed: status={:?}\nstdout={stdout}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Linked @tatolab/foo v1.1.0"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("Processors (1):"), "stdout: {stdout}");

    let slot = app_root.path().join("streamlib_modules/@tatolab/foo");
    assert!(
        std::fs::symlink_metadata(&slot)
            .unwrap()
            .file_type()
            .is_symlink(),
        "slot must be a symlink, not a copy"
    );
    let lock = std::fs::read_to_string(app_root.path().join("streamlib.lock")).unwrap();
    assert!(lock.contains("@tatolab/foo"), "lock: {lock}");
    assert!(lock.contains("kind: link"), "lock: {lock}");

    // --- edit the checkout: the slot reflects it live, no re-link -------
    write_single_processor_foo_package_source(checkout.path(), "EDITED AFTER LINK");
    let slot_manifest = std::fs::read_to_string(slot.join("streamlib.yaml")).unwrap();
    assert!(
        slot_manifest.contains("EDITED AFTER LINK"),
        "checkout edit must be live through the link: {slot_manifest}"
    );

    // --- unlink ---------------------------------------------------------
    let out = run_streamlib_binary(&["unlink", "@tatolab/foo", "--dir", app_dir]);
    let ustdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "unlink failed: {ustdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        ustdout.contains("Unlinked @tatolab/foo"),
        "stdout: {ustdout}"
    );
    assert!(
        std::fs::symlink_metadata(&slot).is_err(),
        "slot must be gone"
    );
    let lock = std::fs::read_to_string(app_root.path().join("streamlib.lock")).unwrap();
    assert!(!lock.contains("@tatolab/foo"), "still locked: {lock}");
    // The linked checkout on disk is untouched.
    assert!(checkout.path().join("streamlib.yaml").is_file());

    // Unlinking an absent package fails loud.
    let out = run_streamlib_binary(&["unlink", "@tatolab/foo", "--dir", app_dir]);
    assert!(!out.status.success(), "unlink of absent package must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not linked"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn link_over_added_copy_flips_to_symlink_and_unlink_of_copy_is_refused() {
    let checkout = tempfile::tempdir().unwrap();
    write_single_processor_foo_package_source(checkout.path(), "a demo package");
    let app_root = tempfile::tempdir().unwrap();
    let app_dir = app_root.path().to_str().unwrap();

    // Add (a real copy), then unlink must refuse it (it's not a link).
    assert!(
        run_streamlib_binary(&["add", checkout.path().to_str().unwrap(), "--dir", app_dir])
            .status
            .success()
    );
    let out = run_streamlib_binary(&["unlink", "@tatolab/foo", "--dir", app_dir]);
    assert!(
        !out.status.success(),
        "unlink of an added copy must be refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("use `streamlib remove`"),
        "stderr should redirect to remove: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Link over the added copy: the slot flips to a symlink, lock flips to link.
    let out = run_streamlib_binary(&["link", checkout.path().to_str().unwrap(), "--dir", app_dir]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "link over add failed: {stdout}");
    assert!(stdout.contains("Relinked @tatolab/foo"), "stdout: {stdout}");
    let slot = app_root.path().join("streamlib_modules/@tatolab/foo");
    assert!(
        std::fs::symlink_metadata(&slot)
            .unwrap()
            .file_type()
            .is_symlink(),
        "slot must become a symlink"
    );
    let lock = std::fs::read_to_string(app_root.path().join("streamlib.lock")).unwrap();
    assert!(lock.contains("kind: link"), "lock: {lock}");
    assert_eq!(
        lock.matches("@tatolab/foo").count(),
        1,
        "one lock entry: {lock}"
    );
}

#[test]
fn link_a_non_directory_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("camera.slpkg");
    std::fs::write(&file, b"archive-not-a-folder").unwrap();
    let app_root = tempfile::tempdir().unwrap();

    let out = run_streamlib_binary(&[
        "link",
        file.to_str().unwrap(),
        "--dir",
        app_root.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "linking a file must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not a directory"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !app_root
            .path()
            .join("streamlib_modules")
            .join("@tatolab")
            .exists()
    );
}

#[test]
fn unlink_name_with_engine_is_a_usage_error() {
    // `unlink --engine` removes the whole-tree engine link and takes no
    // package name; a stray positional must bail rather than be silently
    // ignored (symmetric with the --dir / --force / --skip-verify guards).
    let out = run_streamlib_binary(&["unlink", "@tatolab/foo", "--engine"]);
    assert!(!out.status.success(), "unlink <name> --engine must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("takes no package name"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn bare_link_without_path_or_engine_is_a_usage_error() {
    // Pre-1.0: the old `link` (bare = engine link) shape is gone. A bare
    // `link` with no path and no `--engine` is a loud error.
    let out = run_streamlib_binary(&["link"]);
    assert!(!out.status.success(), "bare link must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("needs a package checkout path"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
