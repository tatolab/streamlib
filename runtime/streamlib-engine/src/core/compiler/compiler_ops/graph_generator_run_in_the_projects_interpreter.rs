// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One run of a project's graph generator in the project's own interpreter, for a fragment's
//! import or refresh: the fragment node it is stored as, or the project's own traceback.

use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::processor_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use super::processor_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, StreamInterpreterExitAwaited,
    remove_every_python_variable_named_in, spawn_standard_error_reader_keeping_its_tail,
    spawn_standard_output_reader_keeping_its_head, standard_error_tail_as_a_refusal_quotes_it,
    wait_for_a_stream_interpreter_to_exit,
};
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::error::{Error, Result};
use crate::core::runtime::StreamEnvironment;
use crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind;

/// The `tatolab.stream` module whose `-m` run runs one graph generator and prints the
/// document of that run on standard output.
pub(crate) const GRAPH_GENERATOR_RUN_ENTRY_MODULE: &str =
    "tatolab.stream._graph_generator_run_entry";

/// How long one generator run may take before it is killed and refused.
pub(crate) const GRAPH_GENERATOR_RUN_BOUND: Duration = Duration::from_secs(60);

/// The most of a run's standard output read: the one document it prints.
const GRAPH_GENERATOR_RUN_STANDARD_OUTPUT_KEPT_BYTES: usize = 64 * 1024 * 1024;

/// The label each line of a run's standard error is logged under.
const GRAPH_GENERATOR_RUN_STANDARD_ERROR_LOG_LABEL: &str = "graph-generator-run";

/// What the run entry prints: the fragment node placed, or why none was.
#[derive(Deserialize)]
struct GraphGeneratorRunDocument {
    placed: Option<Value>,
    failed_because: Option<String>,
}

/// The command that runs `generator` over `promoted_settings` as a fragment named
/// `fragment_name` in the project of `stream_environment`.
pub(crate) fn graph_generator_run_command(
    stream_environment: &StreamEnvironment,
    generator: &str,
    fragment_name: Option<&str>,
    promoted_settings: &Map<String, Value>,
) -> Result<Command> {
    let promoted_settings_json = serde_json::to_string(promoted_settings).map_err(|not_written| {
        Error::Configuration(format!(
            "graph generator `{generator}`'s promoted settings do not write as JSON: {not_written}"
        ))
    })?;
    let mut command = Command::new(&stream_environment.interpreter);
    command.current_dir(&stream_environment.project_directory);
    remove_every_python_variable_named_in(
        &mut command,
        std::env::vars_os().map(|(variable_name, _)| variable_name),
    );
    command
        .args(["-I", "-m", GRAPH_GENERATOR_RUN_ENTRY_MODULE, generator])
        .args(
            fragment_name
                .map(|name| ["--name", name])
                .into_iter()
                .flatten(),
        )
        .args(["--settings", &promoted_settings_json])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    Ok(command)
}

/// Run `generator` over `promoted_settings` as a fragment named `fragment_name`, or its own
/// name, in the project of `stream_environment`, answering the fragment node it is stored as:
/// `{name, type, config, fragment}`, its nodes and links by name. Refused with the project's
/// own traceback when the generator did not run, and when the run could not start, ran past
/// [`GRAPH_GENERATOR_RUN_BOUND`] or printed no document.
pub(crate) fn run_a_graph_generator_in_the_projects_interpreter(
    stream_environment: &StreamEnvironment,
    generator: &str,
    fragment_name: Option<&str>,
    promoted_settings: &Map<String, Value>,
) -> Result<Value> {
    let interpreter = stream_environment.interpreter.display();
    let refused = |why: String| {
        Error::Configuration(format!("graph generator `{generator}` did not run: {why}"))
    };
    let mut command = graph_generator_run_command(
        stream_environment,
        generator,
        fragment_name,
        promoted_settings,
    )?;
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            refused(format!(
                "the project's interpreter `{interpreter}` could not be started in `{}`: \
             {spawn_failure}",
                stream_environment.project_directory.display()
            ))
        })?;
    let standard_output_head = child.stdout.take().map(|standard_output| {
        spawn_standard_output_reader_keeping_its_head(
            standard_output,
            GRAPH_GENERATOR_RUN_STANDARD_OUTPUT_KEPT_BYTES,
        )
    });
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            GRAPH_GENERATOR_RUN_STANDARD_ERROR_LOG_LABEL,
        )
    });
    let run_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        GRAPH_GENERATOR_RUN_BOUND,
        &|| false,
        &|| false,
    );
    let exit_status =
        kill_the_process_group_and_reap_its_leader(&mut child, "a graph generator run");
    let quoted_standard_error = standard_error_tail_as_a_refusal_quotes_it(
        &standard_error_tail
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default(),
    );
    if run_exit == StreamInterpreterExitAwaited::BoundElapsed {
        return Err(refused(format!(
            "it ran past {}s in `{interpreter}` and was killed. {quoted_standard_error}",
            GRAPH_GENERATOR_RUN_BOUND.as_secs()
        )));
    }
    let standard_output_bytes = standard_output_head
        .map(|head| head.bytes_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE))
        .unwrap_or_default();
    let exit_status_rendered = exit_status
        .map(|exit_status| exit_status.to_string())
        .unwrap_or_else(|| "an exit status that could not be collected".to_string());
    let document: GraphGeneratorRunDocument = serde_json::from_slice(&standard_output_bytes)
        .map_err(|not_a_document| {
            refused(format!(
                "`{interpreter}` exited with {exit_status_rendered} and printed no run document \
                 ({not_a_document}); its `tatolab-stream` may predate graph generators. \
                 {quoted_standard_error}"
            ))
        })?;
    match (document.placed, document.failed_because) {
        (Some(placed), None) => Ok(placed),
        (_, Some(failed_because)) => Err(refused(failed_because)),
        (None, None) => Err(refused(format!(
            "`{interpreter}` printed a run document naming neither the fragment nor why. \
             {quoted_standard_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_names_its_entry_isolated_with_the_generator_name_and_settings() {
        let project_directory = tempfile::tempdir().unwrap();
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter: project_directory.path().join(".venv/bin/python"),
        };
        let mut promoted_settings = Map::new();
        promoted_settings.insert("radius".to_owned(), Value::from(8));

        let command = graph_generator_run_command(
            &stream_environment,
            "generators.look:look",
            Some("front-look"),
            &promoted_settings,
        )
        .unwrap();

        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            arguments,
            [
                "-I",
                "-m",
                GRAPH_GENERATOR_RUN_ENTRY_MODULE,
                "generators.look:look",
                "--name",
                "front-look",
                "--settings",
                r#"{"radius":8}"#,
            ]
        );
        assert_eq!(command.get_current_dir(), Some(project_directory.path()));
        assert_eq!(command.get_program(), stream_environment.interpreter);
    }

    #[test]
    fn a_run_whose_interpreter_cannot_start_is_refused_naming_the_generator() {
        let project_directory = tempfile::tempdir().unwrap();
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter: project_directory.path().join(".venv/bin/python"),
        };

        let refusal = run_a_graph_generator_in_the_projects_interpreter(
            &stream_environment,
            "generators.look:look",
            None,
            &Map::new(),
        )
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains("graph generator `generators.look:look` did not run")
                && refusal.contains("could not be started"),
            "{refusal}"
        );
    }
}
