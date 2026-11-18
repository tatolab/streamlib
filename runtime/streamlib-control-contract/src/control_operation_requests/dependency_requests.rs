// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations on a project's dependencies and the fragments they bring.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use streamlib_project_repository::{
    DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION, DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION,
    DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GraphEditStream,
};

use super::{
    AUTHOR_ARGUMENT_DESCRIPTION, GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION, GraphEditMode,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorArgument,
};

/// What `importing_project_directory` says.
pub const IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project that imports, by absolute path: a folder holding `tatolab-project.json`.";

/// What `import_source` says.
pub const IMPORT_SOURCE_ARGUMENT_DESCRIPTION: &str = "Where the project is copied from, on this machine, by absolute path: its project folder, or a `.zip` of one, such as a GitHub archive downloaded first. It must hold a `tatolab-project.json` with a project UUID, at its root or in its one top folder.";

/// `add_dependency`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddDependencyRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the project is copied from, on this machine, by absolute path.
    #[schemars(description = IMPORT_SOURCE_ARGUMENT_DESCRIPTION)]
    pub source: String,
    /// The folder under `imports/` its copy takes.
    #[schemars(
        description = "The folder under `imports/` its copy takes; the imported project's name, lowercase with each run of other characters made `-`, when omitted."
    )]
    pub folder: Option<String>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `update_dependency`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateDependencyRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists
    /// under `dependencies`.
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    pub project: String,
    /// Where to copy it from this time, as `add_dependency` takes it.
    #[schemars(
        description = "Where to copy it from this time, as `add_dependency` takes it; the `source` it was last copied from when omitted."
    )]
    pub source: Option<String>,
    /// Overwrite the files of its folder edited since its copy, which an update without it lists
    /// under `edited_files` and leaves as they are.
    #[schemars(
        description = "Overwrite the files of its folder edited since its copy, which an update without it lists under `edited_files` and leaves as they are."
    )]
    pub overwrite_edited_files: Option<bool>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `list_dependencies`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDependenciesRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Read each import's `source` and answer the version it holds now beside the copy's.
    #[schemars(
        description = "Read each import's `source` and answer the version it holds now beside the copy's."
    )]
    pub check_for_newer: Option<bool>,
}

/// `remove_dependency`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveDependencyRequest {
    /// The project that imports, by absolute path.
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists
    /// under `dependencies`.
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    pub project: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `use_dependency_stream`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UseDependencyStreamRequest {
    /// The stream the edit is to, by its stream id or its stream asset's UUID.
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    pub stream: GraphEditStream,
    /// The imported project, by the UUID `add_dependency` answers and `tatolab-project.json` lists
    /// under `dependencies`.
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    pub project: String,
    /// The imported project's stream, by its stream asset's UUID, the `asset` its graph document
    /// under `imports/<folder>/` holds.
    #[schemars(description = DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION)]
    pub dependency_stream: String,
    /// The fragment node's name, cast to lowercase URL-safe.
    #[schemars(description = DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION)]
    pub name: Option<String>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `refresh_fragments`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefreshFragmentsRequest {
    /// The project, by absolute path.
    #[schemars(
        description = "The project, by absolute path: a folder holding `tatolab-project.json`."
    )]
    pub project_directory: PathBuf,
    /// The project's Python files that changed, each by its path relative to the project directory
    /// or absolute inside it, e.g.
    #[schemars(
        description = "The project's Python files that changed, each by its path relative to the project directory or absolute inside it, e.g. `generators/looks.py`; each names the module it is imported as."
    )]
    pub changed_files: Vec<PathBuf>,
    /// `commit`, the default.
    #[schemars(description = GRAPH_EDIT_MODE_ARGUMENT_DESCRIPTION)]
    pub mode: Option<GraphEditMode>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}
