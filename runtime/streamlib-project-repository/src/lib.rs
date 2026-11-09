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
mod project_import_copy;
mod project_import_folders;
#[cfg(feature = "history")]
mod project_pyproject_import_sources;
#[cfg(feature = "history")]
mod project_repositories_open_in_this_runtime;
#[cfg(feature = "history")]
mod project_repository;
mod project_repository_refusal;
mod project_stream_document;
mod project_transaction;
mod stream_fragment_references;
mod stream_graph_document;

pub use canonical_project_json::canonical_project_json_bytes;
pub use project_asset::{
    ASSET_SIDECAR_FILE_SUFFIX, GRAPH_DOCUMENT_FILE_SUFFIX, PROJECT_IMPORTS_DIRECTORY, ProjectAsset,
    ProjectAssetKind, ProjectAssetSidecar, ProjectRelativeAssetPath,
    every_asset_uuid_the_project_holds, the_content_path_of_an_asset,
};
pub use project_content_store::{
    PROJECT_CONTENT_STORE_DIRECTORY, ProjectContentStore, StoredProjectContent,
};
pub use project_file::{
    PROJECT_FILE_NAME, ProjectImport, THIS_RUNTIME_VERSION, TatolabProjectFile,
};
#[cfg(feature = "history")]
pub use project_history_repository::{
    PROJECT_HISTORY_BRANCH, PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER,
    ProjectHistoryCommit, ProjectHistoryRepository,
};
pub use project_import_folders::{the_file_of_an_imported_asset, the_folder_of_an_import};
#[cfg(feature = "history")]
pub use project_repositories_open_in_this_runtime::{
    ProjectRepositoriesOpenInThisRuntime, ProjectRepositoryOneWriterAtATime,
    ProjectTransactionAnnouncement,
};
#[cfg(feature = "history")]
pub use project_repository::{ProjectImportListed, ProjectRepository};
pub use project_repository_refusal::ProjectRepositoryRefusal;
pub use project_stream_document::{
    ProjectStreamDocument, STREAM_DOCUMENT_ASSET_KEY, project_stream_documents,
};
pub use project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectImportFiles, ProjectImportOperation, ProjectOperation,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, ProjectTransactionReversal,
    StreamGraphOperationOnAStream,
};
pub use stream_fragment_references::{
    graph_document_holds_a_stream_fragment,
    graph_document_with_each_stream_fragment_reference_resolved,
    the_fragment_a_stream_fragment_places, the_stream_document_of_an_import,
};
pub use stream_graph_document::{
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentGroup, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphFragmentPlacement, StreamGraphGroupFields,
    StreamGraphOperation, asset_references_made_references_into_the_import,
    assets_a_node_config_references, fragment_with_each_node_uuid_kept_by_its_name,
};

/// The directory inside a project that holds what the runtime keeps for it: history, content and
/// staged imports.
pub const PROJECT_RUNTIME_DIRECTORY: &str = ".tatolab";
