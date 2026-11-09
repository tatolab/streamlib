// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::project_asset::{ProjectAsset, ProjectAssetKind, ProjectRelativeAssetPath};
use crate::project_file::PinnedProjectDependency;
use crate::stream_graph_document::StreamGraphOperation;

/// Who a transaction is by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTransactionAuthorKind {
    /// A person, through the studio or the CLI.
    Person,
    /// An agent, over MCP.
    Agent,
    /// The runtime itself.
    Runtime,
}

/// The author a transaction names, which undo and history follow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTransactionAuthor {
    /// What kind of author it is.
    pub kind: ProjectTransactionAuthorKind,
    /// The author's name as history shows it.
    pub name: String,
}

impl ProjectTransactionAuthor {
    /// The runtime as an author, for the commits it makes on its own.
    pub fn the_runtime() -> Self {
        Self {
            kind: ProjectTransactionAuthorKind::Runtime,
            name: "tatolab runtime".to_owned(),
        }
    }

    /// The e-mail a commit carries, which Git requires and which names the author's kind.
    #[cfg(feature = "history")]
    pub(crate) fn commit_email(&self) -> String {
        let kind = match self.kind {
            ProjectTransactionAuthorKind::Person => "person",
            ProjectTransactionAuthorKind::Agent => "agent",
            ProjectTransactionAuthorKind::Runtime => "runtime",
        };
        format!("{kind}@tatolab.invalid")
    }
}

/// Where an import's bytes come from; they reach the content store before the transaction, never inside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAssetImportSource {
    /// A file on this machine, by absolute path.
    FileOnThisMachine(PathBuf),
    /// Bytes an upload already put in the content store, by their SHA-256.
    StoredContent(String),
}

/// One change a caller asks of a project's assets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum ProjectAssetChangeRequest {
    /// Track new bytes as an asset at `asset_path`, of `kind` or the kind its extension implies.
    ImportAsset {
        source: ProjectAssetImportSource,
        asset_path: String,
        kind: Option<ProjectAssetKind>,
        #[serde(default)]
        import_settings: Map<String, Value>,
    },
    /// Give an asset another file name in its folder.
    RenameAsset { asset: Uuid, file_name: String },
    /// Put an asset in another folder of the project, `""` being its root.
    MoveAsset { asset: Uuid, into_directory: String },
    /// Stop tracking an asset; its bytes stay in the content store.
    DeleteAsset { asset: Uuid },
    /// Replace an asset's import settings.
    SetAssetSettings {
        asset: Uuid,
        import_settings: Map<String, Value>,
    },
}

/// One change a transaction made, recorded with everything its inverse needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ProjectAssetOperation {
    /// The asset began to be tracked.
    ImportAsset { asset: ProjectAsset },
    /// The asset's path changed.
    RelocateAsset {
        asset: Uuid,
        from: ProjectRelativeAssetPath,
        to: ProjectRelativeAssetPath,
    },
    /// The asset stopped being tracked.
    DeleteAsset { asset: ProjectAsset },
    /// The asset's import settings changed, the asset then at `path`.
    SetAssetSettings {
        asset: Uuid,
        path: ProjectRelativeAssetPath,
        from: Map<String, Value>,
        to: Map<String, Value>,
    },
}

impl ProjectAssetOperation {
    /// The operation that undoes this one.
    pub fn inverse(&self) -> Self {
        match self {
            Self::ImportAsset { asset } => Self::DeleteAsset {
                asset: asset.clone(),
            },
            Self::DeleteAsset { asset } => Self::ImportAsset {
                asset: asset.clone(),
            },
            Self::RelocateAsset { asset, from, to } => Self::RelocateAsset {
                asset: *asset,
                from: to.clone(),
                to: from.clone(),
            },
            Self::SetAssetSettings {
                asset,
                path,
                from,
                to,
            } => Self::SetAssetSettings {
                asset: *asset,
                path: path.clone(),
                from: to.clone(),
                to: from.clone(),
            },
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::ImportAsset { asset } => format!("import_asset {}", asset.path),
            Self::DeleteAsset { asset } => format!("delete_asset {}", asset.path),
            Self::RelocateAsset { from, to, .. } => format!("relocate_asset {from} -> {to}"),
            Self::SetAssetSettings { asset, .. } => format!("set_asset_settings {asset}"),
        }
    }
}

/// One change to the projects this one depends on, recorded with everything its inverse needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ProjectDependencyOperation {
    /// The project began to be depended on, pinned to a commit.
    AddDependency { dependency: PinnedProjectDependency },
    /// The project stopped being depended on.
    RemoveDependency { dependency: PinnedProjectDependency },
    /// The dependency's pin moved from one commit to another.
    MoveDependencyPin {
        project: Uuid,
        from: String,
        to: String,
    },
}

impl ProjectDependencyOperation {
    /// The operation that undoes this one.
    pub fn inverse(&self) -> Self {
        match self.clone() {
            Self::AddDependency { dependency } => Self::RemoveDependency { dependency },
            Self::RemoveDependency { dependency } => Self::AddDependency { dependency },
            Self::MoveDependencyPin { project, from, to } => Self::MoveDependencyPin {
                project,
                from: to,
                to: from,
            },
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::AddDependency { dependency } => format!(
                "add_dependency {} at {}",
                dependency.project, dependency.revision
            ),
            Self::RemoveDependency { dependency } => {
                format!("remove_dependency {}", dependency.project)
            }
            Self::MoveDependencyPin { project, to, .. } => {
                format!("update_dependency {project} to {to}")
            }
        }
    }
}

/// One change to one stream's graph: the stream by its asset's UUID, and the operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamGraphOperationOnAStream {
    /// The stream asset whose graph document the operation edits.
    pub stream: Uuid,
    /// What it changed.
    #[serde(flatten)]
    pub graph_operation: StreamGraphOperation,
}

/// One change a transaction made to a project: to its assets, or to a stream's graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProjectOperation {
    /// A change to the project's assets.
    Asset(ProjectAssetOperation),
    /// A change to one stream's graph.
    StreamGraph(StreamGraphOperationOnAStream),
    /// A change to the projects this one depends on.
    Dependency(ProjectDependencyOperation),
}

impl ProjectOperation {
    /// The operation that undoes this one.
    pub fn inverse(&self) -> Self {
        match self {
            Self::Asset(asset_operation) => Self::Asset(asset_operation.inverse()),
            Self::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) => Self::StreamGraph(StreamGraphOperationOnAStream {
                stream: *stream,
                graph_operation: graph_operation.inverse(),
            }),
            Self::Dependency(dependency_operation) => {
                Self::Dependency(dependency_operation.inverse())
            }
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Asset(asset_operation) => asset_operation.summary(),
            Self::StreamGraph(on_a_stream) => on_a_stream.graph_operation.summary(),
            Self::Dependency(dependency_operation) => dependency_operation.summary(),
        }
    }
}

/// What a transaction is to the ones before it, which undo and redo follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectTransactionLineage {
    /// A new edit, which clears what its author could redo.
    NewEdit,
    /// The undo of the transaction this names.
    UndoOf(Uuid),
    /// The redo of the transaction this names, itself an edit or a redo.
    RedoOf(Uuid),
}

/// The operations an undo or a redo applies, and what it is to the transaction it reverses.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectTransactionReversal {
    /// What the undo or redo changes, in order.
    pub operations: Vec<ProjectOperation>,
    /// The undo or redo it is.
    pub lineage: ProjectTransactionLineage,
}

/// A batch of operations applied whole: the unit of undo and of history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectTransaction {
    /// The transaction's UUID.
    pub transaction: Uuid,
    /// Who made it.
    pub author: ProjectTransactionAuthor,
    /// The commit history was at when it applied, `None` for a project's first.
    pub parent_commit: Option<String>,
    /// What it changed, in order.
    pub operations: Vec<ProjectOperation>,
    /// What undoes it, in order.
    pub inverse_operations: Vec<ProjectOperation>,
    /// The transaction this one undoes, when it is an undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<Uuid>,
    /// The transaction this one redoes, when it is a redo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redoes: Option<Uuid>,
}

impl ProjectTransaction {
    /// What this transaction is to the ones before it.
    pub fn lineage(&self) -> ProjectTransactionLineage {
        match (self.undoes, self.redoes) {
            (Some(undone), _) => ProjectTransactionLineage::UndoOf(undone),
            (None, Some(redone)) => ProjectTransactionLineage::RedoOf(redone),
            (None, None) => ProjectTransactionLineage::NewEdit,
        }
    }
}

/// A transaction the runtime applied and the commit that records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppliedProjectTransaction {
    /// The transaction.
    pub transaction: ProjectTransaction,
    /// The commit on `main` that records it.
    pub commit: String,
}
