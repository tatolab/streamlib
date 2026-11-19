// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One bounded run of a `tatolab.stream` entry module in a project's own interpreter: what it
//! wrote on standard output, how it ended and the tail of its standard error.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::node_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use super::node_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, StreamInterpreterExitAwaited,
    remove_every_python_variable_named_in, spawn_standard_error_reader_keeping_its_tail,
    spawn_standard_output_reader_keeping_its_head, standard_error_tail_as_a_refusal_quotes_it,
    wait_for_a_stream_interpreter_to_exit,
};
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind;

/// What bounds one entry's run and what it is called in the runtime's own log.
pub(crate) struct EntryRunBounds {
    /// How long the run may take before its process group is killed.
    pub(crate) bound: Duration,
    /// The most of its standard output kept.
    pub(crate) standard_output_kept_bytes: usize,
    /// The label each line of its standard error is logged under.
    pub(crate) standard_error_log_label: &'static str,
    /// What the run is, for the log line of its kill: `a project node type listing`.
    pub(crate) what_runs: &'static str,
}

/// How one entry's run ended.
pub(crate) struct EntryRunInTheProjectsInterpreter {
    /// It was still running at its bound and was killed with its process group.
    pub(crate) ran_past_its_bound: bool,
    /// What it wrote on standard output, up to its bound; all of it when it was killed.
    pub(crate) standard_output: Vec<u8>,
    /// It exited on its own, with a status of success.
    pub(crate) exited_successfully: bool,
    /// Its exit status as words.
    pub(crate) exit_status_rendered: String,
    /// The tail of its standard error, as a refusal quotes it.
    pub(crate) quoted_standard_error: String,
}

/// The command that runs `entry_module` with `-I -m` in `interpreter`, in `project_directory`,
/// with no `PYTHON*` variable of ours, every standard stream piped and its lifetime bound to
/// ours. A caller adds the entry's own arguments.
pub(crate) fn entry_command_in_the_projects_interpreter(
    interpreter: &Path,
    project_directory: &Path,
    entry_module: &str,
) -> Command {
    let mut command = Command::new(interpreter);
    command.current_dir(project_directory);
    remove_every_python_variable_named_in(
        &mut command,
        std::env::vars_os().map(|(variable_name, _)| variable_name),
    );
    command
        .args(["-I", "-m", entry_module])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    command
}

/// Run `command`, writing `standard_input` to it and closing it, until it exits or `bounds`
/// ends it. Errs only when the command could not be started.
pub(crate) fn run_an_entry_in_the_projects_interpreter(
    command: &mut Command,
    standard_input: Vec<u8>,
    bounds: &EntryRunBounds,
) -> std::io::Result<EntryRunInTheProjectsInterpreter> {
    let mut child = spawn_outside_every_iceoryx2_listener_bind(command)?;
    let standard_input_writer = child.stdin.take().map(|mut child_standard_input| {
        std::thread::spawn(move || {
            let _ = child_standard_input.write_all(&standard_input);
        })
    });
    let standard_output_head = child.stdout.take().map(|standard_output| {
        spawn_standard_output_reader_keeping_its_head(
            standard_output,
            bounds.standard_output_kept_bytes,
        )
    });
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            bounds.standard_error_log_label,
        )
    });
    let run_exit =
        wait_for_a_stream_interpreter_to_exit(&child, bounds.bound, &|| false, &|| false);
    let exit_status = kill_the_process_group_and_reap_its_leader(&mut child, bounds.what_runs);
    if let Some(standard_input_writer) = standard_input_writer {
        let _ = standard_input_writer.join();
    }
    let quoted_standard_error = standard_error_tail_as_a_refusal_quotes_it(
        &standard_error_tail
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default(),
    );
    let standard_output = standard_output_head
        .map(|head| head.bytes_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE))
        .unwrap_or_default();
    Ok(EntryRunInTheProjectsInterpreter {
        ran_past_its_bound: run_exit == StreamInterpreterExitAwaited::BoundElapsed,
        exited_successfully: run_exit == StreamInterpreterExitAwaited::Exited
            && exit_status.is_some_and(|exit_status| exit_status.success()),
        standard_output,
        exit_status_rendered: exit_status
            .map(|exit_status| exit_status.to_string())
            .unwrap_or_else(|| "an exit status that could not be collected".to_string()),
        quoted_standard_error,
    })
}
