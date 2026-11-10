// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project as a versioned repository of assets, which the runtime alone writes.

mod canonical_project_json;
mod project_asset;
#[cfg(feature = "history")]
mod project_asset_file_bytes;
mod project_file;
#[cfg(feature = "history")]
mod project_history_repository;
#[cfg(feature = "history")]
mod project_import_copy;
mod project_import_folders;
mod project_pyproject_import_sources;
#[cfg(feature = "history")]
mod project_repositories_open_in_this_runtime;
#[cfg(feature = "history")]
mod project_repository;
mod project_repository_refusal;
mod project_stream_document;
mod project_transaction;
mod project_views;
mod stream_fragment_references;
mod stream_graph_action;
mod stream_graph_document;
mod tatolab_view_manifest;

pub use canonical_project_json::canonical_project_json_bytes;
pub use project_asset::{
    ASSET_SIDECAR_FILE_SUFFIX, GRAPH_DOCUMENT_FILE_SUFFIX, PROJECT_IMPORTS_DIRECTORY, ProjectAsset,
    ProjectAssetKind, ProjectAssetSidecar, ProjectRelativeAssetPath,
    every_asset_uuid_the_project_holds, the_file_of_an_asset,
};
#[cfg(feature = "history")]
pub use project_asset_file_bytes::{PROJECT_UPLOADS_DIRECTORY, the_upload_file_of};
pub use project_file::{
    PROJECT_FILE_NAME, ProjectImport, THIS_RUNTIME_VERSION, TatolabProjectFile,
};
#[cfg(feature = "history")]
pub use project_history_repository::{
    PROJECT_HISTORY_BRANCH, PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER,
    ProjectHistoryCommit, ProjectHistoryCommitWritten, ProjectHistoryRepository,
};
pub use project_import_folders::{the_file_of_an_imported_asset, the_folder_of_an_import};
pub use project_pyproject_import_sources::{
    FirstPartyModuleDeclaredByAProject, PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY,
    pyproject_with_each_first_party_module_as_a_uv_path_source,
};
#[cfg(feature = "history")]
pub use project_repositories_open_in_this_runtime::{
    ProjectChangeFeedAnnouncement, ProjectChangeOnTheFeed, ProjectRepositoriesOpenInThisRuntime,
    ProjectRepositoryOneWriterAtATime,
};
#[cfg(feature = "history")]
pub use project_repository::{
    ProjectImportListed, ProjectPythonRequirementsLock, ProjectRepository,
};
pub use project_repository_refusal::ProjectRepositoryRefusal;
pub use project_stream_document::{
    ProjectStreamDocument, STREAM_DOCUMENT_ASSET_KEY, STREAM_DOCUMENT_DISPLAY_NAME_KEY,
    project_stream_documents,
};
pub use project_transaction::{
    AppliedProjectTransaction, PROJECT_TRANSACTION_FORMAT_VERSION, ProjectAction,
    ProjectActionApplyOutcome, ProjectActionIdempotencyKey, ProjectAssetAction,
    ProjectAssetImportSource, ProjectAssetOperation, ProjectImportFiles, ProjectImportOperation,
    ProjectOperation, ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, ProjectTransactionReversal,
    ProjectTransactionUndoState, StreamGraphOperationOnAStream,
};
pub use project_views::{
    PROJECT_VIEWS_DIRECTORY, ProjectBringingAView, ProjectViewFolderRead,
    TATOLAB_VIEW_MANIFEST_FILE_NAME, every_view_a_project_and_its_imports_bring, the_view_type_of,
};
pub use stream_fragment_references::{
    graph_document_holds_a_stream_fragment,
    graph_document_with_each_stream_fragment_reference_resolved,
    the_fragment_a_stream_fragment_places, the_stream_document_of_an_import,
};
pub use stream_graph_action::{
    DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION, DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION,
    DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION, EXPOSED_CONFIG_FIELDS_ARGUMENT_DESCRIPTION,
    GRAPH_EDIT_NODE_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GROUP_ARGUMENT_DESCRIPTION, GROUP_COLOUR_ARGUMENT_DESCRIPTION,
    GROUP_DESCRIPTION_ARGUMENT_DESCRIPTION, GROUP_TITLE_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_CONFIG_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_GENERATOR_ARGUMENT_DESCRIPTION,
    IMPORT_GRAPH_GENERATOR_NAME_ARGUMENT_DESCRIPTION, StreamGraphAction, StreamGraphActionLinkEnd,
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
pub use tatolab_view_manifest::{
    TatolabViewAcceptedFiles, TatolabViewCapability, TatolabViewDeclaredPort, TatolabViewManifest,
    TatolabViewPlacement, VIEW_CONTRACT_READ_MAJOR, VIEW_CONTRACT_READ_MINOR,
    is_a_path_inside_the_views_folder, read_tatolab_view_manifest,
};
