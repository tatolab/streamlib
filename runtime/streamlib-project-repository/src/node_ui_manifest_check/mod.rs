// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The project's own `node_ui/tatolab_node_ui.json` as the `extension_manifest` check of
//! `check_project_code` reads it: a manifest that does not read, and each entry that registers
//! what its module may not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectNodeTypesToolResult, NodeTypeSourceOrigin, NodeUiControlPinType, NodeUiManifest,
    ProjectCodeCheckFinding, ProjectCodeCheckKind, ProjectCodeCheckSeverity, ProjectFileChecked,
};

use crate::content_sha256::content_sha256_of;
use crate::node_ui_manifest::{NODE_UI_DIRECTORY, NODE_UI_MANIFEST_FILE_NAME};
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
    /// Whether the manifest reads and registers an entry judged against what the project's
    /// modules declare: a node body, or a named type's default control.
    pub fn is_judged_against_what_the_projects_modules_declare(&self) -> bool {
        self.manifest.as_ref().is_ok_and(|manifest| {
            !manifest.node_bodies.is_empty()
                || manifest.controls.iter().any(|control| {
                    control.control.is_none()
                        && matches!(control.pin_type, NodeUiControlPinType::Named(_))
                })
        })
    }
}

/// What the project's code and the modules it brings declare, as the node type listing reads
/// it from source, which a node body and a named type's default control are judged against.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WhatTheProjectsModulesDeclare {
    /// Each node type the project's own code declares.
    pub own_node_types: BTreeSet<String>,
    /// Each named pin type a module the project brings declares, by its import path, with the
    /// module that declares it.
    pub module_declaring_each_named_pin_type: BTreeMap<String, ModuleThatDeclares>,
}

/// The module that declares a named pin type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleThatDeclares {
    /// The project's own code, outside `imports/`.
    TheProjectsOwnCode,
    /// An import or an installed package, named in words, as ``the import `rig` ``.
    AnotherModule(String),
}

impl WhatTheProjectsModulesDeclare {
    /// What `listing`, the project's node type listing read from source, says its modules
    /// declare.
    pub fn read_off(listing: &ListProjectNodeTypesToolResult) -> Self {
        let mut declared = Self::default();
        for source in &listing.sources {
            let module = match &source.origin {
                NodeTypeSourceOrigin::Project => {
                    declared
                        .own_node_types
                        .extend(source.node_types.iter().map(|read| read.node_type.clone()));
                    ModuleThatDeclares::TheProjectsOwnCode
                }
                NodeTypeSourceOrigin::Dependency {
                    folder,
                    project_name,
                    ..
                } => ModuleThatDeclares::AnotherModule(format!(
                    "the import `{}` (`imports/{folder}`)",
                    project_name.as_deref().unwrap_or(folder)
                )),
                NodeTypeSourceOrigin::InstalledDistribution { distribution, .. } => {
                    ModuleThatDeclares::AnotherModule(format!(
                        "the installed package `{distribution}`"
                    ))
                }
            };
            for named in &source.named_pin_types {
                declared
                    .module_declaring_each_named_pin_type
                    .entry(named.named_pin_type.as_str().to_owned())
                    .or_insert_with(|| module.clone());
            }
        }
        declared
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
/// Studio's own module's to register, so a project's is always refused. A named type's default
/// control is the declaring module's, and a node body its node type's module's, each judged
/// against `declared`, what the project's modules declare, and not judged when that is `None`.
pub fn the_findings_of_a_node_ui_manifest(
    to_check: &NodeUiManifestToCheck,
    declared: Option<&WhatTheProjectsModulesDeclare>,
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
        if control.control.is_some() {
            continue;
        }
        let type_name = control.pin_type.as_written();
        let who_may_register_it = match &control.pin_type {
            NodeUiControlPinType::BuiltIn(_) => Some(format!(
                "`{type_name}`, a built-in type: only the Studio's own module may register it"
            )),
            NodeUiControlPinType::Named(_) => declared.and_then(|declared| {
                match declared
                    .module_declaring_each_named_pin_type
                    .get(&type_name)
                {
                    Some(ModuleThatDeclares::TheProjectsOwnCode) => None,
                    Some(ModuleThatDeclares::AnotherModule(module)) => Some(format!(
                        "`{type_name}`, a named type {module} declares: only the module that \
                         declares a named type may register its default, so register it in \
                         {module}"
                    )),
                    None => Some(format!(
                        "`{type_name}`, a named type no module the project brings declares: only \
                         the module that declares a named type may register its default, so \
                         declare the class with `@named_pin_type` in the project's own code"
                    )),
                }
            }),
        };
        if let Some(who_may_register_it) = who_may_register_it {
            findings.push(finding(
                "default_control_not_the_modules_to_register",
                format!(
                    "`controls[{index}]` registers the default control of {who_may_register_it}. \
                     Otherwise give it a `\"control\"` name, which a pin's `control` annotation \
                     picks it by"
                ),
                None,
            ));
        }
    }
    if let Some(declared) = declared {
        for (index, body) in manifest.node_bodies.iter().enumerate() {
            if !declared.own_node_types.contains(&body.node_type) {
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

#[cfg(test)]
mod tests;
