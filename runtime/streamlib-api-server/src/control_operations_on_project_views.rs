// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The operations on the Tato Studio views a project brings: listed, and their values written
//! into a running stream.

use std::path::Path;

use streamlib_project_repository::{
    ProjectViewFolderRead, TatolabViewDeclaredPort, every_view_a_project_and_its_imports_bring,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectViewsToolResult, ProjectViewDeclaredPort, ProjectViewListed,
    WriteGraphInputToolResult, WriteStudioViewOutputToolResult,
};

use crate::control_operation_dispatcher::the_refusal_of_a_reach_for_a_node;
use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};

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
    ListProjectViewsRequest, WriteGraphInputRequest, WriteStudioViewOutputRequest,
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

    pub(crate) async fn write_graph_input(
        &self,
        WriteGraphInputRequest {
            stream,
            input,
            value,
        }: WriteGraphInputRequest,
    ) -> Result<WriteGraphInputToolResult, ControlOperationRefusal> {
        let stream_operations = self.the_stream_a_call_names("write_graph_input", &stream)?;
        let family = stream_operations
            .write_a_value_to_a_top_level_graph_input(input.clone(), value)
            .await;
        let family = match family {
            Ok(family) => family,
            Err(refusal) => {
                return Err(the_refusal_of_a_reach_for_a_node(
                    "write_graph_input",
                    refusal,
                ));
            }
        };
        Ok(WriteGraphInputToolResult {
            stream,
            input,
            family: family.wire_name().to_owned(),
        })
    }
}
