// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operations on the Tato Studio views a project brings: listed, and their values written
//! into a running stream.

use std::path::Path;

use streamlib::sdk::runtime::TopLevelGraphInputDeclaringAStreamInput;
use streamlib_project_repository::{
    GraphEditStream, ProjectNodeUiElementsListed, ProjectViewFolderRead, TatolabViewDeclaredPort,
    every_node_ui_element_a_project_and_its_imports_bring,
    every_view_a_project_and_its_imports_bring,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ControlOperationRefusalOnTheWire,
    ListProjectNodeUiElementsToolResult, ListProjectViewsToolResult, ProjectViewDeclaredPort,
    ProjectViewListed,
};

use uuid::Uuid;

use crate::control_operation_dispatcher::the_refusal_of_a_reach_for_a_node;
use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::refusal_code_and_place::RefusalCodeAndPlace;
use crate::stream_graph_transaction_executor::StreamsTheRuntimeHolds;

fn the_declared_port_on_the_wire(port: &TatolabViewDeclaredPort) -> ProjectViewDeclaredPort {
    ProjectViewDeclaredPort {
        name: port.name.clone(),
        pin_type: Some(port.pin_type.clone()),
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
    ControlOperation, ListProjectNodeUiElements, ListProjectNodeUiElementsRequest,
    ListProjectViewsRequest,
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

    pub(crate) async fn list_project_node_ui_elements(
        &self,
        ListProjectNodeUiElementsRequest { project_directory }: ListProjectNodeUiElementsRequest,
    ) -> Result<ListProjectNodeUiElementsToolResult, ControlOperationRefusal> {
        self.a_project_repository_call_off_the_async_runtime(
            ListProjectNodeUiElements::METHOD_NAME,
            project_directory,
            |project_repository| {
                let project_directory = project_repository.project_directory().to_path_buf();
                let ProjectNodeUiElementsListed {
                    elements,
                    manifests,
                    unreadable_manifests,
                } = every_node_ui_element_a_project_and_its_imports_bring(&project_directory)?;
                Ok(ListProjectNodeUiElementsToolResult {
                    project_directory,
                    project: project_repository.project_file().project.to_string(),
                    project_name: project_repository.project_file().name.clone(),
                    elements,
                    manifests,
                    unreadable_manifests,
                })
            },
        )
        .await
    }

    /// The node a `set_node_input_value` naming no node sets the `value` input of, in the stream
    /// `stream` names at `level`: the top-level `GraphInput` declaring the stream input `input`.
    /// Refused below the top level, where no stream input is declared, for a stream the runtime
    /// does not hold, and for an input no top-level `GraphInput` declares, naming the stream's
    /// inputs.
    pub(crate) async fn the_node_a_set_naming_no_node_sets(
        &self,
        action_name: &str,
        stream: &GraphEditStream,
        level: &[Uuid],
        input: &str,
    ) -> Result<String, ControlOperationRefusal> {
        if !level.is_empty() {
            return Err(ControlOperationRefusal::RefusedAction(Box::new(
                ControlOperationRefusalOnTheWire {
                    refused_params_pointer: Some("/level".to_owned()),
                    ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::Refused)
                        .on_the_refusal_of(
                            action_name,
                            the_words_of_a_stream_input_named_below_the_top_level(input),
                        )
                },
            )));
        }
        let listings = self.operations_on_the_loaded_streams.list_streams();
        let (_, stream_id) = StreamsTheRuntimeHolds::listed_in(&listings)
            .the_stream_named(stream)
            .map_err(|runtime_words| {
                ControlOperationRefusal::coded(
                    RefusalCodeAndPlace::coded(ControlOperationRefusalCode::StreamNotFound),
                    format!("{action_name} failed: {runtime_words}"),
                )
            })?;
        let graph_input = self
            .the_graph_input_declaring_the_stream_input(action_name, &stream_id, input)
            .await?;
        Ok(graph_input.node.to_string())
    }

    /// The top-level `GraphInput` of the loaded stream `stream_id` declaring the stream input
    /// `input`, refused as the engine codes it.
    async fn the_graph_input_declaring_the_stream_input(
        &self,
        action_name: &str,
        stream_id: &str,
        input: &str,
    ) -> Result<TopLevelGraphInputDeclaringAStreamInput, ControlOperationRefusal> {
        self.the_stream_a_call_names(action_name, stream_id)?
            .the_top_level_graph_input_declaring(input.to_owned())
            .await
            .map_err(|refusal| the_refusal_of_a_reach_for_a_node(action_name, refusal))
    }
}

/// The words of a `set_node_input_value` naming no node and naming the stream input `input` at
/// a level below the top level.
pub(crate) fn the_words_of_a_stream_input_named_below_the_top_level(input: &str) -> String {
    format!(
        "a `set_node_input_value` naming no node sets an input of the stream, which a top-level \
         `GraphInput` declares, so its `level` is the top level; the input `{input}` was named \
         at a level below it. To fix: leave `level` out, or name the node whose input it is"
    )
}
