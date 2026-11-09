// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project as a versioned repository of assets, which the runtime alone writes.

mod canonical_project_json;
mod project_asset;
mod project_content_store;
mod project_file;
#[cfg(feature = "history")]
mod project_history_repository;
#[cfg(feature = "history")]
mod project_history_smart_http_transport;
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
    ASSET_SIDECAR_FILE_SUFFIX, GRAPH_DOCUMENT_FILE_SUFFIX, ProjectAsset, ProjectAssetKind,
    ProjectAssetSidecar, ProjectRelativeAssetPath, every_asset_uuid_the_project_holds,
    the_content_path_of_an_asset,
};
pub use project_content_store::{
    PROJECT_CONTENT_STORE_DIRECTORY, ProjectContentStore, StoredProjectContent,
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
pub use project_repository::{DependencyStreamFork, FORKED_FROM_KEY, ProjectRepository};
pub use project_repository_refusal::ProjectRepositoryRefusal;
pub use project_stream_document::{
    ProjectStreamDocument, STREAM_DOCUMENT_ASSET_KEY, project_stream_documents,
};
pub use project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectDependencyOperation, ProjectOperation,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, ProjectTransactionReversal,
    StreamGraphOperationOnAStream,
};
pub use stream_graph_document::{
    StreamGraphDocument, StreamGraphDocumentGroup, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd, StreamGraphDocumentNode, StreamGraphFragmentPlacement,
    StreamGraphGroupFields, StreamGraphOperation, a_graph_document_as_a_fragment_run,
    asset_references_made_references_into_the_dependency, assets_a_node_config_references,
    config_with_a_dependency_asset_reference_made_local,
    fragment_with_each_node_uuid_kept_by_its_name,
};

/// The directory inside a project that holds what the runtime keeps for it: history, content and dependencies.
pub const PROJECT_RUNTIME_DIRECTORY: &str = ".tatolab";
