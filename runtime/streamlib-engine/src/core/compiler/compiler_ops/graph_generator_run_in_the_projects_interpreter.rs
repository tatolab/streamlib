// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One run of a project's graph generator in the project's own interpreter, for a fragment's
//! import or refresh: the fragment node it is stored as, or the project's own traceback.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::entry_run_in_the_projects_interpreter::{
    EntryRunBounds, entry_command_in_the_projects_interpreter,
    run_an_entry_in_the_projects_interpreter,
};
use crate::core::error::{Error, Result};
use crate::core::runtime::StreamEnvironment;

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

const GRAPH_GENERATOR_RUN_BOUNDS: EntryRunBounds = EntryRunBounds {
    bound: GRAPH_GENERATOR_RUN_BOUND,
    standard_output_kept_bytes: GRAPH_GENERATOR_RUN_STANDARD_OUTPUT_KEPT_BYTES,
    standard_error_log_label: GRAPH_GENERATOR_RUN_STANDARD_ERROR_LOG_LABEL,
    what_runs: "a graph generator run",
};

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
    let mut command = entry_command_in_the_projects_interpreter(
        &stream_environment.interpreter,
        &stream_environment.project_directory,
        GRAPH_GENERATOR_RUN_ENTRY_MODULE,
    );
    command
        .arg(generator)
        .args(
            fragment_name
                .map(|name| ["--name", name])
                .into_iter()
                .flatten(),
        )
        .args(["--settings", &promoted_settings_json]);
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
    let run = run_an_entry_in_the_projects_interpreter(
        &mut command,
        Vec::new(),
        &GRAPH_GENERATOR_RUN_BOUNDS,
    )
    .map_err(|spawn_failure| {
        refused(format!(
            "the project's interpreter `{interpreter}` could not be started in `{}`: \
             {spawn_failure}",
            stream_environment.project_directory.display()
        ))
    })?;
    let quoted_standard_error = &run.quoted_standard_error;
    if run.ran_past_its_bound {
        return Err(refused(format!(
            "it ran past {}s in `{interpreter}` and was killed. {quoted_standard_error}",
            GRAPH_GENERATOR_RUN_BOUND.as_secs()
        )));
    }
    let document: GraphGeneratorRunDocument = serde_json::from_slice(&run.standard_output)
        .map_err(|not_a_document| {
            refused(format!(
                "`{interpreter}` exited with {} and printed no run document ({not_a_document}); \
                 its `tatolab-stream` may predate graph generators. {quoted_standard_error}",
                run.exit_status_rendered
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
