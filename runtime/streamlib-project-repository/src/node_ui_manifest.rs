// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `node_ui/tatolab_node_ui.json`: the controls and node bodies a module registers. The shape is
//! written here once; the control contract renders it, and the runtime client's TypeScript type
//! and the JSON Schema an author's editor reads are generated from that.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use streamlib_node_schema::pin_type::BuiltInPinTypeName;

use crate::tatolab_view_manifest::{
    a_value_never_written_as_null, is_a_contract_version,
    is_a_contract_version_of_major_up_to_minor, is_a_path_inside_the_views_folder,
    the_field_a_serde_refusal_names,
};

/// The folder of a module that holds its node UI elements.
pub const NODE_UI_DIRECTORY: &str = "node_ui";

/// The manifest the node UI folder holds.
pub const NODE_UI_MANIFEST_FILE_NAME: &str = "tatolab_node_ui.json";

/// The view contract a node UI manifest is read against: another major does not open.
pub const NODE_UI_CONTRACT_READ_MAJOR: u64 = 1;
/// The highest minor of [`NODE_UI_CONTRACT_READ_MAJOR`] a node UI manifest is read against; node
/// UI elements arrive in this minor.
pub const NODE_UI_CONTRACT_READ_MINOR: u64 = 1;

/// What a module's `node_ui/tatolab_node_ui.json` registers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp-tool-json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeUiManifest {
    /// The view contract the elements were built for, as `"1.1"`.
    pub contract: String,
    /// Each control the module registers, which edits or shows an input of one pin type.
    #[serde(default)]
    pub controls: Vec<NodeUiManifestControl>,
    /// Each node body the module registers, which draws the inside of one node type's node.
    #[serde(default)]
    pub node_bodies: Vec<NodeUiManifestNodeBody>,
}

/// A control a module registers for one pin type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp-tool-json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeUiManifestControl {
    /// The pin type whose inputs the control edits or shows.
    #[serde(rename = "type")]
    pub pin_type: BuiltInPinTypeName,
    /// The control's name, which a pin's `control` annotation picks it by. Left out, the control
    /// is its pin type's default.
    #[serde(
        default,
        deserialize_with = "a_value_never_written_as_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "mcp-tool-json-schema", schemars(with = "String"))]
    pub control: Option<String>,
    /// The script that default-exports the control's element class, inside the node UI folder.
    pub script: String,
}

/// A node body a module registers for one node type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "mcp-tool-json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeUiManifestNodeBody {
    /// The node type whose node it draws, as `acme:Crossfade`.
    pub node_type: String,
    /// The script that default-exports the node body's element class, inside the node UI folder.
    pub script: String,
}

/// `manifest_json`, a module's `node_ui/tatolab_node_ui.json` as parsed, read as a
/// [`NodeUiManifest`], or every way it falls short, each as "`field`: why.".
pub fn read_node_ui_manifest(manifest_json: &Value) -> Result<NodeUiManifest, String> {
    let manifest: NodeUiManifest = serde_path_to_error::deserialize(manifest_json.clone())
        .map_err(|not_the_shape| {
            let field_path =
                the_field_a_serde_refusal_names(&not_the_shape, NODE_UI_MANIFEST_FILE_NAME);
            format!("`{field_path}`: {}.", not_the_shape.into_inner())
        })?;
    let issues = every_issue_of_a_node_ui_manifest(&manifest);
    if issues.is_empty() {
        Ok(manifest)
    } else {
        Err(issues.join(" "))
    }
}

fn every_issue_of_a_node_ui_manifest(manifest: &NodeUiManifest) -> Vec<String> {
    let mut issues = Vec::new();
    if !is_a_contract_version_a_node_ui_manifest_is_read_against(&manifest.contract) {
        issues.push(format!(
            "`contract`: it names the view contract the elements were built for, and this runtime \
             reads node UI elements of view contract {NODE_UI_CONTRACT_READ_MAJOR}.\
             {NODE_UI_CONTRACT_READ_MINOR}."
        ));
    }
    let scripts = manifest
        .controls
        .iter()
        .map(|control| &control.script)
        .enumerate()
        .map(|(index, script)| (format!("controls[{index}].script"), script))
        .chain(
            manifest
                .node_bodies
                .iter()
                .enumerate()
                .map(|(index, body)| (format!("node_bodies[{index}].script"), &body.script)),
        );
    for (field_path, script) in scripts {
        if !is_a_path_inside_the_views_folder(script) {
            issues.push(format!(
                "`{field_path}`: it is a path inside the node UI folder, with no `..` step, scheme \
                 or backslash."
            ));
        }
    }
    issues
}

fn is_a_contract_version_a_node_ui_manifest_is_read_against(contract: &str) -> bool {
    is_a_contract_version(contract)
        && is_a_contract_version_of_major_up_to_minor(
            contract,
            NODE_UI_CONTRACT_READ_MAJOR,
            NODE_UI_CONTRACT_READ_MINOR,
        )
}

#[cfg(test)]
mod tests;
