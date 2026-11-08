// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use uuid::Uuid;

/// Why the runtime refused to open a project or to apply a transaction to it, in its own words.
#[derive(Debug, thiserror::Error)]
pub enum ProjectRepositoryRefusal {
    #[error("{project_directory} holds no project: it has no {project_file_name}")]
    NotAProject {
        project_directory: PathBuf,
        project_file_name: &'static str,
    },
    #[error("{project_directory} already holds a project")]
    AlreadyAProject { project_directory: PathBuf },
    #[error("the project in {project_directory} already has a first commit")]
    ProjectAlreadyCommitted { project_directory: PathBuf },
    #[error("the project directory {0} is not an absolute path")]
    ProjectDirectoryNotAbsolute(PathBuf),
    #[error("the project file {path} could not be read: {why}")]
    ProjectFileUnreadable { path: PathBuf, why: String },
    #[error("the asset sidecar {path} could not be read: {why}")]
    AssetSidecarUnreadable { path: PathBuf, why: String },
    #[error("`{first}` and `{second}` both name the asset {asset}")]
    AssetNamedTwice {
        asset: Uuid,
        first: String,
        second: String,
    },
    #[error("`{path}` is not an asset path: {why}")]
    AssetPathRefused { path: String, why: &'static str },
    #[error("the project holds no asset {0}")]
    NoSuchAsset(Uuid),
    #[error("`{path}` is already the path of the asset {holder}")]
    AssetPathHeldByAnotherAsset { path: String, holder: Uuid },
    #[error("`{path}` already holds a file that is not the import's source")]
    AssetPathHoldsAnotherFile { path: String },
    #[error("a stream asset is a graph document the stream verbs create; it is never imported")]
    StreamAssetsAreNotImported,
    #[error("the stream asset {0} has no import settings: its settings are in its graph document")]
    StreamAssetHasNoImportSettings(Uuid),
    #[error("the asset {asset} cannot be deleted while {referencing_documents:?} reference it")]
    AssetStillReferenced {
        asset: Uuid,
        referencing_documents: Vec<String>,
    },
    #[error("the project holds no stream asset {0}")]
    NoSuchStream(Uuid),
    #[error("the graph document {path} refused the change: {why}")]
    GraphOperationRefused { path: String, why: String },
    #[error("node {node}'s config references the asset {asset}, which the project does not hold")]
    ConfigReferencesNoAsset { node: Uuid, asset: Uuid },
    #[error("{author} has no transaction left to undo")]
    NothingToUndo { author: String },
    #[error("{author} has undone nothing that is left to redo")]
    NothingToRedo { author: String },
    #[error("the import source {path} could not be read: {why}")]
    ImportSourceUnreadable { path: PathBuf, why: String },
    #[error("the project's content store holds no content {0}")]
    ContentNotInTheStore(String),
    #[error("a transaction must hold at least one change")]
    EmptyTransaction,
    #[error("the transaction's author needs a name")]
    AuthorWithoutAName,
    #[error("the history remote {url} refused the push: {why}")]
    HistoryRemoteRefused { url: String, why: String },
    #[error("the graph document {path} could not be read, so what it references is unknown: {why}")]
    GraphDocumentUnreadable { path: String, why: String },
    #[error("`{name}` cannot be both a tracked file and a tracked folder in history")]
    TrackedFileAndFolderShareAName { name: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[cfg(feature = "history")]
    #[error("the project's history refused the change: {0}")]
    History(#[from] git2::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}
