// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations that create, import, rename, describe, cover and duplicate a project.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    AUTHOR_ARGUMENT_DESCRIPTION, IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION,
    PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorArgument,
};

/// `rename_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameProjectRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The project's new name.
    #[schemars(description = "The project's new name; never blank.")]
    pub name: String,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_project_description`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetProjectDescriptionRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// What the project is.
    #[schemars(description = "What the project is; `null` or blank clears it.")]
    pub description: Option<String>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_project_cover`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetProjectCoverRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The media asset the project is shown by.
    #[schemars(
        description = "The media asset the project is shown by, by the UUID `import_asset` or `list_assets` answered; `null` clears it."
    )]
    pub cover: Option<String>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `duplicate_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateProjectRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the duplicate goes.
    #[schemars(
        description = "Where the duplicate goes, by absolute path: a folder that does not exist yet, outside the project."
    )]
    pub into: PathBuf,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
}

/// `import_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportProjectRequest {
    /// The `.zip` of a project on this machine, by absolute path, such as a GitHub archive
    /// downloaded first.
    #[schemars(
        description = "The `.zip` of a project on this machine, by absolute path, such as a GitHub archive downloaded first. It must hold a `tatolab-project.json` with a project UUID, at its root or in its one top folder."
    )]
    pub source: String,
    /// Where the new project is made, by absolute path.
    #[schemars(
        description = "Where the new project is made, by absolute path: a folder that does not exist yet, its parent made when missing."
    )]
    pub project_directory: PathBuf,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
}

/// `list_samples`'s params: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSamplesRequest {}

/// `create_project`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectRequest {
    /// Where to create the project, by absolute path.
    #[schemars(
        description = "Where to create the project, by absolute path: created when missing, and holding none of the files a new project starts with. Omit it to create the project in a folder named `<adjective>-<noun>`, drawn at random, under `~/streams`."
    )]
    #[serde(default)]
    pub project_directory: Option<PathBuf>,
    /// The `name` of a sample project `list_samples` lists, to copy into the new project instead of
    /// the starter.
    #[schemars(
        description = "The `name` of a sample project `list_samples` lists, to copy into the new project instead of the starter. Omit it for the starter."
    )]
    #[serde(default)]
    pub sample: Option<String>,
}
