// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The asset tools: a project's assets listed, described, imported, renamed, moved, deleted and
//! given new settings, each change one transaction on the project's history.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Peer, RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib_project_repository::{
    AppliedProjectTransaction, ProjectActionIdempotencyKey, ProjectAsset,
    ProjectAssetChangeRequest, ProjectAssetImportSource, ProjectAssetKind, ProjectAssetOperation,
    ProjectOperation, ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, DescribeAssetToolResult, ListAssetsToolResult,
    ProjectAssetOnTheWire,
};
use uuid::Uuid;

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
struct ImportAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Where the bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    source: String,
    #[schemars(
        description = "Where the asset sits in the project, `/`-separated and relative to the project directory, such as `media/intro.mp4`."
    )]
    asset_path: String,
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
struct RenameAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
    #[schemars(description = "The asset's new file name, in the folder it sits in.")]
    file_name: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct MoveAssetToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    asset: String,
    #[schemars(
        description = "The project folder the asset moves into, `/`-separated, `\"\"` for the project's root."
    )]
    into_directory: String,
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

#[tool_router(router = project_asset_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "List a project's assets, ordered by path: each by its `asset` UUID, its `kind` (`stream`, `media`, `scene` or `data`), its `path` in the project, the SHA-256 and size of its bytes, and its `import_settings`, with the project's UUID, `project_name`, `project_description`, `project_cover` — the UUID of the image asset it is shown by — and `project_last_edited_at`, when its history last changed in seconds since the Unix epoch, each `null` when it has none, and `transactions_resource_uri`, its change feed. `kind` narrows to one kind. Refused for a folder with no `tatolab-project.json`."
    )]
    async fn list_assets(
        &self,
        Parameters(ListAssetsToolArguments {
            project_directory,
            kind,
        }): Parameters<ListAssetsToolArguments>,
    ) -> ToolCallAnswer {
        let kind = kind.map(ProjectAssetKind::from);
        let listed = self
            .a_project_repository_call_off_the_async_runtime(
                "list_assets",
                project_directory,
                move |project_repository| {
                    let mut assets: Vec<ProjectAssetOnTheWire> = project_repository
                        .assets()?
                        .iter()
                        .filter(|asset| kind.is_none_or(|kind| asset.kind == kind))
                        .map(project_asset_on_the_wire)
                        .collect();
                    assets.sort_by(|first, second| first.path.cmp(&second.path));
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
        description = "Describe one asset of a project: its UUID, kind, path, the SHA-256 and size of its bytes, its import settings, and `file_at_its_path`, whether a file is at its path."
    )]
    async fn describe_asset(
        &self,
        Parameters(DescribeAssetToolArguments {
            project_directory,
            asset,
        }): Parameters<DescribeAssetToolArguments>,
    ) -> ToolCallAnswer {
        let asset = the_asset_a_call_names("describe_asset", &asset)?;
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
                        asset: project_asset_on_the_wire(&described_asset),
                    })
                },
            )
            .await?;
        serialized_tool_result("describe_asset", &described)
    }

    #[tool(
        description = "Import a file as a new asset of the project at `asset_path`, answering its `asset` with the UUID the runtime made for it, which every later rename and move keeps, and the `transaction` and `commit` that record it. The bytes are copied once, to `asset_path`, and only the asset's sidecar, `<asset_path>.asset.json`, enters history. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there; a file already at `asset_path` is imported in place. Undoing the import removes the file at `asset_path`, one imported in place included, and nothing keeps its bytes. Refused whole, with nothing written, for a path another asset or another file holds, a path outside the project, and a stream, which is never imported."
    )]
    async fn import_asset(
        &self,
        peer: Peer<RoleServer>,
        Parameters(ImportAssetToolArguments {
            project_directory,
            source,
            asset_path,
            kind,
            import_settings,
            author,
            idempotency_key,
        }): Parameters<ImportAssetToolArguments>,
    ) -> ToolCallAnswer {
        let change = ProjectAssetChangeRequest::ImportAsset {
            source: project_asset_import_source(&source)?,
            asset_path,
            kind: kind.map(ProjectAssetKind::from),
            import_settings: import_settings.unwrap_or_default(),
        };
        self.one_asset_change_as_a_transaction(
            "import_asset",
            project_directory,
            the_author_of_a_change(author, &peer),
            idempotency_key,
            change,
        )
        .await
    }

    #[tool(
        description = "Give an asset a new file name in its folder, its sidecar going with it, as one transaction; the asset keeps its UUID. Answers the asset as it now is, with the `transaction` and `commit`."
    )]
    async fn rename_asset(
        &self,
        peer: Peer<RoleServer>,
        Parameters(RenameAssetToolArguments {
            project_directory,
            asset,
            file_name,
            author,
            idempotency_key,
        }): Parameters<RenameAssetToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "rename_asset",
            project_directory,
            the_author_of_a_change(author, &peer),
            idempotency_key,
            ProjectAssetChangeRequest::RenameAsset {
                asset: the_asset_a_call_names("rename_asset", &asset)?,
                file_name,
            },
        )
        .await
    }

    #[tool(
        description = "Move an asset into another folder of the project, made when missing, its sidecar going with it, as one transaction; the asset keeps its UUID. Answers the asset as it now is, with the `transaction` and `commit`."
    )]
    async fn move_asset(
        &self,
        peer: Peer<RoleServer>,
        Parameters(MoveAssetToolArguments {
            project_directory,
            asset,
            into_directory,
            author,
            idempotency_key,
        }): Parameters<MoveAssetToolArguments>,
    ) -> ToolCallAnswer {
        self.one_asset_change_as_a_transaction(
            "move_asset",
            project_directory,
            the_author_of_a_change(author, &peer),
            idempotency_key,
            ProjectAssetChangeRequest::MoveAsset {
                asset: the_asset_a_call_names("move_asset", &asset)?,
                into_directory,
            },
        )
        .await
    }

    #[tool(
        description = "Stop tracking an asset as one transaction: its file and sidecar leave the project, and nothing keeps its bytes, so undoing the transaction brings back its sidecar and the asset reads missing until its file is at its path again. A stream asset's graph document leaves the project, recorded in the transaction so an undo writes it back, and its stream is unloaded and forgotten. Refused while a stream's graph document references the asset, the refusal naming each document. Answers the asset as it was, with the `transaction` and `commit`."
    )]
    async fn delete_asset(
        &self,
        peer: Peer<RoleServer>,
        Parameters(DeleteAssetToolArguments {
            project_directory,
            asset,
            author,
            idempotency_key,
        }): Parameters<DeleteAssetToolArguments>,
    ) -> ToolCallAnswer {
        let (deleted, project, applied_now) = self
            .an_asset_change_applied_as_a_transaction(
                "delete_asset",
                project_directory,
                the_author_of_a_change(author, &peer),
                idempotency_key,
                ProjectAssetChangeRequest::DeleteAsset {
                    asset: the_asset_a_call_names("delete_asset", &asset)?,
                },
            )
            .await?;
        if applied_now
            && deleted.asset.kind == project_asset_kind_on_the_wire(ProjectAssetKind::Stream)
        {
            let stream_asset = the_asset_a_call_names("delete_asset", &deleted.asset.asset)?;
            self.a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams
                    .forget_the_stream_of_a_deleted_stream_asset(project, stream_asset)
            })
            .await
            .map_err(|not_forgotten| {
                format!(
                    "delete_asset applied its transaction, and the stream of the asset was not \
                     unloaded: {not_forgotten}"
                )
            })?;
        }
        serialized_tool_result("delete_asset", &deleted)
    }

    #[tool(
        description = "Replace an asset's import settings whole, as one transaction. Answers the asset as it now is, with the `transaction` and `commit`."
    )]
    async fn set_asset_settings(
        &self,
        peer: Peer<RoleServer>,
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
            the_author_of_a_change(author, &peer),
            idempotency_key,
            ProjectAssetChangeRequest::SetAssetSettings {
                asset: the_asset_a_call_names("set_asset_settings", &asset)?,
                import_settings,
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

    /// Apply `change` by `author` to the project as one transaction, answering the asset it changed.
    async fn one_asset_change_as_a_transaction(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        change: ProjectAssetChangeRequest,
    ) -> ToolCallAnswer {
        let (answered, _project, _applied_now) = self
            .an_asset_change_applied_as_a_transaction(
                tool_name,
                project_directory,
                author,
                idempotency_key,
                change,
            )
            .await?;
        serialized_tool_result(tool_name, &answered)
    }

    /// Apply `change` by `author` to the project as one transaction, answering the asset it
    /// changed, the project's UUID and whether this call applied it; sent again under
    /// `idempotency_key`, it answers the transaction that key first applied.
    async fn an_asset_change_applied_as_a_transaction(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        change: ProjectAssetChangeRequest,
    ) -> Result<(AssetTransactionToolResult, Uuid, bool), String> {
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        self.a_project_repository_call_off_the_async_runtime(
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
                let changed_asset = match transaction.operations.first() {
                    Some(ProjectOperation::Asset(
                        ProjectAssetOperation::ImportAsset { asset }
                        | ProjectAssetOperation::DeleteAsset { asset },
                    )) => asset.clone(),
                    Some(ProjectOperation::Asset(
                        ProjectAssetOperation::RelocateAsset { asset, .. }
                        | ProjectAssetOperation::SetAssetSettings { asset, .. },
                    )) => project_repository.asset(*asset)?,
                    Some(
                        ProjectOperation::StreamGraph(_)
                        | ProjectOperation::Import(_)
                        | ProjectOperation::StreamDocument(_)
                        | ProjectOperation::ProjectSettings(_),
                    )
                    | None => {
                        return Err(ProjectRepositoryRefusal::EmptyTransaction);
                    }
                };
                Ok((
                    AssetTransactionToolResult {
                        asset: project_asset_on_the_wire(&changed_asset),
                        transaction: transaction.transaction.to_string(),
                        commit,
                    },
                    project_repository.project_file().project,
                    applied_now,
                ))
            },
        )
        .await
    }
}

/// The author a change names: the caller's own, or the agent its MCP client is.
pub(crate) fn the_author_of_a_change(
    author: Option<ProjectTransactionAuthorToolArgument>,
    peer: &Peer<RoleServer>,
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
            name: peer
                .peer_info()
                .map(|client| client.client_info.name.clone())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| AUTHOR_NAME_OF_AN_UNNAMED_MCP_CLIENT.to_owned()),
        },
    }
}

/// The asset UUID a call's `asset` spells.
fn the_asset_a_call_names(tool_name: &str, asset: &str) -> Result<Uuid, String> {
    Uuid::parse_str(asset)
        .map_err(|_| format!("{tool_name} failed: `asset` must be an asset's UUID; got `{asset}`"))
}

/// `source` as an import reads it: an absolute path, or a content hash the store holds.
fn project_asset_import_source(source: &str) -> Result<ProjectAssetImportSource, String> {
    let source_path = std::path::Path::new(source);
    if source_path.is_absolute() {
        return Ok(ProjectAssetImportSource::FileOnThisMachine(
            source_path.to_path_buf(),
        ));
    }
    if source.len() == 64
        && source
            .bytes()
            .all(|digit| digit.is_ascii_digit() || (b'a'..=b'f').contains(&digit))
    {
        return Ok(ProjectAssetImportSource::UploadedContent(source.to_owned()));
    }
    Err(format!(
        "import_asset failed: `source` must be an absolute path or a SHA-256 in 64 lowercase hex digits; got `{source}`"
    ))
}

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
