// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's node types and graph generators, read from source in the project's own
//! interpreter without running any of its code.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::entry_run_in_the_projects_interpreter::{
    EntryRunBounds, entry_command_in_the_projects_interpreter,
    run_an_entry_in_the_projects_interpreter,
};
use crate::core::error::{Error, Result};
use crate::core::runtime::the_projects_venv_interpreter;

/// The `tatolab.stream` module whose `-m` run reads a project's node types and graph
/// generators from source and prints the listing on standard output.
pub const PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE: &str =
    "tatolab.stream._project_node_type_listing_entry";

/// How long one listing may take before it is killed and refused.
pub(crate) const PROJECT_NODE_TYPE_LISTING_BOUND: Duration = Duration::from_secs(60);

/// The most of a listing's standard output read: the one document it prints.
const PROJECT_NODE_TYPE_LISTING_STANDARD_OUTPUT_KEPT_BYTES: usize = 64 * 1024 * 1024;

/// The label each line of a listing's standard error is logged under.
const PROJECT_NODE_TYPE_LISTING_STANDARD_ERROR_LOG_LABEL: &str = "project-node-type-listing";

const PROJECT_NODE_TYPE_LISTING_RUN_BOUNDS: EntryRunBounds = EntryRunBounds {
    bound: PROJECT_NODE_TYPE_LISTING_BOUND,
    standard_output_kept_bytes: PROJECT_NODE_TYPE_LISTING_STANDARD_OUTPUT_KEPT_BYTES,
    standard_error_log_label: PROJECT_NODE_TYPE_LISTING_STANDARD_ERROR_LOG_LABEL,
    what_runs: "a project node type listing",
};

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
    entry_command_in_the_projects_interpreter(
        &the_projects_venv_interpreter(project_directory),
        project_directory,
        PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE,
    )
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
    let listing = run_an_entry_in_the_projects_interpreter(
        &mut project_node_type_listing_command(project_directory),
        request_json,
        &PROJECT_NODE_TYPE_LISTING_RUN_BOUNDS,
    )
    .map_err(|spawn_failure| {
        refused(format!(
            "the project's interpreter `{interpreter}` could not be started: {spawn_failure}"
        ))
    })?;
    let quoted_standard_error = &listing.quoted_standard_error;
    if listing.ran_past_its_bound {
        return Err(refused(format!(
            "it ran past {}s in `{interpreter}` and was killed. {quoted_standard_error}",
            PROJECT_NODE_TYPE_LISTING_BOUND.as_secs()
        )));
    }
    let document: ProjectNodeTypeListingDocument = serde_json::from_slice(&listing.standard_output)
        .map_err(|not_a_document| {
            refused(format!(
                "`{interpreter}` exited with {} and printed no listing ({not_a_document}); its \
                 `tatolab-stream` may predate the node type listing. {quoted_standard_error}",
                listing.exit_status_rendered
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
