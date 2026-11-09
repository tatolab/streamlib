// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project as a versioned repository of assets, which the runtime alone writes.

mod canonical_project_json;
mod project_asset;
#[cfg(feature = "history")]
mod project_content_lfs_remote;
mod project_content_store;
#[cfg(feature = "history")]
mod project_dependency_source;
mod project_file;
#[cfg(feature = "history")]
mod project_history_repository;
#[cfg(feature = "history")]
mod project_history_smart_http_transport;
#[cfg(feature = "history")]
mod project_remote_http_exchange;
#[cfg(feature = "history")]
mod project_repositories_open_in_this_runtime;
#[cfg(feature = "history")]
mod project_repository;
mod project_repository_refusal;
mod project_stream_document;
mod project_transaction;
mod stream_graph_document;

pub use canonical_project_json::canonical_project_json_bytes;
pub use project_asset::{
    ASSET_SIDECAR_FILE_SUFFIX, GRAPH_DOCUMENT_FILE_SUFFIX,
    PROJECT_DEPENDENCY_ASSET_RECORDS_DIRECTORY, ProjectAsset, ProjectAssetForkOrigin,
    ProjectAssetKind, ProjectAssetSidecar, ProjectRelativeAssetPath,
    every_asset_uuid_the_project_holds, the_content_path_of_a_dependency_asset,
    the_content_path_of_an_asset, the_dependency_asset_record_path,
    the_dependency_assets_recorded_at,
};
#[cfg(feature = "history")]
pub use project_content_lfs_remote::{PROJECT_LFS_CONFIG_FILE_NAME, ProjectContentRemote};
pub use project_content_store::{
    PROJECT_CONTENT_STORE_DIRECTORY, ProjectContentStore, StoredProjectContent,
};
#[cfg(feature = "history")]
pub use project_dependency_source::{
    PYTHON_PACKAGE_DEPENDENCY_SOURCE_PREFIX, ProjectDependencySource,
    the_project_directory_an_installed_package_ships,
};
pub use project_file::{PROJECT_FILE_NAME, PinnedProjectDependency, TatolabProjectFile};
#[cfg(feature = "history")]
pub use project_history_repository::{
    PROJECT_HISTORY_BRANCH, PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER,
    ProjectHistoryCommit, ProjectHistoryRemote, ProjectHistoryRepository,
};
#[cfg(feature = "history")]
pub use project_repositories_open_in_this_runtime::{
    ProjectRepositoriesOpenInThisRuntime, ProjectRepositoryOneWriterAtATime,
    ProjectTransactionAnnouncement,
};
#[cfg(feature = "history")]
pub use project_repository::{DependencyAssetFork, ProjectDependencyListed, ProjectRepository};
pub use project_repository_refusal::ProjectRepositoryRefusal;
pub use project_stream_document::{
    FORKED_FROM_KEY, ProjectStreamDocument, STREAM_DOCUMENT_ASSET_KEY, project_stream_documents,
    the_fork_origin_a_graph_document_records,
};
pub use project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectDependencyOperation, ProjectOperation,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, ProjectTransactionReversal,
    StreamGraphOperationOnAStream,
};
pub use stream_graph_document::{
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentGroup, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphFragmentPlacement, StreamGraphGroupFields,
    StreamGraphOperation, a_graph_document_as_a_fragment_run,
    asset_references_made_references_into_the_dependency, assets_a_node_config_references,
    config_with_a_dependency_asset_reference_made_local,
    fragment_with_each_node_uuid_kept_by_its_name, the_refresh_of_each_fragment_holding_a_stream,
};

/// The directory inside a project that holds what the runtime keeps for it: history, content and dependencies.
pub const PROJECT_RUNTIME_DIRECTORY: &str = ".tatolab";
