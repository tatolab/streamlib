// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `extension_manifest` check of `check_project_code`: each `tatolab_view.json` of a project's
//! own views, read as the runtime reads it when the view opens, and each port it still declares
//! by the older `family` field.

use std::path::Path;

use serde_json::Value;
use streamlib_node_schema::pin_type::{BuiltInPinTypeName, built_in_pin_type};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ProjectCodeCheckFinding, ProjectCodeCheckKind, ProjectCodeCheckRun, ProjectCodeCheckSeverity,
    ProjectCodeCheckState, ProjectFileChecked,
};

use crate::project_asset_file_bytes::content_sha256_of;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_views::{
    PROJECT_VIEWS_DIRECTORY, TATOLAB_VIEW_MANIFEST_FILE_NAME, every_view_a_project_brings_itself,
};
use crate::tatolab_view_manifest::{
    TatolabViewDeclaredPort, VIEW_CONTRACT_READ_MAJOR, VIEW_CONTRACT_READ_MINOR,
    read_tatolab_view_manifest,
};

/// What the `extension_manifest` check found: how it ended, each finding, and each manifest it
/// read, with the hash of its bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewManifestsChecked {
    pub run: ProjectCodeCheckRun,
    pub findings: Vec<ProjectCodeCheckFinding>,
    pub files_checked: Vec<ProjectFileChecked>,
}

/// Check the `tatolab_view.json` of each view in the own `views/` of the project at
/// `project_directory`, or of those among `paths`, each a path in the project: a manifest the
/// runtime does not open is an `error`, and a port it declares by `family` alone a `warning`
/// naming the `pin_type` to write. An import's views are its own project's to fix, and are
/// left out.
pub fn check_a_projects_view_manifests(
    project_directory: &Path,
    paths: Option<&[String]>,
) -> Result<ViewManifestsChecked, ProjectRepositoryRefusal> {
    let mut findings = Vec::new();
    let mut files_checked = Vec::new();
    for view in every_view_a_project_brings_itself(project_directory)? {
        let Some(view_folder_name) = view.folder.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let manifest_path = format!(
            "{PROJECT_VIEWS_DIRECTORY}/{view_folder_name}/{TATOLAB_VIEW_MANIFEST_FILE_NAME}"
        );
        if paths.is_some_and(|paths| !paths.contains(&manifest_path)) {
            continue;
        }
        let finding = |finding_code: &str,
                       severity: ProjectCodeCheckSeverity,
                       words: String,
                       port: Option<&str>| ProjectCodeCheckFinding {
            check: ProjectCodeCheckKind::ExtensionManifest,
            finding_code: finding_code.to_owned(),
            severity,
            words,
            path: manifest_path.clone(),
            line: None,
            column: None,
            node_type: Some(view.view_type.clone()),
            port: port.map(str::to_owned),
        };
        let bytes = match std::fs::read(view.folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME)) {
            Ok(bytes) => bytes,
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                findings.push(finding(
                    "view_manifest_missing",
                    ProjectCodeCheckSeverity::Error,
                    format!(
                        "the view's folder `{PROJECT_VIEWS_DIRECTORY}/{view_folder_name}` has no \
                         `{TATOLAB_VIEW_MANIFEST_FILE_NAME}`, so the view does not open"
                    ),
                    None,
                ));
                continue;
            }
            Err(unreadable) => {
                findings.push(finding(
                    "view_manifest_refused",
                    ProjectCodeCheckSeverity::Error,
                    format!(
                        "the view does not open: `{TATOLAB_VIEW_MANIFEST_FILE_NAME}` cannot be \
                         read: {unreadable}"
                    ),
                    None,
                ));
                continue;
            }
        };
        files_checked.push(ProjectFileChecked {
            path: manifest_path.clone(),
            content_sha256: content_sha256_of(&bytes),
        });
        let manifest = serde_json::from_slice::<Value>(&bytes)
            .map_err(|not_json| format!("it is not JSON: {not_json}"))
            .and_then(|manifest_json| read_tatolab_view_manifest(&manifest_json));
        let manifest = match manifest {
            Ok(manifest) => manifest,
            Err(why) => {
                findings.push(finding(
                    "view_manifest_refused",
                    ProjectCodeCheckSeverity::Error,
                    format!("the view does not open: {why}"),
                    None,
                ));
                continue;
            }
        };
        for port in manifest.inputs.iter().chain(&manifest.outputs) {
            if port.pin_type.is_none() {
                findings.push(finding(
                    "port_declared_by_family",
                    ProjectCodeCheckSeverity::Warning,
                    the_words_of_a_port_declared_by_family(port),
                    Some(&port.name),
                ));
            }
        }
    }
    let state = if findings
        .iter()
        .any(|finding| finding.severity == ProjectCodeCheckSeverity::Error)
    {
        ProjectCodeCheckState::Failed
    } else {
        ProjectCodeCheckState::Passed
    };
    Ok(ViewManifestsChecked {
        run: ProjectCodeCheckRun {
            check: ProjectCodeCheckKind::ExtensionManifest,
            state,
            not_checked_because: None,
            checked_with: Vec::new(),
        },
        findings,
        files_checked,
    })
}

/// Whether `path`, a path in a project, is the manifest of one of its own views, which the
/// `extension_manifest` check reads and no other check does.
pub fn is_the_manifest_of_a_projects_own_view(path: &str) -> bool {
    matches!(
        path.split('/').collect::<Vec<_>>().as_slice(),
        [PROJECT_VIEWS_DIRECTORY, view_folder_name, TATOLAB_VIEW_MANIFEST_FILE_NAME]
            if !view_folder_name.is_empty()
    )
}

/// What a port declared by `family` alone writes in its place: the one pin type the family's
/// messages carry where there is one, as an Image port's Texture; else the types it may become,
/// none picked, since a guessed type can refuse a link that works today.
fn the_words_of_a_port_declared_by_family(port: &TatolabViewDeclaredPort) -> String {
    let family = port.family.wire_name();
    let to_write = format!(
        "in place of `\"family\": \"{family}\"`, with `\"contract\": \
         \"{VIEW_CONTRACT_READ_MAJOR}.{VIEW_CONTRACT_READ_MINOR}\"`"
    );
    let pin_types: Vec<&str> = port
        .family
        .the_pin_types_it_carries()
        .into_iter()
        .map(the_quoted_wire_name_of)
        .collect();
    match pin_types.as_slice() {
        [pin_type] => format!(
            "port `{}` is declared by the older `family` field; write `\"pin_type\": \
             {{\"type\": {pin_type}}}` {to_write}",
            port.name
        ),
        _ => format!(
            "port `{}` is declared by the older `family` field; a `{family}` port carries one \
             of {}: write the one it carries as its `pin_type`, as `{{\"type\": \"<type>\"}}`, \
             {to_write}",
            port.name,
            pin_types
                .iter()
                .map(|pin_type| format!("`{}`", pin_type.trim_matches('"')))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The name `name` is written by in a pin type's `type`, quoted, from the contract's one list.
fn the_quoted_wire_name_of(name: BuiltInPinTypeName) -> &'static str {
    built_in_pin_type(name).typescript_form
}

#[cfg(test)]
mod tests;
