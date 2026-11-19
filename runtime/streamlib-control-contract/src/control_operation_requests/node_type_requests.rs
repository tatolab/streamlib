// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, AUTHOR_ARGUMENT_DESCRIPTION,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION,
    ProjectTransactionAuthorArgument,
};

/// `list_pin_types`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPinTypesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `list_node_types`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListNodeTypesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
}

/// `describe_node_types`' params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeNodeTypesRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The node types to describe, each as `list_node_types` names it under `node_type`.
    #[schemars(
        description = "The node types to describe, each as `list_node_types` names it under `node_type`.",
        length(min = 1)
    )]
    #[serde(deserialize_with = "at_least_one_node_type")]
    pub node_types: Vec<String>,
}

/// The node types a request names, refused when it names none, as the schema's `minItems` says.
fn at_least_one_node_type<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let node_types = Vec::<String>::deserialize(deserializer)?;
    if node_types.is_empty() {
        return Err(serde::de::Error::custom(
            "`node_types` names no node type; name at least one, as `list_node_types` lists it",
        ));
    }
    Ok(node_types)
}

/// `set_node_type_group`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetNodeTypeGroupRequest {
    /// The project's directory.
    #[schemars(description = PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The node type to move, as `list_node_types` names it.
    #[schemars(
        description = "The node type to move, as `list_node_types` names it under `node_type`: the project's own, an import's, an installed distribution's or a built-in."
    )]
    pub node_type: String,
    /// The group to file it under.
    #[schemars(
        description = "The group to file it under, a path where the group is inside another, as `Effects/Stylize`; a group no node type is filed under yet is made by filing one there. `null` puts it back where an import or its author files it."
    )]
    pub group: Option<String>,
    /// Who the change is by.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// The key the caller minted for this gesture.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}
