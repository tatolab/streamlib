// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;

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
