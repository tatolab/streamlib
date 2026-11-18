// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operations on the Tato Studio views a project brings: listed, and their values written
//! into a running stream.

use std::path::Path;

use streamlib_project_repository::{
    ProjectViewFolderRead, StreamGraphAction, TatolabViewDeclaredPort,
    every_view_a_project_and_its_imports_bring,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectViewsToolResult, ProjectViewDeclaredPort, ProjectViewListed,
    WriteGraphInputToolResult, WriteStudioViewOutputToolResult,
};

use crate::control_operation_dispatcher::the_refusal_of_a_reach_for_a_node;
use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
};

fn the_declared_port_on_the_wire(port: &TatolabViewDeclaredPort) -> ProjectViewDeclaredPort {
    ProjectViewDeclaredPort {
        name: port.name.clone(),
        family: port.family.wire_name().to_owned(),
        description: port.description.clone(),
        range: port.range,
    }
}

fn the_view_on_the_wire(
    project_directory: &Path,
    view: ProjectViewFolderRead,
) -> ProjectViewListed {
    let (manifest, not_opened_because) = match view.manifest {
        Ok(manifest) => (Some(manifest), None),
        Err(why) => (None, Some(why)),
    };
    let declared_ports = |ports: Option<&Vec<TatolabViewDeclaredPort>>| {
        ports
            .map(|ports| ports.iter().map(the_declared_port_on_the_wire).collect())
            .unwrap_or_default()
    };
    ProjectViewListed {
        view_type: view.view_type,
        project: view.brought_by.project.to_string(),
        project_name: view.brought_by.project_name,
        import_folder: view.brought_by.import_folder,
        view_folder_in_the_project: view
            .folder
            .strip_prefix(project_directory)
            .unwrap_or(&view.folder)
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        opens: manifest.is_some(),
        not_opened_because,
        title: manifest.as_ref().map(|manifest| manifest.title.clone()),
        description: manifest
            .as_ref()
            .and_then(|manifest| manifest.description.clone()),
        icon: manifest.as_ref().and_then(|manifest| manifest.icon.clone()),
        preview: manifest
            .as_ref()
            .and_then(|manifest| manifest.preview.clone()),
        placement: manifest
            .as_ref()
            .map(|manifest| manifest.placement.manifest_name().to_owned()),
        inputs: declared_ports(manifest.as_ref().map(|manifest| &manifest.inputs)),
        outputs: declared_ports(manifest.as_ref().map(|manifest| &manifest.outputs)),
        manifest: view.manifest_as_written,
    }
}

use streamlib_control_contract::{
    ControlOperation, GraphEditMode, ListProjectViewsRequest, WriteGraphInput,
    WriteGraphInputRequest, WriteStudioViewOutputRequest,
};
impl ControlOperationDispatcher {
    pub(crate) async fn list_project_views(
        &self,
        ListProjectViewsRequest { project_directory }: ListProjectViewsRequest,
    ) -> Result<ListProjectViewsToolResult, ControlOperationRefusal> {
        let listed = self
            .a_project_repository_call_off_the_async_runtime(
                "list_project_views",
                project_directory,
                |project_repository| {
                    let project_directory = project_repository.project_directory().to_path_buf();
                    Ok(ListProjectViewsToolResult {
                        views: every_view_a_project_and_its_imports_bring(&project_directory)?
                            .into_iter()
                            .map(|view| the_view_on_the_wire(&project_directory, view))
                            .collect(),
                        project_directory,
                        project: project_repository.project_file().project.to_string(),
                        project_name: project_repository.project_file().name.clone(),
                    })
                },
            )
            .await?;
        Ok(listed)
    }

    pub(crate) async fn write_studio_view_output(
        &self,
        WriteStudioViewOutputRequest {
            stream,
            node,
            port,
            value,
        }: WriteStudioViewOutputRequest,
    ) -> Result<WriteStudioViewOutputToolResult, ControlOperationRefusal> {
        let stream_operations =
            self.the_stream_a_call_names("write_studio_view_output", &stream)?;
        let written = stream_operations
            .write_a_value_onto_an_output_of_a_view_node(node.clone(), port.clone(), value)
            .await;
        if let Err(refusal) = written {
            return Err(the_refusal_of_a_reach_for_a_node(
                "write_studio_view_output",
                refusal,
            ));
        }
        Ok(WriteStudioViewOutputToolResult { stream, node, port })
    }

    /// `write_graph_input`, replaced by `set_node_input_value` naming no node: the one live set
    /// of the `value` input of the top-level `GraphInput` declaring the stream input `input`.
    pub(crate) async fn write_graph_input(
        &self,
        WriteGraphInputRequest {
            stream,
            input,
            value,
        }: WriteGraphInputRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<WriteGraphInputToolResult, ControlOperationRefusal> {
        const ACTION_NAME: &str = <WriteGraphInput as ControlOperation>::METHOD_NAME;
        let stream_operations = self.the_stream_a_call_names(ACTION_NAME, &stream)?;
        let graph_input = stream_operations
            .the_top_level_graph_input_declaring(input.clone())
            .await
            .map_err(|refusal| the_refusal_of_a_reach_for_a_node(ACTION_NAME, refusal))?;
        if let Some(refusal) = graph_input.refusal_while_its_node_does_not_run {
            return Err(the_refusal_of_a_reach_for_a_node(ACTION_NAME, refusal));
        }
        let stream_asset = self
            .operations_on_the_loaded_streams
            .list_streams()
            .into_iter()
            .filter(|listing| listing.stream_id == stream)
            .find_map(|listing| Some(listing.stream_location?.stream_asset))
            .ok_or_else(|| {
                ControlOperationRefusal::Failed(format!(
                    "{ACTION_NAME} failed: the stream `{stream}` runs no stream asset of a \
                     project, so it has no graph to set the input `{input}` on"
                ))
            })?;
        self.apply_a_graph_transaction(
            ACTION_NAME,
            vec![StreamGraphAction::SetNodeInputValue {
                stream: stream_asset,
                level: Vec::new(),
                node: None,
                input: input.clone(),
                value: Some(value),
            }],
            Some(GraphEditMode::Live),
            None,
            None,
            request_origin,
        )
        .await?;
        Ok(WriteGraphInputToolResult {
            stream,
            input,
            family: graph_input.family.wire_name().to_owned(),
        })
    }
}
