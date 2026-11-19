// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The project's own `node_ui/tatolab_node_ui.json` as the `extension_manifest` check of
//! `check_project_code` reads it: a manifest that does not read, and each entry that registers
//! what its module may not.

use std::collections::BTreeSet;
use std::path::Path;

use streamlib_node_schema::pin_type::built_in_pin_type;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeUiManifest, ProjectCodeCheckFinding, ProjectCodeCheckKind, ProjectCodeCheckSeverity,
    ProjectFileChecked,
};

use crate::node_ui_manifest::{NODE_UI_DIRECTORY, NODE_UI_MANIFEST_FILE_NAME};
use crate::project_asset_file_bytes::content_sha256_of;
use crate::project_node_ui_elements::{NodeUiManifestFileRead, the_node_ui_manifest_file_read};

/// The project's own node UI manifest, read for the `extension_manifest` check.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeUiManifestToCheck {
    /// The manifest read against the view contract, or why it does not read.
    pub manifest: Result<NodeUiManifest, String>,
    /// The manifest's file and the hash of the bytes read; `None` when its bytes could not be
    /// read.
    pub file_checked: Option<ProjectFileChecked>,
}

impl NodeUiManifestToCheck {
    /// Whether the manifest reads and registers a node body, which is judged against the node
    /// types the project brings.
    pub fn registers_a_node_body(&self) -> bool {
        self.manifest
            .as_ref()
            .is_ok_and(|manifest| !manifest.node_bodies.is_empty())
    }
}

/// The path, in a project, of its own node UI manifest.
pub fn the_path_of_a_projects_own_node_ui_manifest() -> String {
    format!("{NODE_UI_DIRECTORY}/{NODE_UI_MANIFEST_FILE_NAME}")
}

/// The own `node_ui/tatolab_node_ui.json` of the project at `project_directory`, when it has one
/// and `paths`, each a path in the project, names it or is `None`.
pub fn the_node_ui_manifest_a_check_reads(
    project_directory: &Path,
    paths: Option<&[String]>,
) -> Option<NodeUiManifestToCheck> {
    let manifest_path = the_path_of_a_projects_own_node_ui_manifest();
    if paths.is_some_and(|paths| !paths.contains(&manifest_path)) {
        return None;
    }
    let NodeUiManifestFileRead { bytes, manifest } =
        the_node_ui_manifest_file_read(&project_directory.join(&manifest_path))?;
    Some(NodeUiManifestToCheck {
        manifest,
        file_checked: bytes.map(|bytes| ProjectFileChecked {
            path: manifest_path,
            content_sha256: content_sha256_of(&bytes),
        }),
    })
}

/// What the `extension_manifest` check finds in `to_check`: an `error` when it does not read,
/// and one for each entry its module may not register. A built-in type's default control is the
/// Studio's own module's to register, so a project's is always refused; a node body is its node
/// type's module's, judged against `node_types_the_project_brings`, the node types the project's
/// own code declares, and not judged when that is `None`.
pub fn the_findings_of_a_node_ui_manifest(
    to_check: &NodeUiManifestToCheck,
    node_types_the_project_brings: Option<&BTreeSet<String>>,
) -> Vec<ProjectCodeCheckFinding> {
    let finding =
        |finding_code: &str, words: String, node_type: Option<&str>| ProjectCodeCheckFinding {
            check: ProjectCodeCheckKind::ExtensionManifest,
            finding_code: finding_code.to_owned(),
            severity: ProjectCodeCheckSeverity::Error,
            words,
            path: the_path_of_a_projects_own_node_ui_manifest(),
            line: None,
            column: None,
            node_type: node_type.map(str::to_owned),
            port: None,
        };
    let manifest = match &to_check.manifest {
        Ok(manifest) => manifest,
        Err(why) => {
            return vec![finding(
                "node_ui_manifest_refused",
                format!("the project's node UI elements are not offered: {why}"),
                None,
            )];
        }
    };
    let mut findings = Vec::new();
    for (index, control) in manifest.controls.iter().enumerate() {
        if control.control.is_none() {
            let type_name = built_in_pin_type(control.pin_type)
                .typescript_form
                .trim_matches('"');
            findings.push(finding(
                "default_control_not_the_modules_to_register",
                format!(
                    "`controls[{index}]` registers the default control of `{type_name}`, a \
                     built-in type: only the Studio's own module may register it. Give it a \
                     `\"control\"` name, which a pin's `control` annotation picks it by"
                ),
                None,
            ));
        }
    }
    if let Some(node_types) = node_types_the_project_brings {
        for (index, body) in manifest.node_bodies.iter().enumerate() {
            if !node_types.contains(&body.node_type) {
                findings.push(finding(
                    "node_body_not_the_modules_to_register",
                    format!(
                        "`node_bodies[{index}]` registers a node body for `{}`, which is no node \
                         type of the project's own code: only the module that brings a node type \
                         may register its body. Name one of the project's own node types as \
                         `list_project_node_types` lists it",
                        body.node_type
                    ),
                    Some(&body.node_type),
                ));
            }
        }
    }
    findings
}

/// Whether `path`, a path in a project, is its own node UI manifest, which the
/// `extension_manifest` check reads and no other check does.
pub fn is_a_projects_own_node_ui_manifest(path: &str) -> bool {
    path == the_path_of_a_projects_own_node_ui_manifest()
}

#[cfg(test)]
mod tests;
