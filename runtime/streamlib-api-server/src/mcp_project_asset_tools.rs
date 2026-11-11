// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The asset tools: a project's assets listed, described, imported, renamed, moved, deleted and
//! given new settings, and its streams created, deleted and named, each change one transaction
//! on the project's history.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as STANDARD_BASE64;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib::sdk::error::Result as StreamlibResult;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, StreamActionQueued};
use streamlib_asset_media_facts::{
    AssetMediaFactsCacheOfAProject, AssetMediaFactsCachedInTheProject,
};
use streamlib_project_repository::{
    AppliedProjectTransaction, GRAPH_DOCUMENT_FILE_SUFFIX, ProjectActionIdempotencyKey,
    ProjectAsset, ProjectAssetAction, ProjectAssetImportSource, ProjectAssetKind,
    ProjectAssetOperation, ProjectOperation, ProjectRepository, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, DescribeAssetToolResult, ListAssetsToolResult,
    ProjectAssetOnTheWire, ProjectAssetWithItsMediaFactsOnTheWire, ReadAssetPreviewToolResult,
};
use uuid::Uuid;

use crate::asset_media_facts_reads::AssetMediaFactsNow;
use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_change_feed::project_transactions_resource_uri;

/// The author an asset change names when its caller's MCP client gave no name.
const AUTHOR_NAME_OF_AN_UNNAMED_MCP_CLIENT: &str = "an MCP client";

pub(crate) const IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION: &str = "A key the caller mints once per user gesture, such as a slider drag committed on release. Sent again under a key the project's history holds, the action applies nothing and answers the transaction the key first applied. Omit it to apply the action each time it is sent. A `live` edit is never recorded, so its key is not kept.";

/// The key a caller sent, refused when blank.
pub(crate) fn the_idempotency_key_a_caller_sent(
    idempotency_key: Option<String>,
) -> Result<Option<ProjectActionIdempotencyKey>, ProjectRepositoryRefusal> {
    idempotency_key
        .map(ProjectActionIdempotencyKey::from_caller)
        .transpose()
}

pub(crate) const ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project, by absolute path: a folder holding `tatolab-project.json`.";
const ASSET_ARGUMENT_DESCRIPTION: &str =
    "The asset, by the UUID `import_asset` or `list_assets` answered.";
const STREAM_ASSET_ARGUMENT_DESCRIPTION: &str = "The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports as its location's `stream_asset`.";
const STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION: &str =
    "The name the stream is shown under, written in its graph document; refused when blank.";
pub(crate) const AUTHOR_ARGUMENT_DESCRIPTION: &str = "Who the change is by, as history records it and undo follows it. Omit it to be the agent the MCP client named in its `clientInfo`.";

/// What an asset is, as a caller names it.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
enum ProjectAssetKindToolArgument {
    /// Video, audio or an image.
    Media,
    /// USD, glTF and other 3D scenes.
    Scene,
    /// Everything else a node reads: an FMU, a LUT, model weights, a font.
    Data,
    /// A stream's graph document; listed, never imported.
    Stream,
}

impl From<ProjectAssetKindToolArgument> for ProjectAssetKind {
    fn from(kind: ProjectAssetKindToolArgument) -> Self {
        match kind {
            ProjectAssetKindToolArgument::Media => Self::Media,
            ProjectAssetKindToolArgument::Scene => Self::Scene,
            ProjectAssetKindToolArgument::Data => Self::Data,
            ProjectAssetKindToolArgument::Stream => Self::Stream,
        }
    }
}

/// Who an asset change is by, as a caller names it.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectTransactionAuthorKindToolArgument {
    /// A person, through the studio or the CLI.
    Person,
    /// An agent.
    Agent,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectTransactionAuthorToolArgument {
    #[schemars(description = "`person` or `agent`.")]
    kind: ProjectTransactionAuthorKindToolArgument,
    #[schemars(description = "The author's name as history shows it.")]
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListAssetsToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = "Only the assets of this kind. Omit it for every asset.")]
    kind: Option<ProjectAssetKindToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DescribeAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ReadAssetPreviewToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ImportAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Where the bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    source: String,
    #[schemars(
        description = "Where the asset sits in the project, `/`-separated and relative to the project directory, such as `media/intro.mp4`. Omit it to place the file in its kind's folder — `media/`, `scenes/` or `data/` — under `file_name`, numbered `<name>-2.<extension>`, `-3`, … past a name already taken there."
    )]
    asset_path: Option<String>,
    #[schemars(
        description = "The asset's file name in its kind's folder when `asset_path` is omitted, such as `intro.mp4`. Omit it for the source file's own name; an upload, which carries no name, needs it."
    )]
    file_name: Option<String>,
    #[schemars(
        description = "`media`, `scene` or `data`. Omit it for the kind the file's extension implies, `data` when it implies none."
    )]
    kind: Option<ProjectAssetKindToolArgument>,
    #[schemars(description = "The settings the import takes, kept in the asset's sidecar.")]
    import_settings: Option<Map<String, Value>>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RelocateAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
    #[schemars(
        description = "The asset's new path in the project, `/`-separated and relative to the project directory, such as `archive/opening.mp4`; its folders are made when missing."
    )]
    to_path: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DeleteAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetAssetSettingsToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
    #[schemars(description = "The settings that replace the asset's import settings whole.")]
    import_settings: Map<String, Value>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ReplaceAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
    #[schemars(
        description = "Where the new bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    source: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct CreateStreamToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Where the stream's graph document sits in the project, `/`-separated and relative to the project directory and ending `.graph.json`, such as `streams/preview.graph.json`."
    )]
    path: String,
    #[schemars(description = STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION)]
    display_name: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DeleteStreamToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = STREAM_ASSET_ARGUMENT_DESCRIPTION)]
    stream_asset: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetStreamDisplayNameToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = STREAM_ASSET_ARGUMENT_DESCRIPTION)]
    stream_asset: String,
    #[schemars(description = STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION)]
    display_name: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[tool_router(router = project_asset_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "List a project's assets, ordered by path: each by its `asset` UUID, its `kind` (`stream`, `media`, `scene` or `data`), its `path` in the project, the SHA-256 and size of its bytes, its `import_settings`, and what the runtime has read from its bytes — `media_facts_state` (`read`, `reading`, `unreadable` with `media_facts_unreadable_because`, or `null` for a stream), `media_facts` and `preview`, as `describe_asset` answers them, each from what is already read, an asset not yet read queued for a read — with the project's UUID, `project_name`, `project_description`, `project_cover` — the UUID of the image asset it is shown by — and `project_last_edited_at`, when its history last changed in seconds since the Unix epoch, each `null` when it has none, and `transactions_resource_uri`, its change feed. `kind` narrows to one kind. Refused for a folder with no `tatolab-project.json`."
    )]
    async fn list_assets(
        &self,
        Parameters(ListAssetsToolArguments {
            project_directory,
            kind,
        }): Parameters<ListAssetsToolArguments>,
    ) -> ToolCallAnswer {
        let kind = kind.map(ProjectAssetKind::from);
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads_in_this_runtime);
        let listed = self
            .a_project_repository_call_off_the_async_runtime(
                "list_assets",
                project_directory,
                move |project_repository| {
                    let project = project_repository.project_file().project;
                    let mut assets: Vec<ProjectAssetWithItsMediaFactsOnTheWire> =
                        project_repository
                            .assets()?
                            .iter()
                            .filter(|asset| kind.is_none_or(|kind| asset.kind == kind))
                            .map(|asset| {
                                asset_media_facts_reads.the_asset_with_its_media_facts(
                                    project,
                                    project_repository.project_directory(),
                                    asset,
                                    project_asset_on_the_wire(asset),
                                )
                            })
                            .collect();
                    assets.sort_by(|first, second| first.asset.path.cmp(&second.asset.path));
                    Ok(ListAssetsToolResult {
                        project_directory: project_repository.project_directory().to_path_buf(),
                        project: project_repository.project_file().project.to_string(),
                        project_name: project_repository.project_file().name.clone(),
                        project_description: project_repository.project_file().description.clone(),
                        project_cover: project_repository
                            .project_file()
                            .cover
                            .map(|cover| cover.to_string()),
                        project_last_edited_at: project_repository.last_edited_at_unix_seconds()?,
                        assets,
                        transactions_resource_uri: project_transactions_resource_uri(
                            project_repository.project_file().project,
                        ),
                    })
                },
            )
            .await?;
        serialized_tool_result("list_assets", &listed)
    }

    #[tool(
        description = "Describe one asset of a project: its UUID, kind, path, the SHA-256 and size of its bytes, its import settings, `file_at_its_path`, whether a file is at its path, and what the runtime has read from its bytes, which it reads once per SHA-256 after an import or a replace and keeps in the project's `.tatolab/`. `media_facts_state` is `read`, `reading` — a read queued or under way, after which the project's change feed carries a `media_facts_read` entry — `unreadable`, with why in `media_facts_unreadable_because`, or `null` for a stream asset. `media_facts` carries `duration_ms`; `video` with `pixel_width`, `pixel_height`, `frames_per_second` and `codec`; `audio` with `sample_rate_hz`, `channel_count` and `codec`; `image` with `pixel_width` and `pixel_height`; `lut` with `lut_1d_size` or `lut_3d_size`, `domain_min` and `domain_max`; and `scene` — each `null` when the file does not hold or state it. `preview` is the size and `media_type` of the picture `read_asset_preview` answers, `null` when the asset has none."
    )]
    async fn describe_asset(
        &self,
        Parameters(DescribeAssetToolArguments {
            project_directory,
            asset,
        }): Parameters<DescribeAssetToolArguments>,
    ) -> ToolCallAnswer {
        let asset = the_asset_a_call_names("describe_asset", "asset", &asset)?;
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads_in_this_runtime);
        let described = self
            .a_project_repository_call_off_the_async_runtime(
                "describe_asset",
                project_directory,
                move |project_repository| {
                    let described_asset = project_repository.asset(asset)?;
                    Ok(DescribeAssetToolResult {
                        file_at_its_path: described_asset
                            .path
                            .on_disk_under(project_repository.project_directory())
                            .is_file(),
                        asset: asset_media_facts_reads.the_asset_with_its_media_facts(
                            project_repository.project_file().project,
                            project_repository.project_directory(),
                            &described_asset,
                            project_asset_on_the_wire(&described_asset),
                        ),
                    })
                },
            )
            .await?;
        serialized_tool_result("describe_asset", &described)
    }

    #[tool(
        description = "Read an asset's preview picture, which the runtime makes from its bytes and keeps in the project's `.tatolab/`: a PNG at most 512 pixels on its longest edge, answered as `content_base64`, standard base64 with padding, with its `media_type`, `pixel_width`, `pixel_height` and `byte_len`. An image has one; video, audio, a LUT, a scene and other data have none yet, as `describe_asset`'s `preview` being `null` says. Refused while the runtime is still reading the asset's bytes — `describe_asset`'s `media_facts_state` reads `reading`, and the project's change feed carries a `media_facts_read` entry once it is done — for an asset with no preview, and for a stream asset."
    )]
    async fn read_asset_preview(
        &self,
        Parameters(ReadAssetPreviewToolArguments {
            project_directory,
            asset,
        }): Parameters<ReadAssetPreviewToolArguments>,
    ) -> ToolCallAnswer {
        let asset = the_asset_a_call_names("read_asset_preview", "asset", &asset)?;
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads_in_this_runtime);
        let previewed = self
            .a_project_repository_call_off_the_async_runtime(
                "read_asset_preview",
                project_directory,
                move |project_repository| {
                    let previewed_asset = project_repository.asset(asset)?;
                    Ok((
                        asset_media_facts_reads.the_media_facts_of(
                            project_repository.project_file().project,
                            project_repository.project_directory(),
                            &previewed_asset,
                        ),
                        project_repository.project_directory().to_path_buf(),
                        previewed_asset.content_sha256,
                    ))
                },
            )
            .await?;
        let (media_facts_now, project_directory, content_sha256) = previewed;
        let preview = match media_facts_now {
            AssetMediaFactsNow::NotRead => {
                return Err(format!(
                    "read_asset_preview failed: asset {asset} is a stream, which has no preview; `graph` serves its graph"
                ));
            }
            AssetMediaFactsNow::Reading => {
                return Err(format!(
                    "read_asset_preview failed: the runtime is still reading asset {asset}'s bytes; read again once `describe_asset` answers its `media_facts_state` as `read`"
                ));
            }
            AssetMediaFactsNow::NoFileAtItsPath => {
                return Err(format!(
                    "read_asset_preview failed: no file is at asset {asset}'s path"
                ));
            }
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Unreadable {
                media_facts_unreadable_because,
            }) => {
                return Err(format!(
                    "read_asset_preview failed: asset {asset}'s bytes did not read, so it has no preview: {media_facts_unreadable_because}"
                ));
            }
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Read {
                preview: None,
                ..
            }) => {
                return Err(format!(
                    "read_asset_preview failed: asset {asset} has no preview picture; the runtime makes one for an image"
                ));
            }
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Read {
                preview: Some(preview),
                ..
            }) => preview,
        };
        let png_bytes = tokio::task::spawn_blocking(move || {
            AssetMediaFactsCacheOfAProject::of_the_project_at(&project_directory)
                .preview_png_bytes(&content_sha256)
        })
        .await
        .map_err(|join_failure| {
            format!("read_asset_preview failed: the read did not finish: {join_failure}")
        })?
        .ok_or_else(|| {
            format!(
                "read_asset_preview failed: asset {asset}'s preview picture is gone from the project's `.tatolab/`; it is made again after the asset's next `replace_asset`"
            )
        })?;
        serialized_tool_result(
            "read_asset_preview",
            &ReadAssetPreviewToolResult {
                asset: asset.to_string(),
                pixel_width: preview.pixel_width,
                pixel_height: preview.pixel_height,
                media_type: preview.media_type,
                byte_len: png_bytes.len() as u64,
                content_base64: STANDARD_BASE64.encode(&png_bytes),
            },
        )
    }

    #[tool(
        description = "Import a file as a new asset of the project at `asset_path`, or, with no `asset_path`, in its kind's folder of the project's standard layout — `media/` for video, audio and images, `scenes/` for USD, glTF and other 3D scenes, `data/` for everything else — under `file_name` or the source file's own name, numbered `<name>-2.<extension>`, `-3`, … past a name an asset or another file already takes there. Answers the `asset`, with the UUID the runtime made for it, which every later relocation keeps, and the `path` it landed at, and the `transaction` and `commit` that record it. The bytes are copied once, to `asset_path`, and only the asset's sidecar, `<asset_path>.asset.json`, enters history. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there; a file already at `asset_path` is imported in place. Undoing the import removes the file at `asset_path`, one imported in place included, and nothing keeps its bytes. Refused whole, with nothing written, for a path another asset or another file holds, a path outside the project, and a stream, which is never imported."
    )]
    async fn import_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(ImportAssetToolArguments {
            project_directory,
            source,
            asset_path,
            file_name,
            kind,
            import_settings,
            author,
            idempotency_key,
        }): Parameters<ImportAssetToolArguments>,
    ) -> ToolCallAnswer {
        let change = ProjectAssetAction::ImportAsset {
            source: ProjectAssetImportSource::try_from(source)
                .map_err(|refusal| format!("import_asset failed: {refusal}"))?,
            asset_path,
            file_name,
            kind: kind.map(ProjectAssetKind::from),
            import_settings: import_settings.unwrap_or_default(),
        };
        self.one_asset_change_as_a_transaction(
            "import_asset",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            change,
        )
        .await
    }

    #[tool(
        description = "Put an asset at another path of the project, renaming it, moving it to another folder or both, its sidecar going with it, as one transaction; the asset keeps its UUID. Answers the asset as it now is, with the `transaction` and `commit`."
    )]
    async fn relocate_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(RelocateAssetToolArguments {
            project_directory,
            asset,
            to_path,
            author,
            idempotency_key,
        }): Parameters<RelocateAssetToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "relocate_asset",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::RelocateAsset {
                asset: the_asset_a_call_names("relocate_asset", "asset", &asset)?,
                to_path,
            },
        )
        .await
    }

    #[tool(
        description = "Stop tracking an asset as one transaction: its file and sidecar leave the project, and nothing keeps its bytes, so undoing the transaction brings back its sidecar and the asset reads missing until its file is at its path again. A stream asset's graph document leaves the project, recorded in the transaction so an undo writes it back, and its stream is unloaded and forgotten. Refused while a stream's graph document references the asset, the refusal naming each document. Answers the asset as it was, with the `transaction` and `commit`."
    )]
    async fn delete_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(DeleteAssetToolArguments {
            project_directory,
            asset,
            author,
            idempotency_key,
        }): Parameters<DeleteAssetToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "delete_asset",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::DeleteAsset {
                asset: the_asset_a_call_names("delete_asset", "asset", &asset)?,
            },
        )
        .await
    }

    #[tool(
        description = "Replace an asset's import settings whole, as one transaction. Answers the asset as it now is, with the `transaction` and `commit`."
    )]
    async fn set_asset_settings(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetAssetSettingsToolArguments {
            project_directory,
            asset,
            import_settings,
            author,
            idempotency_key,
        }): Parameters<SetAssetSettingsToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "set_asset_settings",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::SetAssetSettings {
                asset: the_asset_a_call_names("set_asset_settings", "asset", &asset)?,
                import_settings,
            },
        )
        .await
    }
    #[tool(
        description = "Put new bytes at an asset's path as one transaction: the asset keeps its UUID, path, kind and settings, so every node referencing it reads the new bytes at its next load, and its sidecar records their SHA-256 and size. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there. The bytes replaced are kept as an upload under their SHA-256, so an undo puts them back with the earlier sidecar; history records the sidecar alone. A file replaced outside the runtime is not supported. Answers the asset as it now is, with the `transaction` and `commit`; bytes the sidecar already records apply nothing and are refused as an empty transaction. Refused for a stream asset, whose graph document graph edits change."
    )]
    async fn replace_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(ReplaceAssetToolArguments {
            project_directory,
            asset,
            source,
            author,
            idempotency_key,
        }): Parameters<ReplaceAssetToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "replace_asset",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::ReplaceAsset {
                asset: the_asset_a_call_names("replace_asset", "asset", &asset)?,
                source: ProjectAssetImportSource::try_from(source)
                    .map_err(|refusal| format!("replace_asset failed: {refusal}"))?,
            },
        )
        .await
    }

    #[tool(
        description = "Create a stream in the project as one transaction: a new stream asset, its graph document written at `path` holding no nodes, its UUID, which the runtime makes, and its `display_name`. While the project runs, the new stream is loaded on the project's queue and the answer carries that load's `action`, which `describe_stream_action` follows. Answers the stream asset as `asset`, with the `transaction` and `commit`; an undo takes the document out and unloads the stream. Refused for a path another file holds or outside the project, and a blank `display_name`."
    )]
    async fn create_stream(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(CreateStreamToolArguments {
            project_directory,
            path,
            display_name,
            author,
            idempotency_key,
        }): Parameters<CreateStreamToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "create_stream",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::CreateStream { path, display_name },
        )
        .await
    }

    #[tool(
        description = "Delete a stream from the project as one transaction: its graph document leaves the project, recorded in the transaction so an undo writes it back and loads the stream again while the project runs, and its stream is unloaded and forgotten. Answers the stream asset as it was, with the `transaction` and `commit`. Refused for a UUID the project holds no stream asset under."
    )]
    async fn delete_stream(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(DeleteStreamToolArguments {
            project_directory,
            stream_asset,
            author,
            idempotency_key,
        }): Parameters<DeleteStreamToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "delete_stream",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::DeleteStream {
                stream_asset: the_asset_a_call_names(
                    "delete_stream",
                    "stream_asset",
                    &stream_asset,
                )?,
            },
        )
        .await
    }

    #[tool(
        description = "Show a stream under another display name as one transaction, written in its graph document so every machine the project is on shows it and an undo writes the earlier one. A loaded stream is shown under it at once and keeps its stream id. Answers the stream asset as it now is, with the `transaction` and `commit`. Refused for a blank `display_name`."
    )]
    async fn set_stream_display_name(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetStreamDisplayNameToolArguments {
            project_directory,
            stream_asset,
            display_name,
            author,
            idempotency_key,
        }): Parameters<SetStreamDisplayNameToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "set_stream_display_name",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            ProjectAssetAction::SetStreamDisplayName {
                stream_asset: the_asset_a_call_names(
                    "set_stream_display_name",
                    "stream_asset",
                    &stream_asset,
                )?,
                display_name,
            },
        )
        .await
    }
}

impl LocalApiMcpServerHandler {
    /// Run `call` on the project at `project_directory`, its one writer held, on a blocking task;
    /// a refusal is the runtime's own words.
    pub(crate) async fn a_project_repository_call_off_the_async_runtime<
        ProjectRepositoryCallOutcome,
    >(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        call: impl FnOnce(
            &mut ProjectRepository,
        ) -> Result<ProjectRepositoryCallOutcome, ProjectRepositoryRefusal>
        + Send
        + 'static,
    ) -> Result<ProjectRepositoryCallOutcome, String>
    where
        ProjectRepositoryCallOutcome: Send + 'static,
    {
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        tokio::task::spawn_blocking(move || {
            let project_repository =
                project_repositories.the_project_repository_at(&project_directory)?;
            let mut project_repository = project_repository.lock();
            call(&mut project_repository)
        })
        .await
        .map_err(|join_failure| {
            format!("{tool_name} failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("{tool_name} failed: {refusal}"))
    }

    /// Apply `change` by `author` to the project as one transaction, answering the asset it
    /// changed; a transaction applied now also brings the loaded streams in line with each graph
    /// document it wrote or took out. Sent again under `idempotency_key`, it answers the
    /// transaction that key first applied.
    async fn one_asset_change_as_a_transaction(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        change: ProjectAssetAction,
    ) -> ToolCallAnswer {
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads_in_this_runtime);
        let (mut answered, project, project_directory, transaction_applied_now) = self
            .a_project_repository_call_off_the_async_runtime(
                tool_name,
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let outcome = project_repository.apply_asset_changes(
                        &author,
                        idempotency_key.as_ref(),
                        vec![change],
                    )?;
                    project_repositories
                        .announce_the_transaction_if_applied_now(project_repository, &outcome);
                    let applied_now = outcome.was_applied_now();
                    let AppliedProjectTransaction {
                        transaction,
                        commit,
                        ..
                    } = outcome.into_applied_transaction();
                    let changed_asset =
                        project_repository.the_asset_a_transaction_changed(&transaction)?;
                    if applied_now {
                        asset_media_facts_reads.the_media_facts_of(
                            project_repository.project_file().project,
                            project_repository.project_directory(),
                            &changed_asset,
                        );
                    }
                    Ok((
                        AssetTransactionToolResult {
                            asset: project_asset_on_the_wire(&changed_asset),
                            transaction: transaction.transaction.to_string(),
                            commit,
                            action: None,
                        },
                        project_repository.project_file().project,
                        project_repository.project_directory().to_path_buf(),
                        applied_now.then_some(transaction),
                    ))
                },
            )
            .await?;
        if let Some(transaction) = transaction_applied_now {
            let loads_queued = self
                .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                    bring_the_loaded_streams_in_line_with_the_stream_documents_of(
                        operations_on_the_loaded_streams,
                        project,
                        &project_directory,
                        &transaction,
                    )
                })
                .await
                .map_err(|not_in_line| {
                    format!(
                        "{tool_name} applied its transaction, and the loaded streams were not \
                         brought in line with it: {not_in_line}"
                    )
                })?;
            answered.action = loads_queued
                .into_iter()
                .next()
                .map(|queued| queued.action_id);
        }
        serialized_tool_result(tool_name, &answered)
    }
}

/// Bring the streams this runtime loads in line with each graph document `transaction` changed
/// in the project `project`, at `project_directory`: a document taken out unloads and forgets
/// its stream, a document written is loaded while the project runs, and a new display name shows
/// the loaded stream under it. Answers each load queued.
pub(crate) fn bring_the_loaded_streams_in_line_with_the_stream_documents_of(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    project: Uuid,
    project_directory: &Path,
    transaction: &ProjectTransaction,
) -> StreamlibResult<Vec<StreamActionQueued>> {
    let mut loads_queued = Vec::new();
    for operation in &transaction.operations {
        match operation {
            ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::DeleteStreamDocument { stream, .. },
            ) => {
                operations_on_the_loaded_streams
                    .forget_the_stream_of_a_deleted_stream_asset(project, *stream)?;
            }
            ProjectOperation::Asset(ProjectAssetOperation::DeleteAsset { asset })
                if asset.kind == ProjectAssetKind::Stream =>
            {
                operations_on_the_loaded_streams
                    .forget_the_stream_of_a_deleted_stream_asset(project, asset.asset)?;
            }
            ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument { stream, .. },
            ) => loads_queued.extend(
                operations_on_the_loaded_streams
                    .load_the_stream_of_a_created_stream_asset(project_directory, *stream),
            ),
            ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::SetStreamDisplayName {
                    stream, path, to, ..
                },
            ) => {
                let file_name = path.file_name();
                let display_name = to.as_deref().unwrap_or_else(|| {
                    file_name
                        .strip_suffix(GRAPH_DOCUMENT_FILE_SUFFIX)
                        .unwrap_or(file_name)
                });
                operations_on_the_loaded_streams.show_the_stream_of_a_stream_asset_under(
                    project,
                    *stream,
                    display_name,
                );
            }
            _ => {}
        }
    }
    Ok(loads_queued)
}

/// The author a change names: the caller's own, or the agent the request's MCP client names
/// itself — in the request's `_meta` `clientInfo`, or else its `initialize` handshake.
pub(crate) fn the_author_of_a_change(
    author: Option<ProjectTransactionAuthorToolArgument>,
    tool_call_request_context: &RequestContext<RoleServer>,
) -> ProjectTransactionAuthor {
    match author {
        Some(ProjectTransactionAuthorToolArgument { kind, name }) => ProjectTransactionAuthor {
            kind: match kind {
                ProjectTransactionAuthorKindToolArgument::Person => {
                    ProjectTransactionAuthorKind::Person
                }
                ProjectTransactionAuthorKindToolArgument::Agent => {
                    ProjectTransactionAuthorKind::Agent
                }
            },
            name,
        },
        None => ProjectTransactionAuthor {
            kind: ProjectTransactionAuthorKind::Agent,
            name: tool_call_request_context
                .client_info()
                .map(|client| client.name)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| AUTHOR_NAME_OF_AN_UNNAMED_MCP_CLIENT.to_owned()),
        },
    }
}

/// The asset UUID a call spells in its argument `argument_name`.
fn the_asset_a_call_names(
    tool_name: &str,
    argument_name: &str,
    asset: &str,
) -> Result<Uuid, String> {
    Uuid::parse_str(asset).map_err(|_| {
        format!("{tool_name} failed: `{argument_name}` must be an asset's UUID; got `{asset}`")
    })
}

/// `source` as an import reads it: an absolute path, or a content hash the store holds.
pub(crate) fn project_asset_on_the_wire(asset: &ProjectAsset) -> ProjectAssetOnTheWire {
    ProjectAssetOnTheWire {
        asset: asset.asset.to_string(),
        kind: project_asset_kind_on_the_wire(asset.kind).to_owned(),
        path: asset.path.to_string(),
        content_sha256: asset.content_sha256.clone(),
        content_byte_len: asset.content_byte_len,
        import_settings: asset.import_settings.clone(),
    }
}

fn project_asset_kind_on_the_wire(kind: ProjectAssetKind) -> &'static str {
    match kind {
        ProjectAssetKind::Stream => "stream",
        ProjectAssetKind::Media => "media",
        ProjectAssetKind::Scene => "scene",
        ProjectAssetKind::Data => "data",
    }
}
