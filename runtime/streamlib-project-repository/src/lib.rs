// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project as a versioned repository of assets, which the runtime alone writes.

mod canonical_project_json;
mod project_asset;
mod project_content_store;
mod project_file;
mod project_history_repository;
mod project_repository;
mod project_repository_refusal;
mod project_transaction;

pub use canonical_project_json::canonical_project_json_bytes;
pub use project_asset::{
    ASSET_SIDECAR_FILE_SUFFIX, GRAPH_DOCUMENT_FILE_SUFFIX, ProjectAsset, ProjectAssetKind,
    ProjectAssetSidecar, ProjectRelativeAssetPath,
};
pub use project_content_store::{
    PROJECT_CONTENT_STORE_DIRECTORY, ProjectContentStore, StoredProjectContent,
};
pub use project_file::{PROJECT_FILE_NAME, PinnedProjectDependency, TatolabProjectFile};
pub use project_history_repository::{
    PROJECT_HISTORY_BRANCH, PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER,
    ProjectHistoryCommit, ProjectHistoryRepository,
};
pub use project_repository::ProjectRepository;
pub use project_repository_refusal::ProjectRepositoryRefusal;
pub use project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind,
};

/// The directory inside a project that holds what the runtime keeps for it: history, content and dependencies.
pub const PROJECT_RUNTIME_DIRECTORY: &str = ".tatolab";
