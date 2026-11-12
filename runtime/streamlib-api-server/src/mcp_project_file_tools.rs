// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The file tools: a project's files listed, read, written, and a new node file created from
//! the runtime's node template, each write one transaction on
//! the project's history that an undo reverses.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as STANDARD_BASE64;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde::Deserialize;
use streamlib_project_repository::{AppliedProjectTransaction, content_sha256_of};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CreateNodeFileToolResult, ListProjectFilesToolResult, ProjectFileOnTheWire,
    ProjectFileTransactionToolResult, ReadProjectFileToolResult,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, the_author_of_a_change,
    the_idempotency_key_a_caller_sent,
};
use crate::new_node_file_template::{
    NewNodeFileRenderedFromTheTemplate, render_the_new_node_file_template_for,
};
use streamlib_control_contract::{
    AUTHOR_ARGUMENT_DESCRIPTION, IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION,
    ProjectTransactionAuthorArgument,
};

const PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION: &str = "The file, `/`-separated and relative to the project directory, such as `nodes/camera.py`; refused outside the project, inside `.tatolab/`, or through a symbolic link.";

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListProjectFilesToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "A folder of the project, `/`-separated and relative to the project directory, to list alone; the whole project when omitted."
    )]
    under: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ReadProjectFileToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION)]
    path: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WriteProjectFileToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = PROJECT_FILE_PATH_ARGUMENT_DESCRIPTION)]
    path: String,
    #[schemars(
        description = "The file's whole new content as UTF-8 text. Send this or `content_base64`, never both."
    )]
    content: Option<String>,
    #[schemars(
        description = "The file's whole new content as standard base64 with padding, for bytes that are not text. Send this or `content`, never both."
    )]
    content_base64: Option<String>,
    #[schemars(
        description = "The `content_sha256` `read_project_file` or the last `write_project_file` answered for the file: the write is refused, and the file left as it is, when the file no longer holds those bytes. Omitted, the write goes over whatever the file holds."
    )]
    expected_sha256: Option<String>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CreateNodeFileToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "The new Python file, `/`-separated and relative to the project directory, such as `nodes/edge_glow.py`: each folder and the file's name a Python identifier, the class named after the file (`EdgeGlow`). Refused where a file already is, outside the project, inside `.tatolab/` or `imports/`, or through a symbolic link."
    )]
    path: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[tool_router(router = project_file_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "List the project's files, or those under `under`, each by its path and length in bytes, sorted by path. Hidden folders such as `.tatolab/`, Python virtual environments, `node_modules/`, `__pycache__/` and symbolic links are not listed."
    )]
    async fn list_project_files(
        &self,
        Parameters(ListProjectFilesToolArguments {
            project_directory,
            under,
        }): Parameters<ListProjectFilesToolArguments>,
    ) -> ToolCallAnswer {
        let listed = self
            .a_project_repository_call_off_the_async_runtime(
                "list_project_files",
                project_directory,
                move |project_repository| project_repository.list_project_files(under.as_deref()),
            )
            .await?;
        serialized_tool_result(
            "list_project_files",
            &ListProjectFilesToolResult {
                files: listed
                    .into_iter()
                    .map(|listed| ProjectFileOnTheWire {
                        path: listed.path,
                        byte_len: listed.byte_len,
                    })
                    .collect(),
            },
        )
    }

    #[tool(
        description = "Read one of the project's files whole, answering its bytes as `content_base64`, standard base64 with padding, its `byte_len`, and its `content_sha256`, which a `write_project_file` sends back as `expected_sha256`. Refused for a path that is not a file of the project."
    )]
    async fn read_project_file(
        &self,
        Parameters(ReadProjectFileToolArguments {
            project_directory,
            path,
        }): Parameters<ReadProjectFileToolArguments>,
    ) -> ToolCallAnswer {
        let path_read = path.clone();
        let bytes = self
            .a_project_repository_call_off_the_async_runtime(
                "read_project_file",
                project_directory,
                move |project_repository| project_repository.read_project_file(&path_read),
            )
            .await?;
        serialized_tool_result(
            "read_project_file",
            &ReadProjectFileToolResult {
                path,
                byte_len: bytes.len() as u64,
                content_base64: STANDARD_BASE64.encode(&bytes),
                content_sha256: content_sha256_of(&bytes),
            },
        )
    }

    #[tool(
        description = "Create a new node file of the project from the runtime's node template, as one transaction an undo reverses: a `@node.process` class in the current `tatolab.stream` idiom, named after the file, with one Image input `video_in` whose frames it hands on unchanged to one Image output `video_out`, each with its description. It loads as a node type of the project with no edits and `list_project_node_types` lists it at once. Answers the `path`, the `node_type` it declares — `<module>:<class>`, what `add_node` takes — and the `transaction` and `commit`. Refused for a path that already holds a file, that is not a `.py` file importable as a Python module, or whose class name the template cannot declare, and for every path `write_project_file` refuses."
    )]
    async fn create_node_file(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(CreateNodeFileToolArguments {
            project_directory,
            path,
            author,
            idempotency_key,
        }): Parameters<CreateNodeFileToolArguments>,
    ) -> ToolCallAnswer {
        let NewNodeFileRenderedFromTheTemplate { node_type, content } =
            render_the_new_node_file_template_for(&path)
                .map_err(|why| format!("create_node_file failed: {why}"))?;
        let author = the_author_of_a_change(author, &tool_call_request_context);
        let path_created = path.clone();
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let AppliedProjectTransaction {
            transaction,
            commit,
            ..
        } = self
            .a_project_repository_call_off_the_async_runtime(
                "create_node_file",
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let outcome = project_repository.create_project_file(
                        &author,
                        idempotency_key.as_ref(),
                        &path_created,
                        content.as_bytes(),
                    )?;
                    project_repositories
                        .announce_the_transaction_if_applied_now(project_repository, &outcome);
                    Ok(outcome.into_applied_transaction())
                },
            )
            .await?;
        serialized_tool_result(
            "create_node_file",
            &CreateNodeFileToolResult {
                path,
                node_type,
                transaction: transaction.transaction.to_string(),
                commit,
            },
        )
    }

    #[tool(
        description = "Write one of the project's files whole, as one transaction: a new file is created with its folders, an existing one overwritten. An undo puts back what the file held, or takes a new file out. Answers the `path`, the `content_sha256` of the bytes written, and the `transaction` and `commit`. With `expected_sha256`, refused when the file changed or was taken out since it was read, so a save never overwrites another change. Content the file already holds is refused, as it changes nothing. Refused for what another tool changes — a stream's graph document, an asset's sidecar or bytes (`replace_asset`), `tatolab-project.json`, a file under `imports/` — and for a file inside a hidden folder or a Python virtual environment, which history does not record."
    )]
    async fn write_project_file(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(WriteProjectFileToolArguments {
            project_directory,
            path,
            content,
            content_base64,
            expected_sha256,
            author,
            idempotency_key,
        }): Parameters<WriteProjectFileToolArguments>,
    ) -> ToolCallAnswer {
        let bytes = match (content, content_base64) {
            (Some(text), None) => text.into_bytes(),
            (None, Some(encoded)) => STANDARD_BASE64.decode(encoded).map_err(|why| {
                format!("write_project_file failed: `content_base64` is not base64: {why}")
            })?,
            _ => {
                return Err(
                    "write_project_file failed: send exactly one of `content` and \
                     `content_base64`"
                        .to_owned(),
                );
            }
        };
        let content_sha256 = content_sha256_of(&bytes);
        let author = the_author_of_a_change(author, &tool_call_request_context);
        let path_written = path.clone();
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let AppliedProjectTransaction {
            transaction,
            commit,
            ..
        } = self
            .a_project_repository_call_off_the_async_runtime(
                "write_project_file",
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let outcome = project_repository.write_project_file(
                        &author,
                        idempotency_key.as_ref(),
                        &path_written,
                        &bytes,
                        expected_sha256.as_deref(),
                    )?;
                    project_repositories
                        .announce_the_transaction_if_applied_now(project_repository, &outcome);
                    Ok(outcome.into_applied_transaction())
                },
            )
            .await?;
        serialized_tool_result(
            "write_project_file",
            &ProjectFileTransactionToolResult {
                path,
                content_sha256,
                transaction: transaction.transaction.to_string(),
                commit,
            },
        )
    }
}
