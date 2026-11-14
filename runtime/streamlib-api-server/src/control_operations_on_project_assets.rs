// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The operations on a project's assets: listed, described, imported, renamed, moved, deleted and
//! given new settings, and its streams created, deleted and named, each change one transaction
//! on the project's history.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as STANDARD_BASE64;
use streamlib::sdk::error::Result as StreamlibResult;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, StreamActionQueued};
use streamlib_asset_media_facts::{
    AssetMediaFactsCacheOfAProject, AssetMediaFactsCachedInTheProject,
};
use streamlib_project_repository::{
    AppliedProjectTransaction, GRAPH_DOCUMENT_FILE_SUFFIX, ProjectActionIdempotencyKey,
    ProjectAsset, ProjectAssetAction, ProjectAssetImportSource, ProjectAssetKind,
    ProjectAssetOperation, ProjectOperation, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, DescribeAssetToolResult, ListAssetsToolResult,
    ProjectAssetOnTheWire, ProjectAssetWithItsMediaFactsOnTheWire,
    ProjectTransactionsResourceContents, ReadAssetPreviewToolResult,
};
use uuid::Uuid;

use crate::asset_media_facts_reads::AssetMediaFactsNow;
use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};
use crate::mcp_project_change_feed::{
    project_transactions_resource_uri, the_change_feed_of_the_project,
};
use streamlib_control_contract::{
    CreateStreamRequest, DeleteAssetRequest, DeleteStreamRequest, DescribeAssetRequest,
    ImportAssetRequest, ListAssetsRequest, ReadAssetPreviewRequest, RelocateAssetRequest,
    ReplaceAssetRequest, SetAssetSettingsRequest, SetStreamDisplayNameRequest,
};

/// The key a caller sent, refused when blank.
pub(crate) fn the_idempotency_key_a_caller_sent(
    idempotency_key: Option<String>,
) -> Result<Option<ProjectActionIdempotencyKey>, ProjectRepositoryRefusal> {
    idempotency_key
        .map(ProjectActionIdempotencyKey::from_caller)
        .transpose()
}

impl ControlOperationDispatcher {
    pub(crate) async fn read_project_changes(
        &self,
        project_directory: PathBuf,
    ) -> Result<ProjectTransactionsResourceContents, ControlOperationRefusal> {
        let project_actions_refused = Arc::clone(&self.project_actions_refused);
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads);
        self.a_project_repository_call_off_the_async_runtime(
            "read_project_changes",
            project_directory,
            move |project_repository| {
                the_change_feed_of_the_project(
                    project_repository,
                    &project_actions_refused,
                    &asset_media_facts_reads,
                )
            },
        )
        .await
    }

    pub(crate) async fn list_assets(
        &self,
        ListAssetsRequest {
            project_directory,
            kind,
        }: ListAssetsRequest,
    ) -> Result<ListAssetsToolResult, ControlOperationRefusal> {
        let kind = kind.map(ProjectAssetKind::from);
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads);
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
        Ok(listed)
    }

    pub(crate) async fn describe_asset(
        &self,
        DescribeAssetRequest {
            project_directory,
            asset,
        }: DescribeAssetRequest,
    ) -> Result<DescribeAssetToolResult, ControlOperationRefusal> {
        let asset = the_asset_a_call_names("describe_asset", "asset", &asset)?;
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads);
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
        Ok(described)
    }

    pub(crate) async fn read_asset_preview(
        &self,
        ReadAssetPreviewRequest {
            project_directory,
            asset,
        }: ReadAssetPreviewRequest,
    ) -> Result<ReadAssetPreviewToolResult, ControlOperationRefusal> {
        let asset = the_asset_a_call_names("read_asset_preview", "asset", &asset)?;
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads);
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
                        previewed_asset
                            .path
                            .on_disk_under(project_repository.project_directory()),
                        previewed_asset.content_sha256,
                    ))
                },
            )
            .await?;
        let (media_facts_now, project_directory, asset_file_path, content_sha256) = previewed;
        let preview = match media_facts_now {
            AssetMediaFactsNow::NotRead => {
                return Err(format!(
                    "read_asset_preview failed: asset {asset} is a stream, which has no preview; `graph` serves its graph"
                ).into());
            }
            AssetMediaFactsNow::Reading => {
                return Err(format!(
                    "read_asset_preview failed: the runtime is still reading asset {asset}'s bytes; read again once `describe_asset` answers its `media_facts_state` as `read`"
                ).into());
            }
            AssetMediaFactsNow::NoFileAtItsPath => {
                return Err(format!(
                    "read_asset_preview failed: no file is at asset {asset}'s path"
                )
                .into());
            }
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Unreadable {
                media_facts_unreadable_because,
            }) => {
                return Err(format!(
                    "read_asset_preview failed: asset {asset}'s bytes did not read, so it has no preview: {media_facts_unreadable_because}"
                ).into());
            }
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Read {
                preview: None,
                ..
            }) => {
                return Err(format!(
                    "read_asset_preview failed: asset {asset} has no preview picture; the runtime makes one for an image"
                ).into());
            }
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Read {
                preview: Some(preview),
                ..
            }) => preview,
        };
        let png_bytes = tokio::task::spawn_blocking(move || {
            AssetMediaFactsCacheOfAProject::of_the_project_at(&project_directory)
                .preview_png_bytes(&content_sha256, &asset_file_path)
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
        Ok(ReadAssetPreviewToolResult {
            asset: asset.to_string(),
            pixel_width: preview.pixel_width,
            pixel_height: preview.pixel_height,
            media_type: preview.media_type,
            byte_len: png_bytes.len() as u64,
            content_base64: STANDARD_BASE64.encode(&png_bytes),
        })
    }

    pub(crate) async fn import_asset(
        &self,
        ImportAssetRequest {
            project_directory,
            source,
            asset_path,
            file_name,
            kind,
            import_settings,
            author,
            idempotency_key,
        }: ImportAssetRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
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
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            change,
        )
        .await
    }

    pub(crate) async fn relocate_asset(
        &self,
        RelocateAssetRequest {
            project_directory,
            asset,
            to_path,
            author,
            idempotency_key,
        }: RelocateAssetRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "relocate_asset",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectAssetAction::RelocateAsset {
                asset: the_asset_a_call_names("relocate_asset", "asset", &asset)?,
                to_path,
            },
        )
        .await
    }

    pub(crate) async fn delete_asset(
        &self,
        DeleteAssetRequest {
            project_directory,
            asset,
            author,
            idempotency_key,
        }: DeleteAssetRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "delete_asset",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectAssetAction::DeleteAsset {
                asset: the_asset_a_call_names("delete_asset", "asset", &asset)?,
            },
        )
        .await
    }

    pub(crate) async fn set_asset_settings(
        &self,
        SetAssetSettingsRequest {
            project_directory,
            asset,
            import_settings,
            author,
            idempotency_key,
        }: SetAssetSettingsRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "set_asset_settings",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectAssetAction::SetAssetSettings {
                asset: the_asset_a_call_names("set_asset_settings", "asset", &asset)?,
                import_settings,
            },
        )
        .await
    }
    pub(crate) async fn replace_asset(
        &self,
        ReplaceAssetRequest {
            project_directory,
            asset,
            source,
            author,
            idempotency_key,
        }: ReplaceAssetRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "replace_asset",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectAssetAction::ReplaceAsset {
                asset: the_asset_a_call_names("replace_asset", "asset", &asset)?,
                source: ProjectAssetImportSource::try_from(source)
                    .map_err(|refusal| format!("replace_asset failed: {refusal}"))?,
            },
        )
        .await
    }

    pub(crate) async fn create_stream(
        &self,
        CreateStreamRequest {
            project_directory,
            path,
            display_name,
            author,
            idempotency_key,
        }: CreateStreamRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "create_stream",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectAssetAction::CreateStream { path, display_name },
        )
        .await
    }

    pub(crate) async fn delete_stream(
        &self,
        DeleteStreamRequest {
            project_directory,
            stream_asset,
            author,
            idempotency_key,
        }: DeleteStreamRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "delete_stream",
            project_directory,
            the_author_of_a_change(author, request_origin),
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

    pub(crate) async fn set_stream_display_name(
        &self,
        SetStreamDisplayNameRequest {
            project_directory,
            stream_asset,
            display_name,
            author,
            idempotency_key,
        }: SetStreamDisplayNameRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        self.one_asset_change_as_a_transaction(
            "set_stream_display_name",
            project_directory,
            the_author_of_a_change(author, request_origin),
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

impl ControlOperationDispatcher {
    /// Apply `change` by `author` to the project as one transaction, answering the asset it
    /// changed; a transaction applied now also brings the loaded streams in line with each graph
    /// document it wrote or took out. Sent again under `idempotency_key`, it answers the
    /// transaction that key first applied.
    async fn one_asset_change_as_a_transaction(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        change: ProjectAssetAction,
    ) -> Result<AssetTransactionToolResult, ControlOperationRefusal> {
        let project_repositories = Arc::clone(&self.project_repositories);
        let asset_media_facts_reads = Arc::clone(&self.asset_media_facts_reads);
        let (mut answered, project, project_directory, transaction_applied_now) = self
            .a_project_repository_call_off_the_async_runtime(
                method_name,
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
                        "{method_name} applied its transaction, and the loaded streams were not \
                         brought in line with it: {not_in_line}"
                    )
                })?;
            answered.action = loads_queued
                .into_iter()
                .next()
                .map(|queued| queued.action_id);
        }
        Ok(answered)
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

/// The asset UUID a call spells in its argument `argument_name`.
fn the_asset_a_call_names(
    method_name: &str,
    argument_name: &str,
    asset: &str,
) -> Result<Uuid, String> {
    Uuid::parse_str(asset).map_err(|_| {
        format!("{method_name} failed: `{argument_name}` must be an asset's UUID; got `{asset}`")
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
