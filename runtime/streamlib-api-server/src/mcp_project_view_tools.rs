// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The view listing: every Tato Studio view a project and its imports bring in their `views/`.

use std::path::{Path, PathBuf};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use streamlib_project_repository::{
    ProjectViewFolderRead, TatolabViewDeclaredPort, every_view_a_project_and_its_imports_bring,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectViewsToolResult, ProjectViewDeclaredPort, ProjectViewListed,
    WriteGraphInputToolResult, WriteStudioViewOutputToolResult,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;
use crate::mcp_stream_graph_tools::the_answer_to_a_refused_reach_for_a_node;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListProjectViewsToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WriteStudioViewOutputToolArguments {
    #[schemars(description = "The stream id of the stream holding the view node.")]
    stream: String,
    #[schemars(description = "The view node's UUID, as `graph` lists it under `id`.")]
    node: String,
    #[schemars(description = "The view node's output port the value is published on.")]
    port: String,
    #[schemars(
        description = "The value, as the port's family carries it: a Records port takes a JSON object; an Array port a number or a list of numbers, nested to any depth, as its `values` attribute of float64, or an object of such attributes by name; a Channels port a number, as one control value, or `{\"samples\": [[...], ...], \"sample_rate\"}`, float32 channel by channel."
    )]
    value: serde_json::Value,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WriteGraphInputToolArguments {
    #[schemars(description = "The stream id of the running stream.")]
    stream: String,
    #[schemars(
        description = "The Input's name, as a top-level `GraphInput`'s config declares it under `port`, cast as every exposed name is."
    )]
    input: String,
    #[schemars(
        description = "The value, as the Input's family carries it: a Records Input takes a JSON object; an Array Input a number or a list of numbers, nested to any depth, as its `values` attribute of float64, or an object of such attributes by name; a Channels Input a number or a bool, as one control value, or `{\"samples\": [[...], ...], \"sample_rate\"}`, float32 channel by channel."
    )]
    value: serde_json::Value,
}

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

#[tool_router(router = project_view_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "List every Tato Studio view a project brings — each folder of its own `views/`, then of each import's `imports/<folder>/views/` — read from the folder's `tatolab_view.json` without running anything in it. Each view under `views` has its `view_type`, `<project name>:<view folder>`, which is a node type of each stream the project runs when the view opens and its `placement` is `node`; the `project`, `project_name` and `import_folder` (null for the project's own) of the project bringing it; its `view_folder_in_the_project`, the folder relative to the project, as `views/<folder>` or `imports/<import>/views/<folder>`, which names its files to a project file read; its `title`, `description`, `icon`, `preview` and `placement` (`area` or `node`); its declared `inputs` and `outputs`, each a `name`, a `family`, a `description` and an Array port's `range`; and its `manifest` as written. A folder whose manifest is missing, is not JSON, or does not read against view contract 1.0 is listed with `opens` false and why in `not_opened_because`; the rest still lists."
    )]
    async fn list_project_views(
        &self,
        Parameters(ListProjectViewsToolArguments { project_directory }): Parameters<
            ListProjectViewsToolArguments,
        >,
    ) -> ToolCallAnswer {
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
        serialized_tool_result("list_project_views", &listed)
    }

    #[tool(
        description = "Hand a value a Tato Studio panel wrote to the view node `node` of the stream `stream`, which publishes it on its output `port` to every link and viewer reading that port, stamped with stream time now. A view node is a node of a view type, `<project name>:<view folder>`, which each view of the stream's project or of one of its imports is when it opens and its `placement` is `node`; its ports are the view's declared inputs and outputs, and a panel reads its inputs with `open_viewer` on the port linked into each. A write is live data: the last value written is what a reader keeps, many panels may write one node and the last write wins, and nothing is recorded in the graph document or the project's history. Array, Channels and Records outputs are written; an Image or Packets output is refused. Refused for a stream whose project is stopped, naming `start_playback`; a node of another type; a node that is not running; and a value its port's family does not take. A refusal because the node does not run comes with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`."
    )]
    async fn write_studio_view_output(
        &self,
        Parameters(WriteStudioViewOutputToolArguments {
            stream,
            node,
            port,
            value,
        }): Parameters<WriteStudioViewOutputToolArguments>,
    ) -> ToolCallAnswer {
        let stream_operations =
            self.the_stream_a_tool_call_names("write_studio_view_output", &stream)?;
        let written = stream_operations
            .write_a_value_onto_an_output_of_a_view_node(node.clone(), port.clone(), value)
            .await;
        if let Err(refusal) = written {
            return the_answer_to_a_refused_reach_for_a_node("write_studio_view_output", refusal);
        }
        serialized_tool_result(
            "write_studio_view_output",
            &WriteStudioViewOutputToolResult { stream, node, port },
        )
    }

    #[tool(
        description = "Hand a value to the Input `input` of the running stream `stream`: the stream's top-level `GraphInput` declaring it publishes the value on its `value` output to every node it is linked to, stamped with stream time now, and each reads it on its next process. A write is live data: many clients may write one Input and the last write wins, nothing is recorded in the graph document or the project's history, and the value is gone on the next load. Until a write, the Input carries its config's `default`, if it declares one. `graph` renders the value an Input carries now under its node's `graph_interface` component, as `current_value`. Array, Channels and Records Inputs are written; an Image or Packets Input runs nothing and is refused. Refused for a stream that is not running, an Input no top-level `GraphInput` declares, and a value its family does not take. A refusal because the Input's node does not run comes with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`."
    )]
    async fn write_graph_input(
        &self,
        Parameters(WriteGraphInputToolArguments {
            stream,
            input,
            value,
        }): Parameters<WriteGraphInputToolArguments>,
    ) -> ToolCallAnswer {
        let stream_operations = self.the_stream_a_tool_call_names("write_graph_input", &stream)?;
        let family = stream_operations
            .write_a_value_to_a_top_level_graph_input(input.clone(), value)
            .await;
        let family = match family {
            Ok(family) => family,
            Err(refusal) => {
                return the_answer_to_a_refused_reach_for_a_node("write_graph_input", refusal);
            }
        };
        serialized_tool_result(
            "write_graph_input",
            &WriteGraphInputToolResult {
                stream,
                input,
                family: family.wire_name().to_owned(),
            },
        )
    }
}
