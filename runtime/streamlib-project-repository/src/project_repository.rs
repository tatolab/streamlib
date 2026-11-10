// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use crate::PROJECT_RUNTIME_DIRECTORY;
use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_asset::{
    GRAPH_DOCUMENT_FILE_SUFFIX, PROJECT_IMPORTS_DIRECTORY, ProjectAsset, ProjectAssetKind,
    ProjectAssetSidecar, ProjectRelativeAssetPath, project_files_ending_with,
    project_files_the_history_records, read_every_asset_sidecar,
};
use crate::project_asset_file_bytes::{
    AssetFileBytesRecorded, PROJECT_UPLOADS_DIRECTORY, content_sha256_of,
    copy_the_file_into_the_uploads_hashing_it, is_a_sha256_digest,
    the_bytes_recorded_of_the_file_at, the_upload_file_of,
};
use crate::project_file::{
    PROJECT_FILE_NAME, ProjectImport, THIS_RUNTIME_VERSION, TatolabProjectFile,
};
use crate::project_history_repository::{
    PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectHistoryCommit, ProjectHistoryCommitWritten,
    ProjectHistoryRepository,
};
use crate::project_import_copy::{
    PROJECT_IMPORT_STAGING_DIRECTORY, files_an_import_carries, files_edited_since_the_copy,
    refuse_an_import_folder_name, sha256_of_each_file_under, stage_a_copy_of_the_project_source,
    the_import_folder_in, the_import_folder_named_after, unpack_a_project_archive_as_a_new_project,
    version_of_the_copied_files,
};
use crate::project_pyproject_import_sources::{
    PYPROJECT_FILE_NAME, pyproject_with_each_import_as_a_uv_path_source,
};
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_stream_document::{STREAM_DOCUMENT_ASSET_KEY, project_stream_documents};
use crate::project_transaction::{
    AppliedProjectTransaction, PROJECT_TRANSACTION_FORMAT_VERSION, ProjectAction,
    ProjectActionApplyOutcome, ProjectActionIdempotencyKey, ProjectAssetAction,
    ProjectAssetContent, ProjectAssetImportSource, ProjectAssetOperation, ProjectImportFiles,
    ProjectImportOperation, ProjectOperation, ProjectStreamDocumentOperation, ProjectTransaction,
    ProjectTransactionAuthor, ProjectTransactionEnvelopeHeader, ProjectTransactionLineage,
    ProjectTransactionReversal, StreamGraphOperationOnAStream,
};
use crate::stream_graph_document::{StreamGraphDocument, assets_a_node_config_references};

/// An import as `import_listing` answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectImportListed {
    /// The import as the project file records it.
    pub import: ProjectImport,
    /// Each file of its folder edited since its copy, or why its folder could not be read.
    pub edited_files: Result<Vec<String>, String>,
    /// When a check for newer was asked, the version its source holds now, or why the source
    /// could not be read.
    pub version_on_the_source: Option<Result<String, String>>,
}

/// The file uv locks a project's Python requirements in, beside its `pyproject.toml`.
const UV_LOCK_FILE_NAME: &str = "uv.lock";

/// What the host hands a project to lock the Python requirements of the project in a
/// directory with, refused in the resolver's own words when they cannot be resolved.
pub type ProjectPythonRequirementsLock = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// A project the runtime opened: the one writer of its files, its imports and its history.
pub struct ProjectRepository {
    project_directory: PathBuf,
    project_file: TatolabProjectFile,
    assets_by_uuid: BTreeMap<Uuid, ProjectAsset>,
    history: ProjectHistoryRepository,
    python_requirements_lock: Option<ProjectPythonRequirementsLock>,
    asset_files_waiting_for_their_import: BTreeMap<Uuid, AssetFileWaitingForItsImport>,
    commit_of_each_idempotency_key: HashMap<ProjectActionIdempotencyKey, String>,
}

/// The bytes an import of an asset moves to its path once the transaction applies.
struct AssetFileWaitingForItsImport {
    file: PathBuf,
    removed_when_the_import_is_refused: bool,
}

enum PerformedImportEffect {
    /// The import's folder was placed, replacing the folder named beside where it was set aside.
    Placed {
        import_folder: PathBuf,
        replaced: Option<(PathBuf, Option<PathBuf>)>,
    },
    /// The import's folder was set aside.
    TakenOut {
        import_folder: PathBuf,
        set_aside: Option<PathBuf>,
    },
}

enum PerformedAssetEffect {
    Operation(ProjectAssetOperation),
    ImportThatPlacedItsFile {
        asset: ProjectAsset,
        moved_from: PathBuf,
    },
    DeleteThatSetItsFileAside {
        asset: ProjectAsset,
        set_aside: PathBuf,
    },
}

/// One change a caller asked of a project's assets as the operation it stages, with the file
/// its import moves into place.
struct StagedAssetChange {
    operation: ProjectOperation,
    asset_file_waiting_for_its_import: Option<(Uuid, AssetFileWaitingForItsImport)>,
}

impl StagedAssetChange {
    fn of(asset_operation: ProjectAssetOperation) -> Self {
        Self {
            operation: ProjectOperation::Asset(asset_operation),
            asset_file_waiting_for_its_import: None,
        }
    }
}

/// Remove the file each delete of a committed transaction set aside.
fn remove_what_asset_effects_set_aside(performed_effects: Vec<PerformedAssetEffect>) {
    for performed in performed_effects {
        let PerformedAssetEffect::DeleteThatSetItsFileAside { set_aside, .. } = performed else {
            continue;
        };
        if let Err(not_removed) = std::fs::remove_file(&set_aside) {
            tracing::warn!(
                set_aside = %set_aside.display(),
                %not_removed,
                "could not remove the file of an asset a committed transaction deleted"
            );
        }
    }
}

/// A graph document's path and the bytes it held before a transaction wrote it, `None` when
/// it did not exist.
type GraphDocumentAsItWasBeforeAWrite = (PathBuf, Option<Vec<u8>>);

impl ProjectRepository {
    /// Create a project named `name` in `project_directory`, made when missing, with its first commit.
    pub fn create(
        project_directory: &Path,
        name: &str,
        author: &ProjectTransactionAuthor,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        refuse_a_relative_project_directory(project_directory)?;
        refuse_an_author_without_a_name(author)?;
        if project_directory.join(PROJECT_FILE_NAME).exists() {
            return Err(ProjectRepositoryRefusal::AlreadyAProject {
                project_directory: project_directory.to_path_buf(),
            });
        }
        std::fs::create_dir_all(project_directory)?;
        let runtime_directory = project_directory.join(PROJECT_RUNTIME_DIRECTORY);
        let runtime_directory_existed = runtime_directory.exists();
        let created = Self::write_a_new_project(project_directory, name, author);
        if created.is_err() {
            let _ = std::fs::remove_file(project_directory.join(PROJECT_FILE_NAME));
            if !runtime_directory_existed {
                let _ = std::fs::remove_dir_all(&runtime_directory);
            }
        }
        created
    }

    fn write_a_new_project(
        project_directory: &Path,
        name: &str,
        author: &ProjectTransactionAuthor,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        let project_file = TatolabProjectFile::of_a_new_project(name);
        write_file_whole(
            &project_directory.join(PROJECT_FILE_NAME),
            &project_file.canonical_bytes()?,
        )?;
        let repository = Self {
            project_directory: project_directory.to_path_buf(),
            history: ProjectHistoryRepository::create_for_project(project_directory)?,
            project_file,
            assets_by_uuid: BTreeMap::new(),
            python_requirements_lock: None,
            asset_files_waiting_for_their_import: BTreeMap::new(),
            commit_of_each_idempotency_key: HashMap::new(),
        };
        repository.history.commit_tracked_files(
            &repository.tracked_files(&repository.assets_by_uuid)?,
            author,
            &format!("create_project {name}"),
        )?;
        Ok(repository)
    }

    /// Open the project whose files are already written in `project_directory`, and commit them
    /// as its first commit.
    pub fn commit_a_written_project_as_its_first(
        project_directory: &Path,
        author: &ProjectTransactionAuthor,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        Self::commit_the_written_project_as_its_first(project_directory, author, "create_project")
    }

    /// Unpack the project the `.zip` at `source` holds into `project_directory`, which must not
    /// exist yet, under a new project UUID, and commit its files as its first commit.
    pub fn import_from_a_project_archive(
        project_directory: &Path,
        source: &str,
        author: &ProjectTransactionAuthor,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        refuse_a_relative_project_directory(project_directory)?;
        refuse_an_author_without_a_name(author)?;
        unpack_a_project_archive_as_a_new_project(project_directory, source)?;
        let committed = Self::commit_the_written_project_as_its_first(
            project_directory,
            author,
            "import_project",
        );
        if committed.is_err() {
            let _ = std::fs::remove_dir_all(project_directory);
        }
        committed
    }

    fn commit_the_written_project_as_its_first(
        project_directory: &Path,
        author: &ProjectTransactionAuthor,
        verb: &str,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        let repository = Self::open(project_directory)?;
        if repository.history.head_commit()?.is_some() {
            return Err(ProjectRepositoryRefusal::ProjectAlreadyCommitted {
                project_directory: project_directory.to_path_buf(),
            });
        }
        repository.history.commit_tracked_files(
            &repository.tracked_files(&repository.assets_by_uuid)?,
            author,
            &format!("{verb} {}", repository.project_file.name),
        )?;
        Ok(repository)
    }

    /// Open the project in `project_directory`, its assets read from their sidecars.
    pub fn open(project_directory: &Path) -> Result<Self, ProjectRepositoryRefusal> {
        refuse_a_relative_project_directory(project_directory)?;
        let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
        if project_file.created_with_runtime.as_deref() != Some(THIS_RUNTIME_VERSION) {
            tracing::info!(
                project = %project_directory.display(),
                created_with_runtime = project_file.created_with_runtime.as_deref().unwrap_or("unrecorded"),
                this_runtime = THIS_RUNTIME_VERSION,
                "the project was created with another runtime version; it opens all the same"
            );
        }
        let assets_by_uuid = read_every_asset_sidecar(project_directory)?;
        let history = ProjectHistoryRepository::open_for_project(project_directory)?;
        let commit_of_each_idempotency_key = the_commit_of_each_idempotency_key_in(&history)?;
        Ok(Self {
            project_directory: project_directory.to_path_buf(),
            history,
            project_file,
            assets_by_uuid,
            python_requirements_lock: None,
            asset_files_waiting_for_their_import: BTreeMap::new(),
            commit_of_each_idempotency_key,
        })
    }

    /// Lock the project's Python requirements with `python_requirements_lock` each time an
    /// import change rewrites its `pyproject.toml`, refusing the change when they cannot be
    /// resolved; a project never handed one leaves them to be resolved when it loads.
    pub fn lock_the_python_requirements_with(
        &mut self,
        python_requirements_lock: ProjectPythonRequirementsLock,
    ) {
        self.python_requirements_lock = Some(python_requirements_lock);
    }

    /// The project's directory.
    pub fn project_directory(&self) -> &Path {
        &self.project_directory
    }

    /// What the project file holds.
    pub fn project_file(&self) -> &TatolabProjectFile {
        &self.project_file
    }

    /// Every asset, ordered by UUID: each sidecar's, and each stream's, read from its graph
    /// document as it is on disk now.
    pub fn assets(&self) -> Result<Vec<ProjectAsset>, ProjectRepositoryRefusal> {
        Ok(self.every_asset()?.into_values().collect())
    }

    /// The asset `asset` names, or a refusal naming it.
    pub fn asset(&self, asset: Uuid) -> Result<ProjectAsset, ProjectRepositoryRefusal> {
        self.every_asset()?
            .remove(&asset)
            .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))
    }

    /// The asset `transaction`'s first operation changed: as it is now, or, taken out, as it was.
    pub fn the_asset_a_transaction_changed(
        &self,
        transaction: &ProjectTransaction,
    ) -> Result<ProjectAsset, ProjectRepositoryRefusal> {
        match transaction.operations.first() {
            Some(ProjectOperation::Asset(
                ProjectAssetOperation::ImportAsset { asset }
                | ProjectAssetOperation::DeleteAsset { asset },
            )) => Ok(asset.clone()),
            Some(ProjectOperation::Asset(
                ProjectAssetOperation::RelocateAsset { asset, .. }
                | ProjectAssetOperation::SetAssetSettings { asset, .. }
                | ProjectAssetOperation::ReplaceAssetContent { asset, .. },
            )) => self.asset(*asset),
            Some(ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument { stream, .. }
                | ProjectStreamDocumentOperation::SetStreamDisplayName { stream, .. },
            )) => self.asset(*stream),
            Some(ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::DeleteStreamDocument {
                    stream,
                    path,
                    document,
                },
            )) => {
                let bytes = StreamGraphDocument::from_value(document.clone())
                    .map_err(|why| ProjectRepositoryRefusal::GraphDocumentUnreadable {
                        path: path.to_string(),
                        why,
                    })?
                    .canonical_bytes()?;
                Ok(ProjectAsset {
                    asset: *stream,
                    kind: ProjectAssetKind::Stream,
                    content_sha256: content_sha256_of(&bytes),
                    content_byte_len: bytes.len() as u64,
                    import_settings: serde_json::Map::new(),
                    path: path.clone(),
                })
            }
            Some(ProjectOperation::StreamGraph(_) | ProjectOperation::Import(_)) | None => {
                Err(ProjectRepositoryRefusal::EmptyTransaction)
            }
        }
    }

    fn every_asset(&self) -> Result<BTreeMap<Uuid, ProjectAsset>, ProjectRepositoryRefusal> {
        let mut every_asset = self.assets_by_uuid.clone();
        for stream_document in project_stream_documents(&self.project_directory)? {
            if self
                .assets_by_uuid
                .values()
                .any(|asset| asset.path == stream_document.path)
            {
                continue;
            }
            let Ok(bytes) = &stream_document.bytes else {
                continue;
            };
            let stream_asset = ProjectAsset {
                asset: stream_document.stream_asset(self.project_file.project),
                kind: ProjectAssetKind::Stream,
                content_sha256: content_sha256_of(bytes),
                content_byte_len: bytes.len() as u64,
                import_settings: serde_json::Map::new(),
                path: stream_document.path,
            };
            if let Some(earlier) = every_asset.get(&stream_asset.asset) {
                return Err(ProjectRepositoryRefusal::AssetNamedTwice {
                    asset: stream_asset.asset,
                    first: earlier.path.to_string(),
                    second: stream_asset.path.to_string(),
                });
            }
            every_asset.insert(stream_asset.asset, stream_asset);
        }
        Ok(every_asset)
    }

    /// The project's history.
    pub fn history(&self) -> &ProjectHistoryRepository {
        &self.history
    }

    /// The newest `max_transactions` transactions history records, newest first, each read
    /// back from its commit's trailer; a commit without one, such as the first, is skipped.
    pub fn applied_transactions_newest_first(
        &self,
        max_transactions: usize,
    ) -> Result<Vec<AppliedProjectTransaction>, ProjectRepositoryRefusal> {
        self.each_applied_transaction_newest_first()?
            .take(max_transactions)
            .collect()
    }

    /// The transaction history holds under the key a caller sent, which an action sent again
    /// under that key answers instead of applying again; `None` when the caller sent no key.
    pub fn the_transaction_applied_under_a_key_the_caller_sent(
        &self,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
    ) -> Result<Option<AppliedProjectTransaction>, ProjectRepositoryRefusal> {
        let Some(commit) =
            idempotency_key.and_then(|key| self.commit_of_each_idempotency_key.get(key))
        else {
            return Ok(None);
        };
        let Some(history_commit) = self
            .history
            .commits_newest_first()?
            .into_iter()
            .find(|history_commit| history_commit.commit == *commit)
        else {
            return Ok(None);
        };
        let Some(transaction_json) = the_transaction_trailer_of(&history_commit.message) else {
            return Ok(None);
        };
        let transaction_json = transaction_json.to_owned();
        applied_transaction_read_from_its_trailer(&transaction_json, history_commit).map(Some)
    }

    fn each_applied_transaction_newest_first(
        &self,
    ) -> Result<
        impl Iterator<Item = Result<AppliedProjectTransaction, ProjectRepositoryRefusal>>,
        ProjectRepositoryRefusal,
    > {
        Ok(self
            .history
            .commits_newest_first()?
            .into_iter()
            .filter_map(|history_commit| {
                let transaction_json =
                    the_transaction_trailer_of(&history_commit.message)?.to_owned();
                Some(applied_transaction_read_from_its_trailer(
                    &transaction_json,
                    history_commit,
                ))
            }))
    }

    /// Apply `changes` by `author` as one transaction, whole or not at all, and commit it; sent
    /// again under the same `idempotency_key`, it answers the first transaction.
    pub fn apply_asset_changes(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        changes: Vec<ProjectAssetAction>,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let actions_as_sent = changes.iter().cloned().map(ProjectAction::Asset).collect();
        let applied = self.operations_for_changes(changes).and_then(|operations| {
            self.apply_and_commit_project_operations(
                author,
                idempotency_key,
                actions_as_sent,
                operations,
                ProjectTransactionLineage::NewEdit,
            )
        });
        for waiting in std::mem::take(&mut self.asset_files_waiting_for_their_import).into_values()
        {
            if waiting.removed_when_the_import_is_refused && waiting.file.exists() {
                let _ = std::fs::remove_file(&waiting.file);
            }
        }
        applied.map(ProjectActionApplyOutcome::AppliedNow)
    }

    fn operations_for_changes(
        &mut self,
        changes: Vec<ProjectAssetAction>,
    ) -> Result<Vec<ProjectOperation>, ProjectRepositoryRefusal> {
        let mut staged_assets = self.every_asset()?;
        let mut operations = Vec::with_capacity(changes.len());
        for change in changes {
            let Some(staged) = self.operation_for_change(&staged_assets, change)? else {
                continue;
            };
            match &staged.operation {
                ProjectOperation::Asset(asset_operation) => {
                    apply_operation_to_assets(&mut staged_assets, asset_operation);
                }
                ProjectOperation::StreamDocument(
                    ProjectStreamDocumentOperation::DeleteStreamDocument { stream, .. },
                ) => {
                    staged_assets.remove(stream);
                }
                _ => {}
            }
            if let Some((asset, waiting)) = staged.asset_file_waiting_for_its_import {
                self.asset_files_waiting_for_their_import
                    .insert(asset, waiting);
            }
            operations.push(staged.operation);
        }
        Ok(operations)
    }

    /// The graph document of the stream asset `stream`, as it is on disk now.
    pub fn stream_graph_document(
        &self,
        stream: Uuid,
    ) -> Result<StreamGraphDocument, ProjectRepositoryRefusal> {
        self.read_stream_graph_document(&self.every_asset()?, stream)
            .map(|(_, document)| document)
    }

    fn read_stream_graph_document(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        stream: Uuid,
    ) -> Result<(ProjectRelativeAssetPath, StreamGraphDocument), ProjectRepositoryRefusal> {
        let stream_asset = assets
            .get(&stream)
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)
            .ok_or(ProjectRepositoryRefusal::NoSuchStream(stream))?;
        let bytes = std::fs::read(stream_asset.path.on_disk_under(&self.project_directory))?;
        let document = StreamGraphDocument::from_bytes(&bytes).map_err(|why| {
            ProjectRepositoryRefusal::GraphDocumentUnreadable {
                path: stream_asset.path.to_string(),
                why,
            }
        })?;
        Ok((stream_asset.path.clone(), document))
    }

    /// Apply `operations` by `author` as one transaction, whole or not at all, write the
    /// files they change and commit it, recording `actions_as_sent` beside them; sent again under the same `idempotency_key`, it applies
    /// nothing and answers the first transaction.
    pub fn apply_project_operations(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        actions_as_sent: Vec<ProjectAction>,
        operations: Vec<ProjectOperation>,
        lineage: ProjectTransactionLineage,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        self.apply_and_commit_project_operations(
            author,
            idempotency_key,
            actions_as_sent,
            operations,
            lineage,
        )
        .map(ProjectActionApplyOutcome::AppliedNow)
    }

    fn apply_and_commit_project_operations(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        actions_as_sent: Vec<ProjectAction>,
        operations: Vec<ProjectOperation>,
        lineage: ProjectTransactionLineage,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if operations.is_empty() {
            return Err(ProjectRepositoryRefusal::EmptyTransaction);
        }
        let mut staged_assets = self.every_asset()?;
        let mut staged_documents: BTreeMap<Uuid, StreamGraphDocument> = BTreeMap::new();
        let mut staged_project_file = self.project_file.clone();
        let mut stream_documents_taken_out: BTreeSet<ProjectRelativeAssetPath> = BTreeSet::new();
        for operation in &operations {
            match operation {
                ProjectOperation::StreamDocument(stream_document_operation) => {
                    self.apply_operation_to_stream_documents(
                        &mut staged_assets,
                        &mut staged_documents,
                        &mut stream_documents_taken_out,
                        stream_document_operation,
                    )?;
                }
                ProjectOperation::Import(import_operation) => {
                    apply_operation_to_imports(
                        &self.project_directory,
                        &mut staged_project_file,
                        import_operation,
                    )?;
                }
                ProjectOperation::Asset(asset_operation) => {
                    if let ProjectAssetOperation::DeleteAsset { asset } = asset_operation {
                        self.refuse_deleting_an_asset_a_graph_references(
                            asset,
                            &staged_assets,
                            &staged_documents,
                        )?;
                    }
                    apply_operation_to_assets(&mut staged_assets, asset_operation);
                }
                ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                    stream,
                    graph_operation,
                }) => {
                    let staged_document = match staged_documents.entry(*stream) {
                        Entry::Occupied(staged) => staged.into_mut(),
                        Entry::Vacant(unstaged) => unstaged
                            .insert(self.read_stream_graph_document(&staged_assets, *stream)?.1),
                    };
                    for (node, config) in graph_operation.configs_it_sets() {
                        refuse_a_config_referencing_an_asset_the_project_does_not_hold(
                            &staged_assets,
                            node,
                            config,
                        )?;
                    }
                    staged_document.apply(graph_operation).map_err(|why| {
                        ProjectRepositoryRefusal::GraphOperationRefused {
                            path: staged_assets
                                .get(stream)
                                .map_or_else(|| stream.to_string(), |asset| asset.path.to_string()),
                            why,
                        }
                    })?;
                }
            }
        }
        for operation in &operations {
            if let ProjectOperation::Import(ProjectImportOperation::RemoveImport { import }) =
                operation
            {
                self.refuse_removing_an_import_a_graph_references(
                    import.project,
                    &staged_assets,
                    &staged_documents,
                )?;
            }
        }
        let (undoes, redoes) = match lineage {
            ProjectTransactionLineage::NewEdit => (None, None),
            ProjectTransactionLineage::UndoOf(undone) => (Some(undone), None),
            ProjectTransactionLineage::RedoOf(redone) => (None, Some(redone)),
        };
        let parent_commit = self.commit_the_files_when_history_holds_none()?;
        let transaction = ProjectTransaction {
            format_version: PROJECT_TRANSACTION_FORMAT_VERSION,
            transaction: Uuid::new_v4(),
            author: author.clone(),
            inverse_operations: operations
                .iter()
                .rev()
                .map(|operation| operation.inverse_at(&parent_commit))
                .collect(),
            parent_commit: Some(parent_commit),
            operations,
            undoes,
            redoes,
            idempotency_key: idempotency_key.cloned(),
            actions: actions_as_sent,
        };

        let mut performed_effects = Vec::with_capacity(transaction.operations.len());
        for operation in &transaction.operations {
            let ProjectOperation::Asset(asset_operation) = operation else {
                continue;
            };
            match self.perform_effect(asset_operation) {
                Ok(performed) => performed_effects.push(performed),
                Err(refusal) => {
                    self.undo_performed_effects(performed_effects);
                    return Err(refusal);
                }
            }
        }
        let mut performed_import_effects = Vec::new();
        for operation in &transaction.operations {
            let ProjectOperation::Import(import_operation) = operation else {
                continue;
            };
            match self.perform_import_effect(import_operation) {
                Ok(performed) => performed_import_effects.push(performed),
                Err(refusal) => {
                    self.undo_performed_import_effects(performed_import_effects);
                    self.undo_performed_effects(performed_effects);
                    return Err(refusal);
                }
            }
        }
        let written_documents = match self
            .write_staged_documents(&staged_assets, &staged_documents)
            .and_then(|mut written_documents| {
                match self.take_out_stream_documents(&stream_documents_taken_out) {
                    Ok(taken_out) => {
                        written_documents.extend(taken_out);
                        Ok(written_documents)
                    }
                    Err(refusal) => {
                        self.restore_written_documents(written_documents);
                        Err(refusal)
                    }
                }
            }) {
            Ok(written_documents) => written_documents,
            Err(refusal) => {
                self.undo_performed_import_effects(performed_import_effects);
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
        };
        let project_file_as_it_was = self.project_file.clone();
        if staged_project_file != project_file_as_it_was {
            let written = staged_project_file.canonical_bytes().and_then(|bytes| {
                write_file_whole(&self.project_directory.join(PROJECT_FILE_NAME), &bytes)
            });
            if let Err(refusal) = written {
                self.restore_written_documents(written_documents);
                self.undo_performed_import_effects(performed_import_effects);
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
            self.project_file = staged_project_file;
        }
        let pyproject_as_it_was = match self.write_each_import_into_the_pyproject_if_they_changed(
            &project_file_as_it_was.dependencies,
        ) {
            Ok(pyproject_as_it_was) => pyproject_as_it_was,
            Err(refusal) => {
                self.restore_the_project_file(&project_file_as_it_was);
                self.restore_written_documents(written_documents);
                self.undo_performed_import_effects(performed_import_effects);
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
        };
        let uv_lock_as_it_was = match pyproject_as_it_was
            .as_deref()
            .map(|pyproject_as_it_was| {
                self.lock_the_python_requirements_of_the_rewritten_pyproject()
                    .inspect_err(|_| self.restore_the_pyproject(pyproject_as_it_was))
            })
            .transpose()
        {
            Ok(uv_lock_as_it_was) => uv_lock_as_it_was.flatten(),
            Err(refusal) => {
                self.restore_the_project_file(&project_file_as_it_was);
                self.restore_written_documents(written_documents);
                self.undo_performed_import_effects(performed_import_effects);
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
        };
        let committed = self
            .tracked_files(&staged_assets)
            .and_then(|tracked_files| {
                self.history.commit_tracked_files(
                    &tracked_files,
                    author,
                    &transaction_commit_message(&transaction)?,
                )
            });
        let ProjectHistoryCommitWritten {
            commit,
            committed_at_unix_seconds: applied_at_unix_seconds,
        } = match committed {
            Ok(written) => written,
            Err(refusal) => {
                if let Some(uv_lock_as_it_was) = uv_lock_as_it_was {
                    self.restore_the_uv_lock(uv_lock_as_it_was);
                }
                if let Some(pyproject_as_it_was) = pyproject_as_it_was {
                    self.restore_the_pyproject(&pyproject_as_it_was);
                }
                self.restore_the_project_file(&project_file_as_it_was);
                self.restore_written_documents(written_documents);
                self.undo_performed_import_effects(performed_import_effects);
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
        };
        if let Some(idempotency_key) = &transaction.idempotency_key {
            self.commit_of_each_idempotency_key
                .insert(idempotency_key.clone(), commit.clone());
        }
        self.remove_what_import_effects_set_aside(performed_import_effects);
        remove_what_asset_effects_set_aside(performed_effects);
        staged_assets.retain(|_, asset| asset.kind != ProjectAssetKind::Stream);
        self.assets_by_uuid = staged_assets;
        tracing::info!(
            project = %self.project_directory.display(),
            transaction = %transaction.transaction,
            commit = %commit,
            "applied a project transaction"
        );
        Ok(AppliedProjectTransaction {
            transaction,
            commit,
            applied_at_unix_seconds,
        })
    }

    /// Import the project `source` holds, a project folder or a `.zip` of one on this machine by
    /// absolute path, as one transaction by `author`: it is copied whole into
    /// `imports/<folder>/`, `folder` defaulting to its name, and recorded in the project file
    /// with the version the copy carried. Refused, nothing copied, for a source holding no
    /// project, this project itself, a project already imported, or a folder already held.
    pub fn add_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        source: &str,
        folder: Option<&str>,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let staged = stage_a_copy_of_the_project_source(&self.project_directory, source)?;
        let added = (|| {
            let folder = folder
                .map(str::to_owned)
                .unwrap_or_else(|| the_import_folder_named_after(&staged.name));
            let import = ProjectImport {
                project: staged.project,
                folder,
                source: source.to_owned(),
                version: staged.version()?,
                copied_files: staged.copied_files.clone(),
            };
            self.apply_and_commit_project_operations(
                author,
                idempotency_key,
                Vec::new(),
                vec![ProjectOperation::Import(
                    ProjectImportOperation::PlaceImport {
                        import,
                        replacing: None,
                        files: ProjectImportFiles::StagedCopy {
                            staged_at: staged.staged_at.clone(),
                        },
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
        })();
        if added.is_err() {
            let _ = std::fs::remove_dir_all(self.project_directory.join(&staged.staged_at));
        }
        added.map(ProjectActionApplyOutcome::AppliedNow)
    }

    /// Copy the import `project` again, whole, from `source`, or from the source it was copied
    /// from, as one transaction by `author`. When its folder was edited since its copy, refused
    /// naming each edited file unless `overwrite_edited_files`.
    pub fn update_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        project: Uuid,
        source: Option<&str>,
        overwrite_edited_files: bool,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let held = self.import(project)?;
        let edited_files = self.files_edited_in_the_import(&held)?;
        if !edited_files.is_empty() && !overwrite_edited_files {
            return Err(ProjectRepositoryRefusal::ImportEditedSinceItsCopy {
                folder: held.folder,
                edited_files,
            });
        }
        let source = source.unwrap_or(&held.source).to_owned();
        let staged = stage_a_copy_of_the_project_source(&self.project_directory, &source)?;
        let updated = (|| {
            if staged.project != project {
                return Err(ProjectRepositoryRefusal::ImportSourceHoldsAnotherProject {
                    import_source: source.clone(),
                    expected: project,
                    held: staged.project,
                });
            }
            let import = ProjectImport {
                project,
                folder: held.folder.clone(),
                source: source.clone(),
                version: staged.version()?,
                copied_files: staged.copied_files.clone(),
            };
            self.apply_and_commit_project_operations(
                author,
                idempotency_key,
                Vec::new(),
                vec![ProjectOperation::Import(
                    ProjectImportOperation::PlaceImport {
                        import,
                        replacing: Some(held.clone()),
                        files: ProjectImportFiles::StagedCopy {
                            staged_at: staged.staged_at.clone(),
                        },
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
        })();
        if updated.is_err() {
            let _ = std::fs::remove_dir_all(self.project_directory.join(&staged.staged_at));
        }
        updated.map(ProjectActionApplyOutcome::AppliedNow)
    }

    /// The import of `project` as the project file records it.
    pub fn import(&self, project: Uuid) -> Result<ProjectImport, ProjectRepositoryRefusal> {
        self.project_file
            .import_of(project)
            .cloned()
            .ok_or(ProjectRepositoryRefusal::NoSuchImport(project))
    }

    /// Each file of the import's folder edited since its copy: changed, added or taken out.
    pub fn files_edited_in_the_import(
        &self,
        import: &ProjectImport,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        let import_folder = the_import_folder_in(&self.project_directory, &import.folder);
        let held_now = if import_folder.is_dir() {
            sha256_of_each_file_under(&import_folder)?
        } else {
            BTreeMap::new()
        };
        Ok(files_edited_since_the_copy(&import.copied_files, &held_now))
    }

    /// Every file the import's folder carries now, by its path inside the folder, sorted.
    pub fn files_the_import_carries(
        &self,
        import: &ProjectImport,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        files_an_import_carries(&the_import_folder_in(
            &self.project_directory,
            &import.folder,
        ))
    }

    /// Every import the project file records, each with the files edited since its copy and,
    /// when `check_for_newer`, the version its source holds now or why it could not be read.
    pub fn import_listing(&self, check_for_newer: bool) -> Vec<ProjectImportListed> {
        self.project_file
            .dependencies
            .iter()
            .map(|import| ProjectImportListed {
                import: import.clone(),
                edited_files: self
                    .files_edited_in_the_import(import)
                    .map_err(|refusal| refusal.to_string()),
                version_on_the_source: check_for_newer.then(|| {
                    self.version_the_source_holds_now(&import.source)
                        .map_err(|refusal| refusal.to_string())
                }),
            })
            .collect()
    }

    fn version_the_source_holds_now(
        &self,
        source: &str,
    ) -> Result<String, ProjectRepositoryRefusal> {
        let source_path = Path::new(source);
        if source_path.is_absolute() && source_path.is_dir() {
            return version_of_the_copied_files(&sha256_of_each_file_under(source_path)?);
        }
        let staged = stage_a_copy_of_the_project_source(&self.project_directory, source)?;
        let version = staged.version();
        let _ = std::fs::remove_dir_all(self.project_directory.join(&staged.staged_at));
        version
    }

    /// Stop importing `project`, as one transaction by `author`: its folder leaves `imports/`
    /// and its entry the project file. Refused while a node of this project's streams still
    /// references it.
    pub fn remove_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        project: Uuid,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let import = self.import(project)?;
        self.apply_and_commit_project_operations(
            author,
            idempotency_key,
            Vec::new(),
            vec![ProjectOperation::Import(
                ProjectImportOperation::RemoveImport { import },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .map(ProjectActionApplyOutcome::AppliedNow)
    }

    /// Refuse removing the import `project` while a node of a stream, as the transaction has
    /// staged its document or as it is on disk, references it, at any depth of its config.
    fn refuse_removing_an_import_a_graph_references(
        &self,
        project: Uuid,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let project_text = project.to_string();
        let mut referencing_nodes = Vec::new();
        for stream_asset in staged_assets
            .values()
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)
        {
            let document: Value = match staged_documents.get(&stream_asset.asset) {
                Some(staged_document) => {
                    serde_json::from_slice(&staged_document.canonical_bytes()?)?
                }
                None => serde_json::from_slice(&std::fs::read(
                    stream_asset.path.on_disk_under(&self.project_directory),
                )?)
                .map_err(|why| {
                    ProjectRepositoryRefusal::GraphDocumentUnreadable {
                        path: stream_asset.path.to_string(),
                        why: why.to_string(),
                    }
                })?,
            };
            for node in document
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if value_references_the_import(node, &project_text) {
                    referencing_nodes.push(format!(
                        "{} node `{}` ({})",
                        stream_asset.path,
                        node.get("name").and_then(Value::as_str).unwrap_or_default(),
                        node.get("id").and_then(Value::as_str).unwrap_or_default(),
                    ));
                }
            }
        }
        if referencing_nodes.is_empty() {
            Ok(())
        } else {
            Err(ProjectRepositoryRefusal::ImportStillReferenced {
                project,
                referencing_nodes,
            })
        }
    }

    /// Write each import the project file holds into `pyproject.toml` as a uv path source when
    /// they differ from `imports_as_they_were` and the project has a `pyproject.toml`, answering the file's text as it was so a refused
    /// commit can put it back; `None` when nothing was written.
    fn write_each_import_into_the_pyproject_if_they_changed(
        &self,
        imports_as_they_were: &[ProjectImport],
    ) -> Result<Option<String>, ProjectRepositoryRefusal> {
        if self.project_file.dependencies == imports_as_they_were {
            return Ok(None);
        }
        let pyproject_path = self.project_directory.join(PYPROJECT_FILE_NAME);
        let pyproject_as_it_was = match std::fs::read_to_string(&pyproject_path) {
            Ok(text) => text,
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(not_read) => return Err(not_read.into()),
        };
        let pyproject = pyproject_with_each_import_as_a_uv_path_source(
            &pyproject_as_it_was,
            &self.project_directory,
            &self.project_file.dependencies,
        )?;
        if pyproject == pyproject_as_it_was {
            return Ok(None);
        }
        write_file_whole(&pyproject_path, pyproject.as_bytes())?;
        Ok(Some(pyproject_as_it_was))
    }

    /// Lock the Python requirements the rewritten `pyproject.toml` declares with what the host
    /// handed, answering the `uv.lock` as it was, `None` inside when there was none, so a
    /// refused commit can put it back; refused in the resolver's words, `uv.lock` put back.
    /// `None` when the project was handed nothing to lock with.
    fn lock_the_python_requirements_of_the_rewritten_pyproject(
        &self,
    ) -> Result<Option<Option<Vec<u8>>>, ProjectRepositoryRefusal> {
        let Some(python_requirements_lock) = &self.python_requirements_lock else {
            return Ok(None);
        };
        let uv_lock_path = self.project_directory.join(UV_LOCK_FILE_NAME);
        let uv_lock_as_it_was = match std::fs::read(&uv_lock_path) {
            Ok(bytes) => Some(bytes),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => None,
            Err(not_read) => return Err(not_read.into()),
        };
        if let Err(resolver_refusal) = python_requirements_lock(&self.project_directory) {
            self.restore_the_uv_lock(uv_lock_as_it_was);
            return Err(ProjectRepositoryRefusal::ImportRequirementsUnresolvable {
                resolver_refusal,
            });
        }
        Ok(Some(uv_lock_as_it_was))
    }

    fn restore_the_uv_lock(&self, uv_lock_as_it_was: Option<Vec<u8>>) {
        let uv_lock_path = self.project_directory.join(UV_LOCK_FILE_NAME);
        let restored = match uv_lock_as_it_was {
            Some(bytes) => write_file_whole(&uv_lock_path, &bytes),
            None => match std::fs::remove_file(&uv_lock_path) {
                Err(not_removed) if not_removed.kind() != std::io::ErrorKind::NotFound => {
                    Err(not_removed.into())
                }
                _ => Ok(()),
            },
        };
        if let Err(not_restored) = restored {
            tracing::error!(
                project = %self.project_directory.display(),
                %not_restored,
                "could not restore uv.lock after a refused change"
            );
        }
    }

    fn restore_the_pyproject(&self, pyproject_as_it_was: &str) {
        if let Err(not_restored) = write_file_whole(
            &self.project_directory.join(PYPROJECT_FILE_NAME),
            pyproject_as_it_was.as_bytes(),
        ) {
            tracing::error!(
                project = %self.project_directory.display(),
                %not_restored,
                "could not restore pyproject.toml after a refused commit"
            );
        }
    }

    fn restore_the_project_file(&mut self, project_file_as_it_was: &TatolabProjectFile) {
        if self.project_file == *project_file_as_it_was {
            return;
        }
        if let Err(not_restored) = project_file_as_it_was.canonical_bytes().and_then(|bytes| {
            write_file_whole(&self.project_directory.join(PROJECT_FILE_NAME), &bytes)
        }) {
            tracing::error!(
                project = %self.project_directory.display(),
                %not_restored,
                "could not restore the project file after a refused commit"
            );
        }
        self.project_file = project_file_as_it_was.clone();
    }

    /// Put the import an operation places or takes out in place on disk, answering how to undo
    /// it: an import's folder it replaced or took out is set aside in the staging folder.
    fn perform_import_effect(
        &self,
        operation: &ProjectImportOperation,
    ) -> Result<PerformedImportEffect, ProjectRepositoryRefusal> {
        match operation {
            ProjectImportOperation::PlaceImport {
                import,
                replacing,
                files,
            } => {
                let import_folder = the_import_folder_in(&self.project_directory, &import.folder);
                let staged_folder = match files {
                    ProjectImportFiles::StagedCopy { staged_at } => {
                        self.project_directory.join(staged_at)
                    }
                    ProjectImportFiles::RecordedAtCommit { commit } => {
                        self.stage_the_import_folder_history_recorded_at(&import.folder, commit)?
                    }
                };
                let set_aside = match replacing {
                    Some(replaced) => self.set_aside_the_folder_of(&replaced.folder)?,
                    None => None,
                };
                if let Some(imports_folder) = import_folder.parent() {
                    std::fs::create_dir_all(imports_folder)?;
                }
                if let Err(not_placed) = std::fs::rename(&staged_folder, &import_folder) {
                    if let (Some(replaced), Some(set_aside)) = (replacing, &set_aside) {
                        let _ = std::fs::rename(
                            set_aside,
                            the_import_folder_in(&self.project_directory, &replaced.folder),
                        );
                    }
                    return Err(not_placed.into());
                }
                Ok(PerformedImportEffect::Placed {
                    import_folder,
                    replaced: replacing.as_ref().map(|replaced| {
                        (
                            the_import_folder_in(&self.project_directory, &replaced.folder),
                            set_aside,
                        )
                    }),
                })
            }
            ProjectImportOperation::RemoveImport { import } => {
                let import_folder = the_import_folder_in(&self.project_directory, &import.folder);
                Ok(PerformedImportEffect::TakenOut {
                    set_aside: self.set_aside_the_folder_of(&import.folder)?,
                    import_folder,
                })
            }
        }
    }

    /// Move the import's folder `folder` into the staging folder, answering where; `None` when
    /// it is not on disk.
    fn set_aside_the_folder_of(
        &self,
        folder: &str,
    ) -> Result<Option<PathBuf>, ProjectRepositoryRefusal> {
        let import_folder = the_import_folder_in(&self.project_directory, folder);
        if !import_folder.exists() {
            return Ok(None);
        }
        let set_aside = self
            .project_directory
            .join(PROJECT_IMPORT_STAGING_DIRECTORY)
            .join(format!("set-aside-{}", Uuid::new_v4()));
        if let Some(staging) = set_aside.parent() {
            std::fs::create_dir_all(staging)?;
        }
        std::fs::rename(&import_folder, &set_aside)?;
        Ok(Some(set_aside))
    }

    /// Write the files history recorded under `imports/<folder>/` at `commit` into a fresh
    /// staging folder, answering it. An asset's file history records by its sidecar alone is
    /// not among them, so it reads as missing until it is put back.
    fn stage_the_import_folder_history_recorded_at(
        &self,
        folder: &str,
        commit: &str,
    ) -> Result<PathBuf, ProjectRepositoryRefusal> {
        let staged_folder = self
            .project_directory
            .join(PROJECT_IMPORT_STAGING_DIRECTORY)
            .join(Uuid::new_v4().to_string());
        let prefix = format!("{PROJECT_IMPORTS_DIRECTORY}/{folder}/");
        let written = (|| {
            std::fs::create_dir_all(&staged_folder)?;
            for (path, bytes) in self.history.files_at_commit(commit)? {
                let Some(inside_the_import) = path.strip_prefix(&prefix) else {
                    continue;
                };
                let destination = staged_folder.join(inside_the_import);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(destination, bytes)?;
            }
            Ok(())
        })();
        if let Err(refusal) = written {
            let _ = std::fs::remove_dir_all(&staged_folder);
            return Err(refusal);
        }
        Ok(staged_folder)
    }

    fn undo_performed_import_effects(&self, performed_effects: Vec<PerformedImportEffect>) {
        for performed in performed_effects.into_iter().rev() {
            let restored = match performed {
                PerformedImportEffect::Placed {
                    import_folder,
                    replaced,
                } => std::fs::remove_dir_all(&import_folder).and_then(|()| match replaced {
                    Some((replaced_folder, Some(set_aside))) => {
                        std::fs::rename(set_aside, replaced_folder)
                    }
                    _ => Ok(()),
                }),
                PerformedImportEffect::TakenOut {
                    import_folder,
                    set_aside: Some(set_aside),
                } => std::fs::rename(set_aside, import_folder),
                PerformedImportEffect::TakenOut {
                    set_aside: None, ..
                } => Ok(()),
            };
            if let Err(not_restored) = restored {
                tracing::error!(
                    project = %self.project_directory.display(),
                    %not_restored,
                    "could not put an import's folder back after a refused transaction"
                );
            }
        }
    }

    fn remove_what_import_effects_set_aside(&self, performed_effects: Vec<PerformedImportEffect>) {
        for performed in performed_effects {
            let set_aside = match performed {
                PerformedImportEffect::Placed {
                    replaced: Some((_, Some(set_aside))),
                    ..
                }
                | PerformedImportEffect::TakenOut {
                    set_aside: Some(set_aside),
                    ..
                } => set_aside,
                _ => continue,
            };
            if let Err(not_removed) = std::fs::remove_dir_all(&set_aside) {
                tracing::warn!(
                    set_aside = %set_aside.display(),
                    %not_removed,
                    "could not remove an import's folder a committed transaction replaced"
                );
            }
        }
    }

    /// The project enclosing this one as its import, and the folder it holds it in, when this
    /// project is open at `<enclosing>/imports/<folder>/`.
    pub fn the_project_enclosing_this_one(&self) -> Option<(PathBuf, String)> {
        the_project_enclosing_an_import(&self.project_directory, self.project_file.project)
    }

    /// Commit this project's files as they are now, recording the edit `transaction` made in
    /// its import `folder` as one commit by its author.
    pub fn commit_an_edit_of_the_import(
        &self,
        folder: &str,
        transaction: &ProjectTransaction,
    ) -> Result<String, ProjectRepositoryRefusal> {
        let summary = transaction
            .operations
            .iter()
            .map(ProjectOperation::summary)
            .collect::<Vec<_>>()
            .join("; ");
        self.history
            .commit_tracked_files(
                &self.tracked_files(&self.assets_by_uuid)?,
                &transaction.author,
                &format!("edit {PROJECT_IMPORTS_DIRECTORY}/{folder}: {summary}"),
            )
            .map(|written| written.commit)
    }

    /// Commit the project's files as they are when its history holds no commit yet, as for an
    /// import opened as its own project, so every transaction has a parent to undo to.
    fn commit_the_files_when_history_holds_none(&self) -> Result<String, ProjectRepositoryRefusal> {
        if let Some(head) = self.history.head_commit()? {
            return Ok(head);
        }
        self.history
            .commit_tracked_files(
                &self.tracked_files(&self.assets_by_uuid)?,
                &ProjectTransactionAuthor::the_runtime(),
                &format!("open_project {}", self.project_file.name),
            )
            .map(|written| written.commit)
    }

    /// The undo `author`'s latest transaction not yet undone asks for: its inverse.
    pub fn undo_by(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<ProjectTransactionReversal, ProjectRepositoryRefusal> {
        let (undoable, _) = self.what_the_author_can_undo_and_redo(author)?;
        let undone = undoable
            .last()
            .ok_or_else(|| ProjectRepositoryRefusal::NothingToUndo {
                author: author.name.clone(),
            })?;
        Ok(ProjectTransactionReversal {
            operations: undone.transaction.inverse_operations.clone(),
            lineage: ProjectTransactionLineage::UndoOf(undone.transaction.transaction),
        })
    }

    /// The redo `author`'s latest undo asks for: the operations it undid, each import a staged
    /// copy placed read back from the commit that placed it.
    pub fn redo_by(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<ProjectTransactionReversal, ProjectRepositoryRefusal> {
        let (_, redoable) = self.what_the_author_can_undo_and_redo(author)?;
        let redone = redoable
            .last()
            .ok_or_else(|| ProjectRepositoryRefusal::NothingToRedo {
                author: author.name.clone(),
            })?;
        Ok(ProjectTransactionReversal {
            operations: redone
                .transaction
                .operations
                .iter()
                .map(|operation| match operation {
                    ProjectOperation::Import(ProjectImportOperation::PlaceImport {
                        import,
                        replacing,
                        files: ProjectImportFiles::StagedCopy { .. },
                    }) => ProjectOperation::Import(ProjectImportOperation::PlaceImport {
                        import: import.clone(),
                        replacing: replacing.clone(),
                        files: ProjectImportFiles::RecordedAtCommit {
                            commit: redone.commit.clone(),
                        },
                    }),
                    operation => operation.clone(),
                })
                .collect(),
            lineage: ProjectTransactionLineage::RedoOf(redone.transaction.transaction),
        })
    }

    /// `author`'s transactions an undo reverses, newest last, and the ones a redo reapplies,
    /// newest last, replayed from history: an edit clears what could be redone.
    fn what_the_author_can_undo_and_redo(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<
        (
            Vec<AppliedProjectTransaction>,
            Vec<AppliedProjectTransaction>,
        ),
        ProjectRepositoryRefusal,
    > {
        let mut undoable: Vec<AppliedProjectTransaction> = Vec::new();
        let mut redoable: Vec<AppliedProjectTransaction> = Vec::new();
        let mut transactions = self.applied_transactions_newest_first(usize::MAX)?;
        transactions.reverse();
        for applied in transactions {
            if applied.transaction.author != *author {
                continue;
            }
            match applied.transaction.lineage() {
                ProjectTransactionLineage::NewEdit => {
                    redoable.clear();
                    undoable.push(applied);
                }
                ProjectTransactionLineage::UndoOf(undone) => {
                    if let Some(position) = undoable
                        .iter()
                        .rposition(|held| held.transaction.transaction == undone)
                    {
                        redoable.push(undoable.remove(position));
                    }
                }
                ProjectTransactionLineage::RedoOf(redone) => {
                    if let Some(position) = redoable
                        .iter()
                        .rposition(|held| held.transaction.transaction == redone)
                    {
                        redoable.remove(position);
                        undoable.push(applied);
                    }
                }
            }
        }
        Ok((undoable, redoable))
    }

    /// Write each staged graph document at the path its stream asset holds once the
    /// transaction applies, answering what each held before so a refusal can restore it.
    fn write_staged_documents(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<Vec<GraphDocumentAsItWasBeforeAWrite>, ProjectRepositoryRefusal> {
        let mut written_documents = Vec::with_capacity(staged_documents.len());
        for (stream, staged_document) in staged_documents {
            let Some(stream_asset) = staged_assets.get(stream) else {
                continue;
            };
            let on_disk = stream_asset.path.on_disk_under(&self.project_directory);
            let bytes_before = std::fs::read(&on_disk).ok();
            let written = staged_document
                .canonical_bytes()
                .map_err(ProjectRepositoryRefusal::from)
                .and_then(|bytes| write_file_whole(&on_disk, &bytes));
            if let Err(refusal) = written {
                self.restore_written_documents(written_documents);
                return Err(refusal);
            }
            written_documents.push((on_disk, bytes_before));
        }
        Ok(written_documents)
    }

    /// Remove each graph document at `paths`, answering what each held so a refusal can
    /// restore it.
    fn take_out_stream_documents(
        &self,
        paths: &BTreeSet<ProjectRelativeAssetPath>,
    ) -> Result<Vec<GraphDocumentAsItWasBeforeAWrite>, ProjectRepositoryRefusal> {
        let mut taken_out = Vec::with_capacity(paths.len());
        for path in paths {
            let on_disk = path.on_disk_under(&self.project_directory);
            let removed = std::fs::read(&on_disk)
                .and_then(|bytes_before| std::fs::remove_file(&on_disk).map(|()| bytes_before));
            match removed {
                Ok(bytes_before) => taken_out.push((on_disk, Some(bytes_before))),
                Err(not_removed) => {
                    self.restore_written_documents(taken_out);
                    return Err(not_removed.into());
                }
            }
        }
        Ok(taken_out)
    }

    /// Stage `operation` on the stream assets and their graph documents.
    fn apply_operation_to_stream_documents(
        &self,
        staged_assets: &mut BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &mut BTreeMap<Uuid, StreamGraphDocument>,
        stream_documents_taken_out: &mut BTreeSet<ProjectRelativeAssetPath>,
        operation: &ProjectStreamDocumentOperation,
    ) -> Result<(), ProjectRepositoryRefusal> {
        match operation {
            ProjectStreamDocumentOperation::CreateStreamDocument {
                stream,
                path,
                document,
            } => {
                refuse_a_stream_document_path_without_its_suffix(path)?;
                self.refuse_a_path_through_a_symbolic_link(path)?;
                if let Some(holder) = staged_assets.values().find(|asset| asset.path == *path) {
                    return Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset {
                        path: path.to_string(),
                        holder: holder.asset,
                    });
                }
                if !stream_documents_taken_out.contains(path)
                    && path.on_disk_under(&self.project_directory).exists()
                {
                    return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                        path: path.to_string(),
                    });
                }
                if let Some(earlier) = staged_assets.get(stream) {
                    return Err(ProjectRepositoryRefusal::AssetNamedTwice {
                        asset: *stream,
                        first: earlier.path.to_string(),
                        second: path.to_string(),
                    });
                }
                let unreadable = |why: String| ProjectRepositoryRefusal::GraphDocumentUnreadable {
                    path: path.to_string(),
                    why,
                };
                if document
                    .get(STREAM_DOCUMENT_ASSET_KEY)
                    .and_then(Value::as_str)
                    != Some(stream.to_string().as_str())
                {
                    return Err(unreadable(format!(
                        "its `{STREAM_DOCUMENT_ASSET_KEY}` does not name its stream asset {stream}"
                    )));
                }
                let staged_document =
                    StreamGraphDocument::from_value(document.clone()).map_err(unreadable)?;
                let bytes = staged_document.canonical_bytes()?;
                for (node, config) in staged_document.configs_of_every_node_at_every_level() {
                    refuse_a_config_referencing_an_asset_the_project_does_not_hold(
                        staged_assets,
                        node,
                        config,
                    )?;
                }
                staged_assets.insert(
                    *stream,
                    ProjectAsset {
                        asset: *stream,
                        kind: ProjectAssetKind::Stream,
                        content_sha256: content_sha256_of(&bytes),
                        content_byte_len: bytes.len() as u64,
                        import_settings: serde_json::Map::new(),
                        path: path.clone(),
                    },
                );
                staged_documents.insert(*stream, staged_document);
                stream_documents_taken_out.remove(path);
            }
            ProjectStreamDocumentOperation::DeleteStreamDocument {
                stream,
                path,
                document,
            } => {
                let stream_asset = staged_assets
                    .get(stream)
                    .filter(|asset| asset.kind == ProjectAssetKind::Stream && asset.path == *path)
                    .cloned()
                    .ok_or(ProjectRepositoryRefusal::NoSuchStream(*stream))?;
                let document_as_it_is = match staged_documents.get(stream) {
                    Some(staged_document) => staged_document.clone(),
                    None => self.read_stream_graph_document(staged_assets, *stream)?.1,
                };
                let document_as_it_was_written = StreamGraphDocument::from_value(document.clone())
                    .map_err(|why| ProjectRepositoryRefusal::GraphDocumentUnreadable {
                        path: path.to_string(),
                        why,
                    })?;
                if document_as_it_is.canonical_bytes()?
                    != document_as_it_was_written.canonical_bytes()?
                {
                    return Err(
                        ProjectRepositoryRefusal::StreamDocumentEditedSinceItWasWritten {
                            path: path.to_string(),
                        },
                    );
                }
                self.refuse_deleting_an_asset_a_graph_references(
                    &stream_asset,
                    staged_assets,
                    staged_documents,
                )?;
                staged_assets.remove(stream);
                staged_documents.remove(stream);
                stream_documents_taken_out.insert(path.clone());
            }
            ProjectStreamDocumentOperation::SetStreamDisplayName {
                stream,
                path,
                from,
                to,
            } => {
                if !staged_assets.get(stream).is_some_and(|asset| {
                    asset.kind == ProjectAssetKind::Stream && asset.path == *path
                }) {
                    return Err(ProjectRepositoryRefusal::NoSuchStream(*stream));
                }
                let staged_document = match staged_documents.entry(*stream) {
                    Entry::Occupied(staged) => staged.into_mut(),
                    Entry::Vacant(unstaged) => {
                        unstaged.insert(self.read_stream_graph_document(staged_assets, *stream)?.1)
                    }
                };
                if staged_document.display_name() != from.as_deref() {
                    return Err(
                        ProjectRepositoryRefusal::StreamDocumentEditedSinceItWasWritten {
                            path: path.to_string(),
                        },
                    );
                }
                staged_document.set_display_name(to.as_deref());
            }
        }
        Ok(())
    }

    fn restore_written_documents(&self, written_documents: Vec<GraphDocumentAsItWasBeforeAWrite>) {
        for (on_disk, bytes_before) in written_documents.into_iter().rev() {
            let restored = match bytes_before {
                Some(bytes) => write_file_whole(&on_disk, &bytes),
                None => std::fs::remove_file(&on_disk).map_err(ProjectRepositoryRefusal::from),
            };
            if let Err(refusal) = restored {
                tracing::error!(
                    project = %self.project_directory.display(),
                    document = %on_disk.display(),
                    %refusal,
                    "could not restore a graph document a refused transaction wrote"
                );
            }
        }
    }

    /// Refuse deleting `asset` while a graph document other than its own references it, as
    /// the transaction has staged that document or as it is on disk.
    fn refuse_deleting_an_asset_a_graph_references(
        &self,
        asset: &ProjectAsset,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let asset_text = asset.asset.to_string();
        let mut referencing_documents = Vec::new();
        for (stream, staged_document) in staged_documents {
            if *stream == asset.asset {
                continue;
            }
            let document: Value = serde_json::from_slice(&staged_document.canonical_bytes()?)?;
            if value_references_the_asset(&document, &asset_text) {
                referencing_documents.push(
                    staged_assets
                        .get(stream)
                        .map_or_else(|| stream.to_string(), |held| held.path.to_string()),
                );
            }
        }
        let staged_paths: BTreeSet<String> = staged_documents
            .keys()
            .filter_map(|stream| staged_assets.get(stream))
            .map(|held| held.path.to_string())
            .collect();
        for referencing in self.graph_documents_referencing(asset.asset, &asset.path)? {
            if !staged_paths.contains(&referencing) {
                referencing_documents.push(referencing);
            }
        }
        if referencing_documents.is_empty() {
            Ok(())
        } else {
            Err(ProjectRepositoryRefusal::AssetStillReferenced {
                asset: asset.asset,
                referencing_documents,
            })
        }
    }

    fn operation_for_change(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        change: ProjectAssetAction,
    ) -> Result<Option<StagedAssetChange>, ProjectRepositoryRefusal> {
        let staged_asset = |asset: Uuid| {
            staged_assets
                .get(&asset)
                .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))
        };
        match change {
            ProjectAssetAction::ImportAsset {
                source,
                asset_path,
                kind,
                import_settings,
            } => {
                let path = ProjectRelativeAssetPath::parse(&asset_path)?;
                self.refuse_a_path_through_a_symbolic_link(&path)?;
                let kind = kind
                    .unwrap_or_else(|| ProjectAssetKind::inferred_from_file_name(path.file_name()));
                if kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetsAreNotImported);
                }
                refuse_a_graph_document_path(&path)?;
                refuse_a_path_another_asset_holds(staged_assets, &path, None)?;
                let destination = path.on_disk_under(&self.project_directory);
                let asset = Uuid::new_v4();
                let (recorded, waiting) = match &source {
                    ProjectAssetImportSource::FileOnThisMachine(source_path) => {
                        if !source_path.is_absolute() {
                            return Err(ProjectRepositoryRefusal::ImportSourceUnreadable {
                                path: source_path.clone(),
                                why: "an import source is named by its absolute path".to_owned(),
                            });
                        }
                        if destination.exists() && !same_file(source_path, &destination) {
                            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                                path: path.to_string(),
                            });
                        }
                        if destination.exists() {
                            self.refuse_a_file_another_asset_already_is(staged_assets, &path)?;
                            (the_bytes_recorded_of_the_file_at(&destination)?, None)
                        } else {
                            let (copy, recorded) = copy_the_file_into_the_uploads_hashing_it(
                                &self.project_directory,
                                source_path,
                            )?;
                            (
                                recorded,
                                Some(AssetFileWaitingForItsImport {
                                    file: copy,
                                    removed_when_the_import_is_refused: true,
                                }),
                            )
                        }
                    }
                    ProjectAssetImportSource::UploadedContent(content_sha256) => {
                        if destination.exists() {
                            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                                path: path.to_string(),
                            });
                        }
                        (
                            self.the_bytes_recorded_of_the_upload(content_sha256)?,
                            Some(AssetFileWaitingForItsImport {
                                file: the_upload_file_of(&self.project_directory, content_sha256),
                                removed_when_the_import_is_refused: false,
                            }),
                        )
                    }
                };
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::Asset(ProjectAssetOperation::ImportAsset {
                        asset: ProjectAsset {
                            asset,
                            kind,
                            path,
                            content_sha256: recorded.content_sha256,
                            content_byte_len: recorded.content_byte_len,
                            import_settings,
                        },
                    }),
                    asset_file_waiting_for_its_import: waiting.map(|waiting| (asset, waiting)),
                }))
            }
            ProjectAssetAction::RelocateAsset { asset, to_path } => {
                let current = staged_asset(asset)?;
                let to = ProjectRelativeAssetPath::parse(&to_path)?;
                Ok(self
                    .relocation(staged_assets, current, to)?
                    .map(StagedAssetChange::of))
            }
            ProjectAssetAction::DeleteAsset { asset } => {
                let current = staged_asset(asset)?;
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                if current.kind == ProjectAssetKind::Stream {
                    return self.deletion_of_the_stream_document(staged_assets, asset);
                }
                Ok(Some(StagedAssetChange::of(
                    ProjectAssetOperation::DeleteAsset {
                        asset: current.clone(),
                    },
                )))
            }
            ProjectAssetAction::SetAssetSettings {
                asset,
                import_settings,
            } => {
                let current = staged_asset(asset)?;
                if current.kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetHasNoImportSettings(
                        asset,
                    ));
                }
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                if current.import_settings == import_settings {
                    return Ok(None);
                }
                Ok(Some(StagedAssetChange::of(
                    ProjectAssetOperation::SetAssetSettings {
                        asset,
                        path: current.path.clone(),
                        from: current.import_settings.clone(),
                        to: import_settings,
                    },
                )))
            }
            ProjectAssetAction::ReplaceAsset { asset, source } => {
                let current = staged_assets
                    .get(&asset)
                    .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))?;
                if current.kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetsAreNotReplaced(asset));
                }
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                let (recorded, waiting) = match &source {
                    ProjectAssetImportSource::FileOnThisMachine(source_path) => {
                        if !source_path.is_absolute() {
                            return Err(ProjectRepositoryRefusal::ImportSourceUnreadable {
                                path: source_path.clone(),
                                why: "a replacement's source is named by its absolute path"
                                    .to_owned(),
                            });
                        }
                        let (copy, recorded) = copy_the_file_into_the_uploads_hashing_it(
                            &self.project_directory,
                            source_path,
                        )?;
                        (
                            recorded,
                            AssetFileWaitingForItsImport {
                                file: copy,
                                removed_when_the_import_is_refused: true,
                            },
                        )
                    }
                    ProjectAssetImportSource::UploadedContent(content_sha256) => (
                        self.the_bytes_recorded_of_the_upload(content_sha256)?,
                        AssetFileWaitingForItsImport {
                            file: the_upload_file_of(&self.project_directory, content_sha256),
                            removed_when_the_import_is_refused: false,
                        },
                    ),
                };
                if recorded.content_sha256 == current.content_sha256 {
                    if waiting.removed_when_the_import_is_refused {
                        let _ = std::fs::remove_file(&waiting.file);
                    }
                    return Ok(None);
                }
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::Asset(
                        ProjectAssetOperation::ReplaceAssetContent {
                            asset,
                            path: current.path.clone(),
                            from: ProjectAssetContent {
                                content_sha256: current.content_sha256.clone(),
                                content_byte_len: current.content_byte_len,
                            },
                            to: ProjectAssetContent {
                                content_sha256: recorded.content_sha256,
                                content_byte_len: recorded.content_byte_len,
                            },
                        },
                    ),
                    asset_file_waiting_for_its_import: Some((asset, waiting)),
                }))
            }
            ProjectAssetAction::CreateStream { path, display_name } => {
                let display_name = refuse_a_display_name_that_shows_nothing(display_name)?;
                let path = ProjectRelativeAssetPath::parse(&path)?;
                let stream = Uuid::new_v4();
                let mut document = StreamGraphDocument::from_value(serde_json::json!({
                    STREAM_DOCUMENT_ASSET_KEY: stream,
                }))
                .map_err(|why| {
                    ProjectRepositoryRefusal::GraphDocumentUnreadable {
                        path: path.to_string(),
                        why,
                    }
                })?;
                document.set_display_name(Some(&display_name));
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::StreamDocument(
                        ProjectStreamDocumentOperation::CreateStreamDocument {
                            stream,
                            path,
                            document: document.to_value(),
                        },
                    ),
                    asset_file_waiting_for_its_import: None,
                }))
            }
            ProjectAssetAction::DeleteStream { stream_asset } => {
                let current = staged_assets
                    .get(&stream_asset)
                    .filter(|asset| asset.kind == ProjectAssetKind::Stream)
                    .ok_or(ProjectRepositoryRefusal::NoSuchStream(stream_asset))?;
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                self.deletion_of_the_stream_document(staged_assets, stream_asset)
            }
            ProjectAssetAction::SetStreamDisplayName {
                stream_asset,
                display_name,
            } => {
                let display_name = refuse_a_display_name_that_shows_nothing(display_name)?;
                let (path, document) =
                    self.read_stream_graph_document(staged_assets, stream_asset)?;
                let from = document.display_name().map(str::to_owned);
                if from.as_deref() == Some(display_name.as_str()) {
                    return Ok(None);
                }
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::StreamDocument(
                        ProjectStreamDocumentOperation::SetStreamDisplayName {
                            stream: stream_asset,
                            path,
                            from,
                            to: Some(display_name),
                        },
                    ),
                    asset_file_waiting_for_its_import: None,
                }))
            }
        }
    }

    /// The operation taking the stream asset `stream`'s graph document out, as it is on disk.
    fn deletion_of_the_stream_document(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        stream: Uuid,
    ) -> Result<Option<StagedAssetChange>, ProjectRepositoryRefusal> {
        let (path, document) = self.read_stream_graph_document(staged_assets, stream)?;
        Ok(Some(StagedAssetChange {
            operation: ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::DeleteStreamDocument {
                    stream,
                    path,
                    document: serde_json::from_slice(&document.canonical_bytes()?)?,
                },
            ),
            asset_file_waiting_for_its_import: None,
        }))
    }

    /// What an upload named `content_sha256` records, refused when the project holds no such
    /// upload or its bytes hash otherwise.
    fn the_bytes_recorded_of_the_upload(
        &self,
        content_sha256: &str,
    ) -> Result<AssetFileBytesRecorded, ProjectRepositoryRefusal> {
        let no_such_upload = || ProjectRepositoryRefusal::NoSuchUpload(content_sha256.to_owned());
        if !is_a_sha256_digest(content_sha256) {
            return Err(no_such_upload());
        }
        let upload = the_upload_file_of(&self.project_directory, content_sha256);
        if !upload.is_file() {
            return Err(no_such_upload());
        }
        let recorded = the_bytes_recorded_of_the_file_at(&upload)?;
        if !recorded.content_sha256.eq_ignore_ascii_case(content_sha256) {
            return Err(ProjectRepositoryRefusal::UploadHoldsOtherBytes {
                content_sha256: content_sha256.to_owned(),
                held_sha256: recorded.content_sha256,
            });
        }
        Ok(recorded)
    }

    fn relocation(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        current: &ProjectAsset,
        to: ProjectRelativeAssetPath,
    ) -> Result<Option<ProjectAssetOperation>, ProjectRepositoryRefusal> {
        if to == current.path {
            return Ok(None);
        }
        self.refuse_a_path_through_a_symbolic_link(&current.path)?;
        self.refuse_a_path_through_a_symbolic_link(&to)?;
        if current.kind == ProjectAssetKind::Stream {
            refuse_a_stream_document_path_without_its_suffix(&to)?;
        } else {
            refuse_a_graph_document_path(&to)?;
        }
        refuse_a_path_another_asset_holds(staged_assets, &to, Some(current.asset))?;
        if to.on_disk_under(&self.project_directory).exists() {
            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                path: to.to_string(),
            });
        }
        Ok(Some(ProjectAssetOperation::RelocateAsset {
            asset: current.asset,
            from: current.path.clone(),
            to,
        }))
    }

    fn perform_effect(
        &self,
        operation: &ProjectAssetOperation,
    ) -> Result<PerformedAssetEffect, ProjectRepositoryRefusal> {
        match operation {
            ProjectAssetOperation::ImportAsset { asset } => {
                let destination = asset.path.on_disk_under(&self.project_directory);
                let moved_from = match self.asset_files_waiting_for_their_import.get(&asset.asset) {
                    Some(waiting) if !destination.exists() => {
                        if let Some(parent) = destination.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::rename(&waiting.file, &destination)?;
                        Some(waiting.file.clone())
                    }
                    _ => None,
                };
                if let Err(refusal) = self.write_sidecar(asset) {
                    if let Some(moved_from) = &moved_from {
                        let _ = std::fs::rename(&destination, moved_from);
                    }
                    return Err(refusal);
                }
                Ok(match moved_from {
                    Some(moved_from) => PerformedAssetEffect::ImportThatPlacedItsFile {
                        asset: asset.clone(),
                        moved_from,
                    },
                    None => PerformedAssetEffect::Operation(operation.clone()),
                })
            }
            ProjectAssetOperation::DeleteAsset { asset } => {
                self.remove_the_sidecar_of(asset)?;
                let file = asset.path.on_disk_under(&self.project_directory);
                if !file.exists() {
                    return Ok(PerformedAssetEffect::Operation(operation.clone()));
                }
                let set_aside = self
                    .project_directory
                    .join(PROJECT_UPLOADS_DIRECTORY)
                    .join(format!(".deleted-{}", Uuid::new_v4()));
                let set = set_aside
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::rename(&file, &set_aside));
                if let Err(not_set_aside) = set {
                    self.restore_the_sidecar_of(asset);
                    return Err(not_set_aside.into());
                }
                Ok(PerformedAssetEffect::DeleteThatSetItsFileAside {
                    asset: asset.clone(),
                    set_aside,
                })
            }
            ProjectAssetOperation::RelocateAsset { from, to, .. } => {
                let from_file = from.on_disk_under(&self.project_directory);
                let to_file = to.on_disk_under(&self.project_directory);
                if let Some(parent) = to_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if from_file.exists() {
                    std::fs::rename(&from_file, &to_file)?;
                }
                if is_a_stream_document(from) {
                    return Ok(PerformedAssetEffect::Operation(operation.clone()));
                }
                if let Err(error) =
                    std::fs::rename(self.sidecar_on_disk(from), self.sidecar_on_disk(to))
                {
                    if to_file.exists() {
                        let _ = std::fs::rename(&to_file, &from_file);
                    }
                    return Err(error.into());
                }
                Ok(PerformedAssetEffect::Operation(operation.clone()))
            }
            ProjectAssetOperation::SetAssetSettings { path, to, .. } => {
                let sidecar_on_disk = self.sidecar_on_disk(path);
                let mut sidecar: ProjectAssetSidecar =
                    serde_json::from_slice(&std::fs::read(&sidecar_on_disk)?)?;
                sidecar.import_settings = to.clone();
                write_file_whole(&sidecar_on_disk, &canonical_project_json_bytes(&sidecar)?)?;
                Ok(PerformedAssetEffect::Operation(operation.clone()))
            }
            ProjectAssetOperation::ReplaceAssetContent {
                asset,
                path,
                from,
                to,
            } => {
                self.swap_in_the_bytes_of_a_replacement(*asset, path, from, to)?;
                let sidecar_on_disk = self.sidecar_on_disk(path);
                let mut sidecar: ProjectAssetSidecar =
                    serde_json::from_slice(&std::fs::read(&sidecar_on_disk)?)?;
                sidecar.content_sha256 = to.content_sha256.clone();
                sidecar.content_byte_len = to.content_byte_len;
                write_file_whole(&sidecar_on_disk, &canonical_project_json_bytes(&sidecar)?)?;
                Ok(PerformedAssetEffect::Operation(operation.clone()))
            }
        }
    }

    /// Put the bytes `to` names at the asset's `path` — the file its replacement waits with, or
    /// the upload of those bytes — keeping the bytes there now as the upload of `from`, so an
    /// undo swaps them back. Bytes neither waits nor is uploaded leave the file as it is.
    fn swap_in_the_bytes_of_a_replacement(
        &self,
        asset: Uuid,
        path: &ProjectRelativeAssetPath,
        from: &ProjectAssetContent,
        to: &ProjectAssetContent,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let replacement = self
            .asset_files_waiting_for_their_import
            .get(&asset)
            .map(|waiting| waiting.file.clone())
            .filter(|waiting_file| waiting_file.is_file())
            .unwrap_or_else(|| the_upload_file_of(&self.project_directory, &to.content_sha256));
        if !replacement.is_file() {
            return Ok(());
        }
        let destination = path.on_disk_under(&self.project_directory);
        if destination.is_file() {
            let kept_as_the_upload_of_the_earlier_bytes =
                the_upload_file_of(&self.project_directory, &from.content_sha256);
            if kept_as_the_upload_of_the_earlier_bytes.exists() {
                std::fs::remove_file(&destination)?;
            } else {
                std::fs::rename(&destination, &kept_as_the_upload_of_the_earlier_bytes)?;
            }
        }
        std::fs::rename(&replacement, &destination)?;
        Ok(())
    }

    fn undo_performed_effects(&self, performed_effects: Vec<PerformedAssetEffect>) {
        for performed in performed_effects.into_iter().rev() {
            let undone = match performed {
                PerformedAssetEffect::ImportThatPlacedItsFile { asset, moved_from } => {
                    self.remove_the_sidecar_of(&asset).and_then(|()| {
                        Ok(std::fs::rename(
                            asset.path.on_disk_under(&self.project_directory),
                            moved_from,
                        )?)
                    })
                }
                PerformedAssetEffect::Operation(ProjectAssetOperation::ImportAsset { asset }) => {
                    self.remove_the_sidecar_of(&asset)
                }
                PerformedAssetEffect::Operation(ProjectAssetOperation::DeleteAsset { asset }) => {
                    self.write_sidecar(&asset)
                }
                PerformedAssetEffect::DeleteThatSetItsFileAside { asset, set_aside } => {
                    self.restore_the_sidecar_of(&asset);
                    std::fs::rename(set_aside, asset.path.on_disk_under(&self.project_directory))
                        .map_err(ProjectRepositoryRefusal::from)
                }
                PerformedAssetEffect::Operation(operation) => {
                    self.perform_effect(&operation.inverse()).map(|_| ())
                }
            };
            if let Err(refusal) = undone {
                tracing::error!(
                    project = %self.project_directory.display(),
                    %refusal,
                    "could not undo part of a refused project transaction"
                );
            }
        }
    }

    fn restore_the_sidecar_of(&self, asset: &ProjectAsset) {
        if let Err(refusal) = self.write_sidecar(asset) {
            tracing::error!(
                project = %self.project_directory.display(),
                asset = %asset.asset,
                %refusal,
                "could not restore an asset's sidecar"
            );
        }
    }

    /// Refuse a path whose folders pass through a symbolic link, which could lead a change
    /// outside the project.
    fn refuse_a_path_through_a_symbolic_link(
        &self,
        path: &ProjectRelativeAssetPath,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let mut folder = self.project_directory.clone();
        for segment in path
            .directory()
            .split('/')
            .filter(|segment| !segment.is_empty())
        {
            folder.push(segment);
            if std::fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(ProjectRepositoryRefusal::AssetPathRefused {
                    path: path.to_string(),
                    why: "a folder on its way is a symbolic link, which could lead outside the project",
                });
            }
        }
        Ok(())
    }

    /// Write `asset`'s sidecar; a stream asset has none, its UUID being inside its document.
    fn write_sidecar(&self, asset: &ProjectAsset) -> Result<(), ProjectRepositoryRefusal> {
        if asset.kind == ProjectAssetKind::Stream {
            return Ok(());
        }
        write_file_whole(&self.sidecar_on_disk(&asset.path), &asset.sidecar_bytes()?)
    }

    fn remove_the_sidecar_of(&self, asset: &ProjectAsset) -> Result<(), ProjectRepositoryRefusal> {
        if asset.kind == ProjectAssetKind::Stream {
            return Ok(());
        }
        Ok(std::fs::remove_file(self.sidecar_on_disk(&asset.path))?)
    }

    fn sidecar_on_disk(&self, path: &ProjectRelativeAssetPath) -> PathBuf {
        self.project_directory.join(path.sidecar_path())
    }

    /// The whole project as its history records it with `assets`: every file the project's
    /// scan records, a media, scene or data asset as its sidecar alone, never its bytes.
    fn tracked_files(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        let asset_paths_recorded_by_their_sidecar: BTreeSet<&str> = assets
            .values()
            .filter(|asset| asset.kind != ProjectAssetKind::Stream)
            .map(|asset| asset.path.as_str())
            .collect();
        let mut tracked_files = BTreeMap::new();
        for project_file in project_files_the_history_records(&self.project_directory)? {
            if asset_paths_recorded_by_their_sidecar.contains(project_file.as_str()) {
                continue;
            }
            let bytes = std::fs::read(self.project_directory.join(&project_file))?;
            tracked_files.insert(project_file, bytes);
        }
        tracked_files.insert(
            PROJECT_FILE_NAME.to_owned(),
            self.project_file.canonical_bytes()?,
        );
        for asset in assets.values() {
            if asset.kind != ProjectAssetKind::Stream {
                tracked_files.insert(asset.path.sidecar_path(), asset.sidecar_bytes()?);
            }
        }
        Ok(tracked_files)
    }

    /// The project's `*.graph.json` files, leaving out any path an asset holds.
    fn graph_documents_beside(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        let mut graph_documents =
            project_files_ending_with(&self.project_directory, GRAPH_DOCUMENT_FILE_SUFFIX)?;
        graph_documents.retain(|graph_document| {
            !assets
                .values()
                .any(|asset| asset.path.as_str() == graph_document)
        });
        Ok(graph_documents)
    }

    /// Refuse an in-place import whose file the filesystem already resolves to another asset's.
    fn refuse_a_file_another_asset_already_is(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        path: &ProjectRelativeAssetPath,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let destination = path.on_disk_under(&self.project_directory);
        match assets.values().find(|asset| {
            same_file(
                &asset.path.on_disk_under(&self.project_directory),
                &destination,
            )
        }) {
            Some(holder) => Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset {
                path: path.to_string(),
                holder: holder.asset,
            }),
            None => Ok(()),
        }
    }

    /// Every graph document but the asset's own at `asset_path` that references `asset`.
    fn graph_documents_referencing(
        &self,
        asset: Uuid,
        asset_path: &ProjectRelativeAssetPath,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        let asset_text = asset.to_string();
        let mut referencing = Vec::new();
        for graph_document in self.graph_documents_beside(&self.assets_by_uuid)? {
            if graph_document == asset_path.as_str() {
                continue;
            }
            let bytes = std::fs::read(self.project_directory.join(&graph_document))?;
            let document = serde_json::from_slice::<Value>(&bytes).map_err(|not_parsed| {
                ProjectRepositoryRefusal::GraphDocumentUnreadable {
                    path: graph_document.clone(),
                    why: not_parsed.to_string(),
                }
            })?;
            if value_references_the_asset(&document, &asset_text) {
                referencing.push(graph_document);
            }
        }
        Ok(referencing)
    }
}

/// The commit each idempotency key in `history` was recorded under, read from each trailer's
/// envelope alone so a record in a newer format still yields its key.
fn the_commit_of_each_idempotency_key_in(
    history: &ProjectHistoryRepository,
) -> Result<HashMap<ProjectActionIdempotencyKey, String>, ProjectRepositoryRefusal> {
    let mut commit_of_each_idempotency_key = HashMap::new();
    for history_commit in history.commits_newest_first()? {
        let Some(transaction_json) = the_transaction_trailer_of(&history_commit.message) else {
            continue;
        };
        let envelope: ProjectTransactionEnvelopeHeader =
            match serde_json::from_str(transaction_json) {
                Ok(envelope) => envelope,
                Err(unreadable) => {
                    tracing::warn!(
                        commit = %history_commit.commit,
                        %unreadable,
                        "a transaction trailer has no readable envelope; its key is not indexed"
                    );
                    continue;
                }
            };
        if let Some(idempotency_key) = envelope.idempotency_key {
            commit_of_each_idempotency_key
                .entry(idempotency_key)
                .or_insert(history_commit.commit);
        }
    }
    Ok(commit_of_each_idempotency_key)
}

/// The transaction JSON a commit message carries in its trailer, if it carries one.
fn the_transaction_trailer_of(commit_message: &str) -> Option<&str> {
    commit_message.lines().find_map(|line| {
        line.strip_prefix(PROJECT_TRANSACTION_COMMIT_TRAILER)?
            .strip_prefix(": ")
    })
}

fn applied_transaction_read_from_its_trailer(
    transaction_json: &str,
    history_commit: ProjectHistoryCommit,
) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
    let envelope: ProjectTransactionEnvelopeHeader = serde_json::from_str(transaction_json)?;
    if envelope.format_version > PROJECT_TRANSACTION_FORMAT_VERSION {
        return Err(
            ProjectRepositoryRefusal::TransactionFormatNewerThanThisRuntime {
                transaction: envelope.transaction,
                format_version: envelope.format_version,
                newest_format_version: PROJECT_TRANSACTION_FORMAT_VERSION,
            },
        );
    }
    let transaction: ProjectTransaction = serde_json::from_str(transaction_json)?;
    Ok(AppliedProjectTransaction {
        transaction,
        commit: history_commit.commit,
        applied_at_unix_seconds: history_commit.committed_at_unix_seconds,
    })
}

fn transaction_commit_message(
    transaction: &ProjectTransaction,
) -> Result<String, ProjectRepositoryRefusal> {
    let summary = transaction
        .operations
        .iter()
        .map(ProjectOperation::summary)
        .collect::<Vec<_>>()
        .join("; ");
    Ok(format!(
        "{summary}\n\n{PROJECT_TRANSACTION_COMMIT_TRAILER}: {}\n",
        serde_json::to_string(transaction)?
    ))
}

fn apply_operation_to_assets(
    assets: &mut BTreeMap<Uuid, ProjectAsset>,
    operation: &ProjectAssetOperation,
) {
    match operation {
        ProjectAssetOperation::ImportAsset { asset } => {
            assets.insert(asset.asset, asset.clone());
        }
        ProjectAssetOperation::DeleteAsset { asset } => {
            assets.remove(&asset.asset);
        }
        ProjectAssetOperation::RelocateAsset { asset, to, .. } => {
            if let Some(relocated) = assets.get_mut(asset) {
                relocated.path = to.clone();
            }
        }
        ProjectAssetOperation::SetAssetSettings { asset, to, .. } => {
            if let Some(changed) = assets.get_mut(asset) {
                changed.import_settings = to.clone();
            }
        }
        ProjectAssetOperation::ReplaceAssetContent { asset, to, .. } => {
            if let Some(replaced) = assets.get_mut(asset) {
                replaced.content_sha256 = to.content_sha256.clone();
                replaced.content_byte_len = to.content_byte_len;
            }
        }
    }
}

fn refuse_a_config_referencing_an_asset_the_project_does_not_hold(
    assets: &BTreeMap<Uuid, ProjectAsset>,
    node: Uuid,
    config: &Value,
) -> Result<(), ProjectRepositoryRefusal> {
    match assets_a_node_config_references(config)
        .into_iter()
        .find(|referenced| !assets.contains_key(referenced))
    {
        Some(asset) => Err(ProjectRepositoryRefusal::ConfigReferencesNoAsset { node, asset }),
        None => Ok(()),
    }
}

fn refuse_a_graph_document_path(
    path: &ProjectRelativeAssetPath,
) -> Result<(), ProjectRepositoryRefusal> {
    if path.as_str().ends_with(GRAPH_DOCUMENT_FILE_SUFFIX) {
        return Err(ProjectRepositoryRefusal::AssetPathRefused {
            path: path.to_string(),
            why: "it names a stream's graph document, which only the stream verbs write",
        });
    }
    Ok(())
}

fn is_a_stream_document(path: &ProjectRelativeAssetPath) -> bool {
    path.as_str().ends_with(GRAPH_DOCUMENT_FILE_SUFFIX)
}

fn refuse_a_stream_document_path_without_its_suffix(
    path: &ProjectRelativeAssetPath,
) -> Result<(), ProjectRepositoryRefusal> {
    if !is_a_stream_document(path) {
        return Err(ProjectRepositoryRefusal::AssetPathRefused {
            path: path.to_string(),
            why: "a stream asset's graph document is named `<name>.graph.json`",
        });
    }
    Ok(())
}

fn refuse_a_path_another_asset_holds(
    assets: &BTreeMap<Uuid, ProjectAsset>,
    path: &ProjectRelativeAssetPath,
    except: Option<Uuid>,
) -> Result<(), ProjectRepositoryRefusal> {
    match assets.values().find(|asset| {
        asset.path.as_str().to_lowercase() == path.as_str().to_lowercase()
            && Some(asset.asset) != except
    }) {
        Some(holder) => Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset {
            path: path.to_string(),
            holder: holder.asset,
        }),
        None => Ok(()),
    }
}

fn value_references_the_import(value: &Value, project_text: &str) -> bool {
    match value {
        Value::Object(object) => {
            (object.contains_key("asset")
                && object.get("from").and_then(Value::as_str) == Some(project_text))
                || object
                    .values()
                    .any(|entry| value_references_the_import(entry, project_text))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| value_references_the_import(item, project_text)),
        _ => false,
    }
}

fn value_references_the_asset(value: &Value, asset_text: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get("asset").and_then(Value::as_str) == Some(asset_text)
                || object
                    .values()
                    .any(|entry| value_references_the_asset(entry, asset_text))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| value_references_the_asset(item, asset_text)),
        _ => false,
    }
}

/// The project enclosing `imported_directory` as its import of `imported_project`, with the
/// folder it holds it in: `<enclosing>/imports/<folder>` whose project file records it there.
fn the_project_enclosing_an_import(
    imported_directory: &Path,
    imported_project: Uuid,
) -> Option<(PathBuf, String)> {
    let folder = imported_directory.file_name()?.to_str()?.to_owned();
    let imports_folder = imported_directory.parent()?;
    if imports_folder.file_name()? != PROJECT_IMPORTS_DIRECTORY {
        return None;
    }
    let enclosing_directory = imports_folder.parent()?;
    let enclosing = TatolabProjectFile::read_from_project_directory(enclosing_directory).ok()?;
    enclosing
        .import_of(imported_project)
        .filter(|import| import.folder == folder)
        .map(|_| (enclosing_directory.to_path_buf(), folder))
}

fn apply_operation_to_imports(
    project_directory: &Path,
    project_file: &mut TatolabProjectFile,
    operation: &ProjectImportOperation,
) -> Result<(), ProjectRepositoryRefusal> {
    match operation {
        ProjectImportOperation::PlaceImport {
            import,
            replacing: None,
            ..
        } => {
            refuse_an_import_folder_name(&import.folder)?;
            if import.project == project_file.project {
                return Err(ProjectRepositoryRefusal::ProjectImportsItself(
                    import.project,
                ));
            }
            if project_file.import_of(import.project).is_some() {
                return Err(ProjectRepositoryRefusal::ImportAlreadyHeld(import.project));
            }
            if project_file
                .dependencies
                .iter()
                .any(|held| held.folder == import.folder)
                || the_import_folder_in(project_directory, &import.folder).exists()
            {
                return Err(ProjectRepositoryRefusal::ImportFolderAlreadyHeld {
                    folder: import.folder.clone(),
                });
            }
            project_file.dependencies.push(import.clone());
        }
        ProjectImportOperation::PlaceImport {
            import,
            replacing: Some(replaced),
            ..
        } => {
            refuse_an_import_folder_name(&import.folder)?;
            let held = project_file
                .dependencies
                .iter_mut()
                .find(|held| *held == replaced)
                .ok_or(ProjectRepositoryRefusal::NoSuchImport(replaced.project))?;
            *held = import.clone();
        }
        ProjectImportOperation::RemoveImport { import } => {
            if !project_file.dependencies.contains(import) {
                return Err(ProjectRepositoryRefusal::NoSuchImport(import.project));
            }
            project_file.dependencies.retain(|held| held != import);
        }
    }
    project_file
        .dependencies
        .sort_by(|left, right| left.folder.cmp(&right.folder));
    Ok(())
}

fn write_file_whole(path: &Path, bytes: &[u8]) -> Result<(), ProjectRepositoryRefusal> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension(format!("partial-{}", Uuid::new_v4()));
    if let Err(not_written) = std::fs::write(&partial, bytes) {
        let _ = std::fs::remove_file(&partial);
        return Err(not_written.into());
    }
    if let Err(error) = std::fs::rename(&partial, path) {
        let _ = std::fs::remove_file(&partial);
        return Err(error.into());
    }
    Ok(())
}

/// Device and inode decide it, because a case-folding or normalizing filesystem such as APFS
/// resolves differently spelled paths to one file while `canonicalize` keeps their spellings.
#[cfg(unix)]
fn same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn refuse_a_relative_project_directory(
    project_directory: &Path,
) -> Result<(), ProjectRepositoryRefusal> {
    if project_directory.is_absolute() {
        Ok(())
    } else {
        Err(ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(
            project_directory.to_path_buf(),
        ))
    }
}

fn refuse_an_author_without_a_name(
    author: &ProjectTransactionAuthor,
) -> Result<(), ProjectRepositoryRefusal> {
    if author.name.trim().is_empty() {
        Err(ProjectRepositoryRefusal::AuthorWithoutAName)
    } else {
        Ok(())
    }
}

/// `display_name` trimmed, refused when it shows nothing.
fn refuse_a_display_name_that_shows_nothing(
    display_name: String,
) -> Result<String, ProjectRepositoryRefusal> {
    let trimmed = display_name.trim();
    if trimmed.is_empty() {
        return Err(ProjectRepositoryRefusal::DisplayNameShowsNothing(
            display_name,
        ));
    }
    Ok(trimmed.to_owned())
}
