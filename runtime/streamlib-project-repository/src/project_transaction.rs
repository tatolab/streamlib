// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::project_asset::{ProjectAsset, ProjectAssetKind, ProjectRelativeAssetPath};
use crate::project_file::ProjectImport;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
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

/// Where an import's bytes come from; they reach the asset's path once, never the transaction.
/// On the wire it is one string: an absolute path, or an upload's SHA-256 in 64 lowercase hex
/// digits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum ProjectAssetImportSource {
    /// A file on this machine, by absolute path.
    FileOnThisMachine(PathBuf),
    /// Bytes an upload put in the project's uploads, by their SHA-256, which the import moves
    /// to the asset's path.
    UploadedContent(String),
}

impl TryFrom<String> for ProjectAssetImportSource {
    type Error = ProjectRepositoryRefusal;

    fn try_from(source: String) -> Result<Self, Self::Error> {
        if std::path::Path::new(&source).is_absolute() {
            return Ok(Self::FileOnThisMachine(PathBuf::from(source)));
        }
        let names_an_upload = source.len() == 64
            && source
                .bytes()
                .all(|digit| digit.is_ascii_digit() || (b'a'..=b'f').contains(&digit));
        if names_an_upload {
            return Ok(Self::UploadedContent(source));
        }
        Err(ProjectRepositoryRefusal::ImportSourceNeitherAPathNorAnUpload(source))
    }
}

impl From<ProjectAssetImportSource> for String {
    fn from(source: ProjectAssetImportSource) -> Self {
        match source {
            ProjectAssetImportSource::FileOnThisMachine(path) => {
                path.to_string_lossy().into_owned()
            }
            ProjectAssetImportSource::UploadedContent(content_sha256) => content_sha256,
        }
    }
}

/// One action as its caller sent it, of whichever family, each tagged by its `operation`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProjectAction {
    /// An action on the project's assets.
    Asset(ProjectAssetAction),
}

/// One action on a project's assets, as every caller sends it and history records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectAssetAction {
    /// Track new bytes as an asset at `asset_path`, of `kind` or the kind its extension implies.
    ImportAsset {
        source: ProjectAssetImportSource,
        asset_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ProjectAssetKind>,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        import_settings: Map<String, Value>,
    },
    /// Put an asset at another path of the project, its sidecar going with it.
    RelocateAsset { asset: Uuid, to_path: String },
    /// Stop tracking an asset; its file leaves the project and nothing keeps its bytes.
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

/// Where the files an import places come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectImportFiles {
    /// A copy staged inside the project's runtime folder before the transaction, by its path
    /// relative to the project; the transaction moves it into place.
    StagedCopy { staged_at: String },
    /// The import's folder as the project's history recorded it at `commit`.
    RecordedAtCommit { commit: String },
}

/// One change to the projects this one imports, recorded with what its inverse needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ProjectImportOperation {
    /// The import's folder was placed, replacing the import `replacing` when it names one.
    PlaceImport {
        import: ProjectImport,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacing: Option<ProjectImport>,
        files: ProjectImportFiles,
    },
    /// The import's folder was taken out.
    RemoveImport { import: ProjectImport },
}

impl ProjectImportOperation {
    /// The operation that undoes this one, applied when history is at `parent_commit`: the
    /// files it took out are read back from that commit.
    pub fn inverse_at(&self, parent_commit: &str) -> Self {
        let recorded_before = ProjectImportFiles::RecordedAtCommit {
            commit: parent_commit.to_owned(),
        };
        match self.clone() {
            Self::PlaceImport {
                import,
                replacing: None,
                ..
            } => Self::RemoveImport { import },
            Self::PlaceImport {
                import,
                replacing: Some(replaced),
                ..
            } => Self::PlaceImport {
                import: replaced,
                replacing: Some(import),
                files: recorded_before,
            },
            Self::RemoveImport { import } => Self::PlaceImport {
                import,
                replacing: None,
                files: recorded_before,
            },
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::PlaceImport {
                import,
                replacing: None,
                ..
            } => format!("add_dependency imports/{}", import.folder),
            Self::PlaceImport { import, .. } => {
                format!("update_dependency imports/{}", import.folder)
            }
            Self::RemoveImport { import } => {
                format!("remove_dependency imports/{}", import.folder)
            }
        }
    }
}

/// One stream asset's graph document written into the project or taken out of it whole,
/// recorded with the document so its inverse can restore it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ProjectStreamDocumentOperation {
    /// The stream asset's graph document was written at `path`.
    CreateStreamDocument {
        stream: Uuid,
        path: ProjectRelativeAssetPath,
        document: Value,
    },
    /// The stream asset's graph document at `path` was taken out.
    DeleteStreamDocument {
        stream: Uuid,
        path: ProjectRelativeAssetPath,
        document: Value,
    },
}

impl ProjectStreamDocumentOperation {
    /// The operation that undoes this one.
    pub fn inverse(&self) -> Self {
        match self.clone() {
            Self::CreateStreamDocument {
                stream,
                path,
                document,
            } => Self::DeleteStreamDocument {
                stream,
                path,
                document,
            },
            Self::DeleteStreamDocument {
                stream,
                path,
                document,
            } => Self::CreateStreamDocument {
                stream,
                path,
                document,
            },
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::CreateStreamDocument { path, .. } => format!("create_stream_document {path}"),
            Self::DeleteStreamDocument { path, .. } => format!("delete_stream_document {path}"),
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
    /// A change to the projects this one imports.
    Import(ProjectImportOperation),
    /// A stream asset's graph document written or taken out whole.
    StreamDocument(ProjectStreamDocumentOperation),
}

impl ProjectOperation {
    /// The operation that undoes this one, applied when history is at `parent_commit`.
    pub fn inverse_at(&self, parent_commit: &str) -> Self {
        match self {
            Self::Asset(asset_operation) => Self::Asset(asset_operation.inverse()),
            Self::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation,
            }) => Self::StreamGraph(StreamGraphOperationOnAStream {
                stream: *stream,
                graph_operation: graph_operation.inverse(),
            }),
            Self::Import(import_operation) => {
                Self::Import(import_operation.inverse_at(parent_commit))
            }
            Self::StreamDocument(stream_document_operation) => {
                Self::StreamDocument(stream_document_operation.inverse())
            }
        }
    }

    /// The operation as a commit's summary line names it.
    #[cfg(feature = "history")]
    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Asset(asset_operation) => asset_operation.summary(),
            Self::StreamGraph(on_a_stream) => on_a_stream.graph_operation.summary(),
            Self::Import(import_operation) => import_operation.summary(),
            Self::StreamDocument(stream_document_operation) => stream_document_operation.summary(),
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

/// The transaction format this runtime writes; it reads and undoes every older one.
pub const PROJECT_TRANSACTION_FORMAT_VERSION: u32 = 2;

fn project_transaction_format_version_before_it_was_recorded() -> u32 {
    1
}

/// A key the caller mints once per user gesture, so an action it sends again applies once.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct ProjectActionIdempotencyKey(String);

impl TryFrom<String> for ProjectActionIdempotencyKey {
    type Error = ProjectRepositoryRefusal;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        Self::from_caller(key)
    }
}

impl From<ProjectActionIdempotencyKey> for String {
    fn from(key: ProjectActionIdempotencyKey) -> Self {
        key.0
    }
}

impl ProjectActionIdempotencyKey {
    /// The key `key` names, refused when it is blank.
    pub fn from_caller(key: impl Into<String>) -> Result<Self, ProjectRepositoryRefusal> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(ProjectRepositoryRefusal::BlankIdempotencyKey);
        }
        Ok(Self(key))
    }

    /// The key as the caller sent it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectActionIdempotencyKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The part of a recorded transaction every format shares, read before the rest so a record
/// in a newer format is refused by name rather than by what it holds.
#[derive(Deserialize)]
pub(crate) struct ProjectTransactionEnvelopeHeader {
    #[serde(default = "project_transaction_format_version_before_it_was_recorded")]
    pub(crate) format_version: u32,
    pub(crate) transaction: Uuid,
    #[serde(default)]
    pub(crate) idempotency_key: Option<ProjectActionIdempotencyKey>,
}

/// A batch of operations applied whole: the unit of undo and of history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectTransaction {
    /// The format the transaction was recorded in, `1` for one recorded before it was written.
    #[serde(default = "project_transaction_format_version_before_it_was_recorded")]
    pub format_version: u32,
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
    /// The key the caller sent it under, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<ProjectActionIdempotencyKey>,
    /// The actions its caller sent, exactly as sent; empty for an undo, a redo, or a change the
    /// runtime made itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ProjectAction>,
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

/// What applying an action did: applied it now, or found it applied before under its key.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectActionApplyOutcome {
    /// Applied and committed by this call.
    AppliedNow(AppliedProjectTransaction),
    /// Applied by an earlier call under the same idempotency key; this call changed nothing.
    AppliedBeforeUnderItsKey(AppliedProjectTransaction),
}

impl ProjectActionApplyOutcome {
    /// Whether this call applied the transaction, rather than an earlier one under its key.
    pub fn was_applied_now(&self) -> bool {
        matches!(self, Self::AppliedNow(_))
    }

    /// The transaction, whichever call applied it.
    pub fn applied_transaction(&self) -> &AppliedProjectTransaction {
        match self {
            Self::AppliedNow(applied) | Self::AppliedBeforeUnderItsKey(applied) => applied,
        }
    }

    /// The transaction, whichever call applied it.
    pub fn into_applied_transaction(self) -> AppliedProjectTransaction {
        match self {
            Self::AppliedNow(applied) | Self::AppliedBeforeUnderItsKey(applied) => applied,
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
    /// When the commit was made, in whole seconds since the Unix epoch, for display only.
    pub applied_at_unix_seconds: i64,
}
