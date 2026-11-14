// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The cross-floor check of a project's Python, run in the project's own interpreter as a
//! load reads the project, its findings a warning in the project's log that never blocks a
//! start.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::processor_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use super::processor_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, StreamInterpreterExitAwaited,
    spawn_standard_error_reader_keeping_its_tail, standard_error_tail_as_a_refusal_quotes_it,
    stream_interpreter_command_in_its_project_directory, wait_for_a_stream_interpreter_to_exit,
};
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::error::Result;
use crate::core::runtime::StreamEnvironment;
use crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind;

/// The `tatolab.stream` module whose `-m` run prints the cross-floor warning block of the
/// project directory it is handed on standard error.
pub(crate) const CROSS_FLOOR_CHECK_MODULE: &str = "tatolab.stream._cross_floor_check";

/// How long the check may read the project's source before it is killed and skipped.
pub(crate) const CROSS_FLOOR_CHECK_BOUND: Duration = Duration::from_secs(15);

/// The label each line of the check's standard error is logged under.
const CROSS_FLOOR_CHECK_STANDARD_ERROR_LOG_LABEL: &str = "cross-floor-check";

/// The command that runs the cross-floor check over the project of `stream_environment`.
pub(crate) fn cross_floor_check_command(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    let mut command = stream_interpreter_command_in_its_project_directory(
        &stream_environment.interpreter,
        &stream_environment.project_directory,
        lend_directory,
    )?;
    // `-I` keeps the project directory off `sys.path`: the check reads the project's source
    // as text and imports none of it.
    command
        .args(["-I", "-m", CROSS_FLOOR_CHECK_MODULE])
        .arg(&stream_environment.project_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    Ok(command)
}

/// Run the cross-floor check over the project of `stream_environment`, each line it prints
/// logged where the caller's log route sends it. Advisory: a check that cannot start, runs
/// past [`CROSS_FLOOR_CHECK_BOUND`], is interrupted or fails is logged as a warning, and the
/// load goes on.
pub(crate) fn warn_of_what_binds_the_project_to_one_floor(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) {
    let project = stream_environment.project_directory.display();
    let mut command = match cross_floor_check_command(stream_environment, lend_directory) {
        Ok(command) => command,
        Err(refusal) => {
            tracing::warn!("the cross-floor check of {project} could not run: {refusal}");
            return;
        }
    };
    let mut child = match spawn_outside_every_iceoryx2_listener_bind(&mut command) {
        Ok(child) => child,
        Err(spawn_failure) => {
            tracing::warn!(
                "the cross-floor check of {project} could not start `{}`: {spawn_failure}",
                stream_environment.interpreter.display()
            );
            return;
        }
    };
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            CROSS_FLOOR_CHECK_STANDARD_ERROR_LOG_LABEL,
        )
    });
    let check_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        CROSS_FLOOR_CHECK_BOUND,
        &|| false,
        is_interrupted_by_the_host,
    );
    let exit_status =
        kill_the_process_group_and_reap_its_leader(&mut child, "the cross-floor check");
    let quoted_standard_error = || {
        standard_error_tail_as_a_refusal_quotes_it(
            &standard_error_tail
                .as_ref()
                .map(|tail| {
                    tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
                })
                .unwrap_or_default(),
        )
    };
    match check_exit {
        StreamInterpreterExitAwaited::Exited
            if exit_status.is_some_and(|status| status.success()) => {}
        StreamInterpreterExitAwaited::Exited => tracing::warn!(
            "the cross-floor check of {project} failed and was skipped: {}",
            quoted_standard_error()
        ),
        StreamInterpreterExitAwaited::BoundElapsed => tracing::warn!(
            "the cross-floor check of {project} ran past {}s and was skipped",
            CROSS_FLOOR_CHECK_BOUND.as_secs()
        ),
        StreamInterpreterExitAwaited::ItsStreamsShutdownRequested
        | StreamInterpreterExitAwaited::InterruptedByTheHost => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_runs_its_module_isolated_over_the_project_directory() {
        let project_directory = tempfile::tempdir().unwrap();
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter: project_directory.path().join(".venv/bin/python"),
        };
        let command =
            cross_floor_check_command(&stream_environment, Path::new("/opt/tatolab/lend")).unwrap();
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(
            arguments,
            [
                std::ffi::OsStr::new("-I"),
                std::ffi::OsStr::new("-m"),
                std::ffi::OsStr::new(CROSS_FLOOR_CHECK_MODULE),
                project_directory.path().as_os_str(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(project_directory.path()));
        assert_eq!(command.get_program(), stream_environment.interpreter);
    }
}
