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
    let mut child_command = a_command_that_reruns_this_test(
        &std::env::current_exe().expect("the test binary's own path"),
        test_path,
    );
    child_command.stdin(std::process::Stdio::null());
    prepare_the_child_environment(&mut child_command);
    let child_process_output = child_command
        .output()
        .expect("the test binary re-runs this test in a child process");
    assert_the_child_process_ran_the_test(test_path, &child_process_output);
    child_process_output
}

/// Start the test at `test_path` from `test_binary` — this test binary or a copy
/// of it — with piped standard streams, without waiting for it.
pub(crate) fn spawn_this_test_from_a_test_binary(
    test_binary: &std::path::Path,
    test_path: &str,
    prepare_the_child_environment: impl FnOnce(&mut std::process::Command),
) -> std::io::Result<std::process::Child> {
    let mut child_command = a_command_that_reruns_this_test(test_binary, test_path);
    child_command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    prepare_the_child_environment(&mut child_command);
    child_command.spawn()
}

fn a_command_that_reruns_this_test(
    test_binary: &std::path::Path,
    test_path: &str,
) -> std::process::Command {
    let mut child_command = std::process::Command::new(test_binary);
    child_command.args([test_path, "--exact", "--test-threads=1", "--nocapture"]);
    child_command
}

/// `--exact` on a name that matches nothing runs no test and exits 0, which
/// reads as a pass for a test that was renamed away.
pub(crate) fn assert_the_child_process_ran_the_test(
    test_path: &str,
    child_process_output: &std::process::Output,
) {
    let child_standard_output = String::from_utf8_lossy(&child_process_output.stdout);
    assert!(
        child_standard_output.contains("running 1 test"),
        "the child process ran no test named `{test_path}`:\n{child_standard_output}"
    );
}
