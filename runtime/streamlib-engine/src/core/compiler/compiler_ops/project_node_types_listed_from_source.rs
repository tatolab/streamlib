// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project's node types and graph generators, read from source in the project's own
//! interpreter without running any of its code.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
use crate::core::runtime::the_projects_venv_interpreter;
use crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind;

/// The `tatolab.stream` module whose `-m` run reads a project's node types and graph
/// generators from source and prints the listing on standard output.
pub const PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE: &str =
    "tatolab.stream._project_node_type_listing_entry";

/// How long one listing may take before it is killed and refused.
const PROJECT_NODE_TYPE_LISTING_BOUND: Duration = Duration::from_secs(60);

/// The most of a listing's standard output read: the one document it prints.
const PROJECT_NODE_TYPE_LISTING_STANDARD_OUTPUT_KEPT_BYTES: usize = 64 * 1024 * 1024;

/// The label each line of a listing's standard error is logged under.
const PROJECT_NODE_TYPE_LISTING_STANDARD_ERROR_LOG_LABEL: &str = "project-node-type-listing";

/// An imported project's Python files as its folder under `imports/` holds them now, as the
/// listing reads them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportedProjectPythonSources {
    /// The imported project's UUID.
    pub project: String,
    /// Its folder under `imports/`.
    pub folder: String,
    /// The version its copy carried.
    pub version: String,
    /// Each `.py` file of its folder, by its path inside the folder.
    pub files: BTreeMap<String, String>,
}

/// What the listing entry reads on standard input: every import's Python sources.
#[derive(Serialize)]
struct ProjectNodeTypeListingRequest<'request> {
    dependencies: &'request [ImportedProjectPythonSources],
}

/// What the listing entry prints: every source's listing, or why there is none.
#[derive(Deserialize)]
struct ProjectNodeTypeListingDocument {
    sources: Option<Vec<Value>>,
    failed_because: Option<String>,
}

/// The command that lists the node types and graph generators of the project at
/// `project_directory` in its venv's interpreter.
fn project_node_type_listing_command(project_directory: &Path) -> Command {
    let mut command = Command::new(the_projects_venv_interpreter(project_directory));
    command.current_dir(project_directory);
    remove_every_python_variable_named_in(
        &mut command,
        std::env::vars_os().map(|(variable_name, _)| variable_name),
    );
    command
        .args(["-I", "-m", PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    command
}

/// Every source's node types and graph generators for the project at `project_directory`:
/// the project's own node code outside `imports/`, each import's from `import_sources`, and each installed distribution's that declares `tatolab.nodes`.
/// Each source is `{source, node_types, graph_generators, unreadable_modules}`. Refused when
/// the project's interpreter could not start, ran past its bound or printed no listing.
pub fn list_a_projects_node_types_and_graph_generators_from_source(
    project_directory: &Path,
    import_sources: &[ImportedProjectPythonSources],
) -> Result<Vec<Value>> {
    let interpreter_path = the_projects_venv_interpreter(project_directory);
    let interpreter = interpreter_path.display();
    let refused = |why: String| {
        Error::Configuration(format!(
            "the node types of the project `{}` were not listed: {why}",
            project_directory.display()
        ))
    };
    let request_json = serde_json::to_vec(&ProjectNodeTypeListingRequest {
        dependencies: import_sources,
    })
    .map_err(|not_written| refused(format!("its request does not write as JSON: {not_written}")))?;
    let mut command = project_node_type_listing_command(project_directory);
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            refused(format!(
                "the project's interpreter `{interpreter}` could not be started: {spawn_failure}"
            ))
        })?;
    let standard_input_writer = child.stdin.take().map(|mut standard_input| {
        std::thread::spawn(move || {
            let _ = standard_input.write_all(&request_json);
        })
    });
    let standard_output_head = child.stdout.take().map(|standard_output| {
        spawn_standard_output_reader_keeping_its_head(
            standard_output,
            PROJECT_NODE_TYPE_LISTING_STANDARD_OUTPUT_KEPT_BYTES,
        )
    });
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            PROJECT_NODE_TYPE_LISTING_STANDARD_ERROR_LOG_LABEL,
        )
    });
    let listing_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        PROJECT_NODE_TYPE_LISTING_BOUND,
        &|| false,
        &|| false,
    );
    let exit_status =
        kill_the_process_group_and_reap_its_leader(&mut child, "a project node type listing");
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
    if listing_exit == StreamInterpreterExitAwaited::BoundElapsed {
        return Err(refused(format!(
            "it ran past {}s in `{interpreter}` and was killed. {quoted_standard_error}",
            PROJECT_NODE_TYPE_LISTING_BOUND.as_secs()
        )));
    }
    let standard_output_bytes = standard_output_head
        .map(|head| head.bytes_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE))
        .unwrap_or_default();
    let exit_status_rendered = exit_status
        .map(|exit_status| exit_status.to_string())
        .unwrap_or_else(|| "an exit status that could not be collected".to_string());
    let document: ProjectNodeTypeListingDocument = serde_json::from_slice(&standard_output_bytes)
        .map_err(|not_a_document| {
        refused(format!(
            "`{interpreter}` exited with {exit_status_rendered} and printed no listing \
                 ({not_a_document}); its `tatolab-stream` may predate the node type listing. \
                 {quoted_standard_error}"
        ))
    })?;
    match (document.sources, document.failed_because) {
        (Some(sources), None) => Ok(sources),
        (_, Some(failed_because)) => Err(refused(failed_because)),
        (None, None) => Err(refused(format!(
            "`{interpreter}` printed a listing naming neither its sources nor why. \
             {quoted_standard_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_runs_its_entry_isolated_in_the_projects_venv() {
        let project_directory = tempfile::tempdir().unwrap();

        let command = project_node_type_listing_command(project_directory.path());

        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            arguments,
            ["-I", "-m", PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE]
        );
        assert_eq!(command.get_current_dir(), Some(project_directory.path()));
        assert_eq!(
            command.get_program(),
            project_directory.path().join(".venv/bin/python")
        );
    }

    #[test]
    fn a_listing_whose_interpreter_cannot_start_is_refused_naming_the_project() {
        let project_directory = tempfile::tempdir().unwrap();

        let refusal = list_a_projects_node_types_and_graph_generators_from_source(
            project_directory.path(),
            &[],
        )
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains("were not listed") && refusal.contains("could not be started"),
            "{refusal}"
        );
    }
}
