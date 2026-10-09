// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::unix::fs::PermissionsExt;

use crate::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE;

/// A temporary directory at exactly owner-only mode, whatever umask it was made under.
pub(crate) fn a_temporary_directory_at_owner_only_mode() -> std::io::Result<tempfile::TempDir> {
    let temporary_directory = tempfile::tempdir()?;
    std::fs::set_permissions(
        temporary_directory.path(),
        std::fs::Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE),
    )?;
    Ok(temporary_directory)
}

/// Re-run the test at `test_path` in a child process of this test binary with
/// `environment_variable` set to `value`, and wait for it — for a test that
/// changes something process-wide.
pub(crate) fn rerun_this_test_in_a_child_process(
    test_path: &str,
    environment_variable: &str,
    value: &std::ffi::OsStr,
) -> std::process::Output {
    rerun_this_test_in_a_child_process_with_its_environment(test_path, |child_command| {
        child_command.env(environment_variable, value);
    })
}

/// Re-run the test at `test_path` in a child process of this test binary whose
/// environment `prepare_the_child_environment` sets, and wait for it.
pub(crate) fn rerun_this_test_in_a_child_process_with_its_environment(
    test_path: &str,
    prepare_the_child_environment: impl FnOnce(&mut std::process::Command),
) -> std::process::Output {
    let mut child_command =
        std::process::Command::new(std::env::current_exe().expect("the test binary's own path"));
    child_command
        .args([test_path, "--exact", "--test-threads=1", "--nocapture"])
        .stdin(std::process::Stdio::null());
    prepare_the_child_environment(&mut child_command);
    let child_process_output = child_command
        .output()
        .expect("the test binary re-runs this test in a child process");
    // `--exact` on a name that matches nothing runs no test and exits 0, which
    // reads as a pass for a test that was renamed away.
    let child_standard_output = String::from_utf8_lossy(&child_process_output.stdout);
    assert!(
        child_standard_output.contains("running 1 test"),
        "the child process ran no test named `{test_path}`:\n{child_standard_output}"
    );
    child_process_output
}
