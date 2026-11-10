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
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListProjectViewsToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
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
        description = "List every Tato Studio view a project brings — each folder of its own `views/`, then of each import's `imports/<folder>/views/` — read from the folder's `tatolab_view.json` without running anything in it. Each view under `views` has its `view_type`, `<project name>:<view folder>`, which a `tatolab.studio:StudioView` node's `view_type` names; the `project`, `project_name` and `import_folder` (null for the project's own) of the project bringing it; its `view_folder_in_the_project`, the folder relative to the project, as `views/<folder>` or `imports/<import>/views/<folder>`, which names its files to a project file read; its `title`, `description`, `icon`, `preview` and `placement` (`area` or `node`); its declared `inputs` and `outputs`, each a `name`, a `family`, a `description` and an Array port's `range`; and its `manifest` as written. A folder whose manifest is missing, is not JSON, or does not read against view contract 1.0 is listed with `opens` false and why in `not_opened_because`; the rest still lists."
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
}
