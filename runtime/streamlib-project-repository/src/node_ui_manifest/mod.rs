// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `node_ui/tatolab_node_ui.json`, the controls and node bodies a module registers, read against
//! the view contract. Its shape is the contract's [`NodeUiManifest`].

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeUiManifest;

use crate::manifest_fields::{
    is_a_contract_version, is_a_contract_version_of_major_up_to_minor,
    is_a_path_inside_its_own_folder, the_field_a_serde_refusal_names,
};
use crate::tatolab_view_manifest::VIEW_CONTRACT_READ_MAJOR;

/// The folder of a module that holds its node UI elements.
pub const NODE_UI_DIRECTORY: &str = "node_ui";

/// The manifest the node UI folder holds.
pub const NODE_UI_MANIFEST_FILE_NAME: &str = "tatolab_node_ui.json";

/// The highest minor of [`VIEW_CONTRACT_READ_MAJOR`] a node UI manifest is read against: a
/// manifest that names a later minor, or another major, does not open.
pub const NODE_UI_CONTRACT_READ_MINOR: u64 = 1;

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
             reads node UI elements of view contract {VIEW_CONTRACT_READ_MAJOR}.\
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
        if !is_a_path_inside_its_own_folder(script) {
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
            VIEW_CONTRACT_READ_MAJOR,
            NODE_UI_CONTRACT_READ_MINOR,
        )
}

#[cfg(test)]
mod tests;
