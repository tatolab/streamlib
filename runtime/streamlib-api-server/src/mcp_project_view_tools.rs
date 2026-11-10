// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Tato Studio view tools: the listing of every view a project and its imports bring in
//! their `views/`, and the write of a panel's value onto a StudioView node's output.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use streamlib_project_repository::{
    ProjectViewFolderRead, TatolabViewDeclaredPort, every_view_a_project_and_its_imports_bring,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectViewsToolResult, ProjectViewDeclaredPort, ProjectViewListed,
    WriteStudioViewOutputToolResult,
};

use crate::mcp::{
    LocalApiMcpServerHandler, STREAM_ARGUMENT_DESCRIPTION, ToolCallAnswer, serialized_tool_result,
};
use crate::mcp_project_asset_tools::ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;

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
    #[schemars(description = STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(
        description = "The `tatolab.studio:StudioView` node's id, as `graph` lists it under `id`."
    )]
    node: String,
    #[schemars(
        description = "The node's output, one its view's `tatolab_view.json` declares, as `graph` lists it under `ports.outputs`."
    )]
    port: String,
    #[schemars(
        description = "The value, as the output's family takes it. Array: a number, a rectangular list of numbers, or an object of such, each an attribute by its key, a bare number or list being the attribute `value`; every attribute is `float64`. Channels: `{\"sample_rate\": <samples a second>, \"channels\": [[<sample>, …], …]}`, each channel as many samples, written as `float32`. Records: an object of fields."
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

fn the_view_on_the_wire(view: ProjectViewFolderRead) -> ProjectViewListed {
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
        folder: view.folder,
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
        description = "List every Tato Studio view a project brings — each folder of its own `views/`, then of each import's `imports/<folder>/views/` — read from the folder's `tatolab_view.json` without running anything in it. Each view under `views` has its `view_type`, `<project name>:<view folder>`, which a `tatolab.studio:StudioView` node's `view_type` names; the `project`, `project_name` and `import_folder` (null for the project's own) of the project bringing it; its absolute `folder`; its `title`, `description`, `icon`, `preview` and `placement` (`area` or `node`); its declared `inputs` and `outputs`, each a `name`, a `family`, a `description` and an Array port's `range`; and its `manifest` as written. A folder whose manifest is missing, is not JSON, or does not read against view contract 1.0 is listed with `opens` false and why in `not_opened_because`; the rest still lists."
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
                            .map(the_view_on_the_wire)
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
        description = "Write a value onto an output of a running `tatolab.studio:StudioView` node, which publishes it on that output to every node linked from it, as a node publishes what it writes; a linked reader keeps the last value it read. Any number of panels may write one node, and the last write wins. A write is live data: it is never recorded in the stream's graph document or the project's history. Array, Channels and Records outputs take a value; an Image or Packets output takes none. Refused for a node that is not a running StudioView, an output its view does not declare, and a value its family does not take, in the runtime's own words."
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
        stream_operations
            .write_a_studio_view_output(node.clone(), port.clone(), value)
            .await
            .map_err(|refusal| format!("write_studio_view_output failed: {refusal}"))?;
        serialized_tool_result(
            "write_studio_view_output",
            &WriteStudioViewOutputToolResult { stream, node, port },
        )
    }
}
