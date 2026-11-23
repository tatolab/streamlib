// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `extension_manifest` check of `check_project_code`: each `tatolab_view.json` of a project's
//! own views, read as the runtime reads it when the view opens.

use std::path::Path;

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ProjectCodeCheckFinding, ProjectCodeCheckKind, ProjectCodeCheckRun, ProjectCodeCheckSeverity,
    ProjectCodeCheckState, ProjectFileChecked,
};

use crate::content_sha256::content_sha256_of;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_views::{
    PROJECT_VIEWS_DIRECTORY, TATOLAB_VIEW_MANIFEST_FILE_NAME, every_view_a_project_brings_itself,
};
use crate::tatolab_view_manifest::read_tatolab_view_manifest;

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
/// runtime does not open is an `error`, in the words the runtime refuses it in. An import's
/// views are its own project's to fix, and are left out.
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
        let refusal = |finding_code: &str, words: String| ProjectCodeCheckFinding {
            check: ProjectCodeCheckKind::ExtensionManifest,
            finding_code: finding_code.to_owned(),
            severity: ProjectCodeCheckSeverity::Error,
            words,
            path: manifest_path.clone(),
            line: None,
            column: None,
            node_type: Some(view.view_type.clone()),
            port: None,
        };
        let bytes = match std::fs::read(view.folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME)) {
            Ok(bytes) => bytes,
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
                findings.push(refusal(
                    "view_manifest_missing",
                    format!(
                        "the view's folder `{PROJECT_VIEWS_DIRECTORY}/{view_folder_name}` has no \
                         `{TATOLAB_VIEW_MANIFEST_FILE_NAME}`, so the view does not open"
                    ),
                ));
                continue;
            }
            Err(unreadable) => {
                findings.push(refusal(
                    "view_manifest_refused",
                    format!(
                        "the view does not open: `{TATOLAB_VIEW_MANIFEST_FILE_NAME}` cannot be \
                         read: {unreadable}"
                    ),
                ));
                continue;
            }
        };
        files_checked.push(ProjectFileChecked {
            path: manifest_path.clone(),
            content_sha256: content_sha256_of(&bytes),
        });
        if let Err(why) = serde_json::from_slice::<Value>(&bytes)
            .map_err(|not_json| format!("it is not JSON: {not_json}"))
            .and_then(|manifest_json| read_tatolab_view_manifest(&manifest_json))
        {
            findings.push(refusal(
                "view_manifest_refused",
                format!("the view does not open: {why}"),
            ));
        }
    }
    let state = if findings.is_empty() {
        ProjectCodeCheckState::Passed
    } else {
        ProjectCodeCheckState::Failed
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

#[cfg(test)]
mod tests;
