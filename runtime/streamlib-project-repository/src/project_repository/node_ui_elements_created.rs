// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeUiControlPinType, NodeUiManifest, NodeUiManifestControl, NodeUiManifestNodeBody,
    ProjectNodeUiElement,
};

use super::project_files::FileToWrite;
use super::*;
use crate::node_ui_manifest::{
    NODE_UI_CONTRACT_READ_MINOR, NODE_UI_DIRECTORY, NODE_UI_MANIFEST_FILE_NAME,
};
use crate::project_node_ui_elements::the_node_ui_manifest_file_read;
use crate::tatolab_view_manifest::VIEW_CONTRACT_READ_MAJOR;

impl ProjectRepository {
    /// Write a new node UI element's `script_content` at `node_ui/<script>` and register
    /// `element` with it in the project's `node_ui/tatolab_node_ui.json`, created when missing,
    /// as one transaction by `author` that one undo reverses. Refused where a file already is
    /// at the script's path, when the manifest does not read, and when it already registers an
    /// element for the same pin type and name, or the same node type.
    pub fn create_node_ui_element(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        element: &ProjectNodeUiElement,
        script: &str,
        script_content: &[u8],
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let script_path = ProjectRelativeFilePath::parse(&format!("{NODE_UI_DIRECTORY}/{script}"))?;
        let manifest_path = ProjectRelativeFilePath::parse(&format!(
            "{NODE_UI_DIRECTORY}/{NODE_UI_MANIFEST_FILE_NAME}"
        ))?;
        let not_registered = |why: String| ProjectRepositoryRefusal::NodeUiElementNotRegistered {
            path: manifest_path.as_str().to_owned(),
            why,
        };
        if script_path == manifest_path {
            return Err(not_registered(format!(
                "`{NODE_UI_MANIFEST_FILE_NAME}` is the manifest's own name; name the script \
                 another, such as `knob.ts`"
            )));
        }
        if let ProjectNodeUiElement::Control {
            pin_type: NodeUiControlPinType::BuiltIn(_),
            control: None,
        } = element
        {
            return Err(not_registered(
                "a control with no `control` name is its pin type's default control, which only \
                 the Studio's own module registers for a built-in type; give it a name, such as \
                 `knob`"
                    .to_owned(),
            ));
        }
        let assets = self.every_asset()?;
        for path in [&script_path, &manifest_path] {
            self.refuse_a_file_another_action_writes(&assets, path)?;
        }
        if script_path
            .on_disk_under(&self.project_directory)
            .symlink_metadata()
            .is_ok()
        {
            return Err(ProjectRepositoryRefusal::ProjectFileAlreadyExists(
                script_path.as_str().to_owned(),
            ));
        }
        let manifest_on_disk = manifest_path.on_disk_under(&self.project_directory);
        let manifest_read = the_node_ui_manifest_file_read(&manifest_on_disk);
        let (manifest, bytes_on_disk_now) = match manifest_read {
            None => (
                NodeUiManifest {
                    contract: format!("{VIEW_CONTRACT_READ_MAJOR}.{NODE_UI_CONTRACT_READ_MINOR}"),
                    controls: Vec::new(),
                    node_bodies: Vec::new(),
                },
                None,
            ),
            Some(read) => (read.manifest.map_err(not_registered)?, read.bytes),
        };
        let manifest =
            the_manifest_registering(manifest, element, script).map_err(not_registered)?;
        let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        manifest_bytes.push(b'\n');
        let replaced_the_manifest = bytes_on_disk_now.is_some();
        self.write_project_files_as_one_transaction(
            author,
            idempotency_key,
            vec![
                FileToWrite {
                    path: script_path,
                    content: script_content,
                    bytes_on_disk_now: None,
                    replaced_a_file: false,
                },
                FileToWrite {
                    path: manifest_path,
                    content: &manifest_bytes,
                    bytes_on_disk_now,
                    replaced_a_file: replaced_the_manifest,
                },
            ],
        )
    }
}

/// `manifest` with `element` registered at `script`, or why it does not take it: it already
/// registers an element for the same pin type and name, or the same node type.
fn the_manifest_registering(
    mut manifest: NodeUiManifest,
    element: &ProjectNodeUiElement,
    script: &str,
) -> Result<NodeUiManifest, String> {
    match element {
        ProjectNodeUiElement::Control { pin_type, control } => {
            if let Some(registered) = manifest.controls.iter().find(|registered| {
                registered.pin_type == *pin_type && registered.control == *control
            }) {
                return Err(format!(
                    "it already registers {} in `{}`",
                    match control {
                        Some(control) =>
                            format!("the control `{control}` for `{}`", pin_type.as_written()),
                        None => format!("the default control for `{}`", pin_type.as_written()),
                    },
                    registered.script
                ));
            }
            manifest.controls.push(NodeUiManifestControl {
                pin_type: pin_type.clone(),
                control: control.clone(),
                script: script.to_owned(),
            });
        }
        ProjectNodeUiElement::NodeBody { node_type } => {
            if let Some(registered) = manifest
                .node_bodies
                .iter()
                .find(|registered| registered.node_type == *node_type)
            {
                return Err(format!(
                    "it already registers a node body for `{node_type}` in `{}`",
                    registered.script
                ));
            }
            manifest.node_bodies.push(NodeUiManifestNodeBody {
                node_type: node_type.clone(),
                script: script.to_owned(),
            });
        }
    }
    Ok(manifest)
}
