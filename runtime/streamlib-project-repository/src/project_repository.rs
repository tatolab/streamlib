// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::PROJECT_RUNTIME_DIRECTORY;
use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_asset::{
    GRAPH_DOCUMENT_FILE_SUFFIX, ProjectAsset, ProjectAssetKind, ProjectAssetSidecar,
    ProjectRelativeAssetPath, project_files_ending_with, project_files_the_history_records,
    read_every_asset_sidecar,
};
use crate::project_content_store::{ProjectContentStore, content_sha256_of};
use crate::project_file::{PROJECT_FILE_NAME, PinnedProjectDependency, TatolabProjectFile};
use crate::project_history_repository::{
    PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectHistoryRemote,
    ProjectHistoryRepository,
};

/// Where each dependency's history is fetched to, relative to the project: one bare Git
/// repository per depended-on project, named by its UUID.
pub const PROJECT_DEPENDENCIES_DIRECTORY: &str = ".tatolab/dependencies";
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_stream_document::project_stream_documents;
use crate::project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectDependencyOperation, ProjectOperation, ProjectTransaction,
    ProjectTransactionAuthor, ProjectTransactionLineage, ProjectTransactionReversal,
    StreamGraphOperationOnAStream,
};
use crate::stream_graph_document::{StreamGraphDocument, assets_a_node_config_references};

/// A project the runtime opened: the one writer of its files, its content store and its history.
pub struct ProjectRepository {
    project_directory: PathBuf,
    project_file: TatolabProjectFile,
    assets_by_uuid: BTreeMap<Uuid, ProjectAsset>,
    content_store: ProjectContentStore,
    history: ProjectHistoryRepository,
}

enum PerformedAssetEffect {
    Operation(ProjectAssetOperation),
    ImportThatPlacedItsFile(ProjectAsset),
    DeleteThatRemovedItsFile(ProjectAsset),
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
            content_store: ProjectContentStore::of_project(project_directory)?,
            history: ProjectHistoryRepository::create_for_project(project_directory)?,
            project_file,
            assets_by_uuid: BTreeMap::new(),
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
        let repository = Self::open(project_directory)?;
        if repository.history.head_commit()?.is_some() {
            return Err(ProjectRepositoryRefusal::ProjectAlreadyCommitted {
                project_directory: project_directory.to_path_buf(),
            });
        }
        repository.history.commit_tracked_files(
            &repository.tracked_files(&repository.assets_by_uuid)?,
            author,
            &format!("create_project {}", repository.project_file.name),
        )?;
        Ok(repository)
    }

    /// Open the project in `project_directory`, its assets read from their sidecars.
    pub fn open(project_directory: &Path) -> Result<Self, ProjectRepositoryRefusal> {
        refuse_a_relative_project_directory(project_directory)?;
        let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
        let assets_by_uuid = read_every_asset_sidecar(project_directory)?;
        Ok(Self {
            project_directory: project_directory.to_path_buf(),
            content_store: ProjectContentStore::of_project(project_directory)?,
            history: ProjectHistoryRepository::open_for_project(project_directory)?,
            project_file,
            assets_by_uuid,
        })
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

    /// The project's content store, which an upload writes into before its import.
    pub fn content_store(&self) -> &ProjectContentStore {
        &self.content_store
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
        let trailer_prefix = format!("{PROJECT_TRANSACTION_COMMIT_TRAILER}: ");
        let mut applied_transactions = Vec::new();
        for history_commit in self.history.commits_newest_first()? {
            if applied_transactions.len() == max_transactions {
                break;
            }
            let Some(transaction_json) = history_commit
                .message
                .lines()
                .find_map(|line| line.strip_prefix(&trailer_prefix))
            else {
                continue;
            };
            applied_transactions.push(AppliedProjectTransaction {
                transaction: serde_json::from_str(transaction_json)?,
                commit: history_commit.commit,
            });
        }
        Ok(applied_transactions)
    }

    /// Apply `changes` by `author` as one transaction, whole or not at all, and commit it.
    pub fn apply_asset_changes(
        &mut self,
        author: &ProjectTransactionAuthor,
        changes: Vec<ProjectAssetChangeRequest>,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        let mut staged_assets = self.every_asset()?;
        let mut operations = Vec::with_capacity(changes.len());
        for change in changes {
            if let Some(operation) = self.operation_for_change(&staged_assets, change)? {
                apply_operation_to_assets(&mut staged_assets, &operation);
                operations.push(ProjectOperation::Asset(operation));
            }
        }
        self.apply_project_operations(author, operations, ProjectTransactionLineage::NewEdit)
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
    /// files they change and commit it.
    pub fn apply_project_operations(
        &mut self,
        author: &ProjectTransactionAuthor,
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
        for operation in &operations {
            match operation {
                ProjectOperation::Dependency(dependency_operation) => {
                    apply_operation_to_dependencies(
                        &mut staged_project_file,
                        dependency_operation,
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
                    if let (Some(config), Some(node)) =
                        (graph_operation.config_it_sets(), graph_operation.node())
                    {
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
        let (undoes, redoes) = match lineage {
            ProjectTransactionLineage::NewEdit => (None, None),
            ProjectTransactionLineage::UndoOf(undone) => (Some(undone), None),
            ProjectTransactionLineage::RedoOf(redone) => (None, Some(redone)),
        };
        let transaction = ProjectTransaction {
            transaction: Uuid::new_v4(),
            author: author.clone(),
            parent_commit: self.history.head_commit()?,
            inverse_operations: operations
                .iter()
                .rev()
                .map(ProjectOperation::inverse)
                .collect(),
            operations,
            undoes,
            redoes,
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
        let written_documents = match self.write_staged_documents(&staged_assets, &staged_documents)
        {
            Ok(written_documents) => written_documents,
            Err(refusal) => {
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
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
            self.project_file = staged_project_file;
        }
        let committed = self
            .tracked_files(&staged_assets)
            .and_then(|tracked_files| {
                self.history.commit_tracked_files(
                    &tracked_files,
                    author,
                    &transaction_commit_message(&transaction)?,
                )
            });
        let commit = match committed {
            Ok(commit) => commit,
            Err(refusal) => {
                if self.project_file != project_file_as_it_was {
                    if let Err(not_restored) =
                        project_file_as_it_was.canonical_bytes().and_then(|bytes| {
                            write_file_whole(
                                &self.project_directory.join(PROJECT_FILE_NAME),
                                &bytes,
                            )
                        })
                    {
                        tracing::error!(
                            project = %self.project_directory.display(),
                            %not_restored,
                            "could not restore the project file after a refused commit"
                        );
                    }
                    self.project_file = project_file_as_it_was;
                }
                self.restore_written_documents(written_documents);
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
        };
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
        })
    }

    /// Depend on the project `source` holds — a project directory or a Git remote's URL — pinned
    /// to the commit `revision` names there, as one transaction by `author`: its history is
    /// fetched into [`PROJECT_DEPENDENCIES_DIRECTORY`] and the pin recorded in the project file.
    pub fn add_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        source: &str,
        revision: &str,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        let dependencies_directory = self.project_directory.join(PROJECT_DEPENDENCIES_DIRECTORY);
        let fetching_directory =
            dependencies_directory.join(format!(".fetching-{}", Uuid::new_v4()));
        let fetched = (|| {
            let fetching = ProjectHistoryRepository::open_or_create_at(&fetching_directory)?;
            fetching.fetch_every_branch_and_tag_from(&the_remote_a_source_names(source))?;
            let commit = fetching.commit_a_revision_names(revision)?;
            let project_file_at_the_pin =
                project_file_in(&fetching.files_at_commit(&commit)?, source)?;
            apply_operation_to_dependencies(
                &mut self.project_file.clone(),
                &ProjectDependencyOperation::AddDependency {
                    dependency: PinnedProjectDependency {
                        project: project_file_at_the_pin.project,
                        source: source.to_owned(),
                        revision: commit.clone(),
                    },
                },
            )?;
            Ok::<_, ProjectRepositoryRefusal>((commit, project_file_at_the_pin))
        })();
        let (commit, project_file_at_the_pin) = match fetched {
            Ok(fetched) => fetched,
            Err(refusal) => {
                let _ = std::fs::remove_dir_all(&fetching_directory);
                return Err(refusal);
            }
        };
        let dependency_history_directory =
            dependencies_directory.join(project_file_at_the_pin.project.to_string());
        let _ = std::fs::remove_dir_all(&dependency_history_directory);
        std::fs::rename(&fetching_directory, &dependency_history_directory)?;
        self.apply_project_operations(
            author,
            vec![ProjectOperation::Dependency(
                ProjectDependencyOperation::AddDependency {
                    dependency: PinnedProjectDependency {
                        project: project_file_at_the_pin.project,
                        source: source.to_owned(),
                        revision: commit,
                    },
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
    }

    /// Fetch the dependency `project` from its source again and move its pin to the commit
    /// `revision` names there, as one transaction by `author`.
    pub fn update_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        project: Uuid,
        revision: &str,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        let pinned = self.dependency(project)?;
        let history = self.dependency_history(project)?;
        history.fetch_every_branch_and_tag_from(&the_remote_a_source_names(&pinned.source))?;
        let commit = history.commit_a_revision_names(revision)?;
        let project_file_at_the_pin =
            project_file_in(&history.files_at_commit(&commit)?, &pinned.source)?;
        if project_file_at_the_pin.project != project {
            return Err(
                ProjectRepositoryRefusal::DependencySourceHoldsAnotherProject {
                    dependency_source: pinned.source,
                    expected: project,
                    held: project_file_at_the_pin.project,
                },
            );
        }
        self.apply_project_operations(
            author,
            vec![ProjectOperation::Dependency(
                ProjectDependencyOperation::MoveDependencyPin {
                    project,
                    from: pinned.revision,
                    to: commit,
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
    }

    /// The dependency `project` as the project file pins it.
    pub fn dependency(
        &self,
        project: Uuid,
    ) -> Result<PinnedProjectDependency, ProjectRepositoryRefusal> {
        self.project_file
            .dependencies
            .iter()
            .find(|held| held.project == project)
            .cloned()
            .ok_or(ProjectRepositoryRefusal::NoSuchDependency(project))
    }

    /// Every file of the dependency `project` at the commit it is pinned to, by path; read from
    /// its fetched history, with no network.
    pub fn dependency_files_at_its_pin(
        &self,
        project: Uuid,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        let pinned = self.dependency(project)?;
        self.dependency_history(project)?
            .files_at_commit(&pinned.revision)
    }

    fn dependency_history(
        &self,
        project: Uuid,
    ) -> Result<ProjectHistoryRepository, ProjectRepositoryRefusal> {
        let history_directory = self
            .project_directory
            .join(PROJECT_DEPENDENCIES_DIRECTORY)
            .join(project.to_string());
        if !history_directory.exists() {
            return Err(ProjectRepositoryRefusal::DependencyNotFetched {
                project,
                history_directory,
            });
        }
        ProjectHistoryRepository::open_or_create_at(&history_directory)
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
            operations: undone.inverse_operations.clone(),
            lineage: ProjectTransactionLineage::UndoOf(undone.transaction),
        })
    }

    /// The redo `author`'s latest undo asks for: the operations it undid.
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
            operations: redone.operations.clone(),
            lineage: ProjectTransactionLineage::RedoOf(redone.transaction),
        })
    }

    /// `author`'s transactions an undo reverses, newest last, and the ones a redo reapplies,
    /// newest last, replayed from history: an edit clears what could be redone.
    fn what_the_author_can_undo_and_redo(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<(Vec<ProjectTransaction>, Vec<ProjectTransaction>), ProjectRepositoryRefusal> {
        let mut undoable: Vec<ProjectTransaction> = Vec::new();
        let mut redoable: Vec<ProjectTransaction> = Vec::new();
        let mut transactions = self.applied_transactions_newest_first(usize::MAX)?;
        transactions.reverse();
        for AppliedProjectTransaction { transaction, .. } in transactions {
            if transaction.author != *author {
                continue;
            }
            match transaction.lineage() {
                ProjectTransactionLineage::NewEdit => {
                    redoable.clear();
                    undoable.push(transaction);
                }
                ProjectTransactionLineage::UndoOf(undone) => {
                    if let Some(position) =
                        undoable.iter().rposition(|held| held.transaction == undone)
                    {
                        redoable.push(undoable.remove(position));
                    }
                }
                ProjectTransactionLineage::RedoOf(redone) => {
                    if let Some(position) =
                        redoable.iter().rposition(|held| held.transaction == redone)
                    {
                        redoable.remove(position);
                        undoable.push(transaction);
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
        change: ProjectAssetChangeRequest,
    ) -> Result<Option<ProjectAssetOperation>, ProjectRepositoryRefusal> {
        let staged_asset = |asset: Uuid| {
            staged_assets
                .get(&asset)
                .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))
        };
        match change {
            ProjectAssetChangeRequest::ImportAsset {
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
                let stored = match &source {
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
                        }
                        self.content_store.store_the_file_at(source_path)?
                    }
                    ProjectAssetImportSource::StoredContent(content_sha256) => {
                        if destination.exists() {
                            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                                path: path.to_string(),
                            });
                        }
                        self.content_store.stored_content(content_sha256)?
                    }
                };
                Ok(Some(ProjectAssetOperation::ImportAsset {
                    asset: ProjectAsset {
                        asset: Uuid::new_v4(),
                        kind,
                        path,
                        content_sha256: stored.content_sha256,
                        content_byte_len: stored.content_byte_len,
                        import_settings,
                    },
                }))
            }
            ProjectAssetChangeRequest::RenameAsset { asset, file_name } => {
                let current = staged_asset(asset)?;
                let to = current.path.renamed_to(&file_name)?;
                self.relocation(staged_assets, current, to)
            }
            ProjectAssetChangeRequest::MoveAsset {
                asset,
                into_directory,
            } => {
                let current = staged_asset(asset)?;
                let to = current.path.moved_into_directory(&into_directory)?;
                self.relocation(staged_assets, current, to)
            }
            ProjectAssetChangeRequest::DeleteAsset { asset } => {
                let current = staged_asset(asset)?;
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                if current.kind == ProjectAssetKind::Stream {
                    self.content_store
                        .store_the_file_at(&current.path.on_disk_under(&self.project_directory))?;
                }
                Ok(Some(ProjectAssetOperation::DeleteAsset {
                    asset: current.clone(),
                }))
            }
            ProjectAssetChangeRequest::SetAssetSettings {
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
                Ok(Some(ProjectAssetOperation::SetAssetSettings {
                    asset,
                    path: current.path.clone(),
                    from: current.import_settings.clone(),
                    to: import_settings,
                }))
            }
        }
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
                let placed_its_file = !destination.exists();
                if placed_its_file {
                    self.content_store
                        .place_content_at(&asset.content_sha256, &destination)?;
                }
                if let Err(refusal) = self.write_sidecar(asset) {
                    if placed_its_file {
                        let _ = std::fs::remove_file(&destination);
                    }
                    return Err(refusal);
                }
                Ok(if placed_its_file {
                    PerformedAssetEffect::ImportThatPlacedItsFile(asset.clone())
                } else {
                    PerformedAssetEffect::Operation(operation.clone())
                })
            }
            ProjectAssetOperation::DeleteAsset { asset } => {
                self.remove_the_sidecar_of(asset)?;
                let file = asset.path.on_disk_under(&self.project_directory);
                if !file.exists() {
                    return Ok(PerformedAssetEffect::Operation(operation.clone()));
                }
                if let Err(not_removed) = std::fs::remove_file(file) {
                    self.restore_the_sidecar_of(asset);
                    return Err(not_removed.into());
                }
                Ok(PerformedAssetEffect::DeleteThatRemovedItsFile(
                    asset.clone(),
                ))
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
        }
    }

    fn undo_performed_effects(&self, performed_effects: Vec<PerformedAssetEffect>) {
        for performed in performed_effects.into_iter().rev() {
            let undone = match performed {
                PerformedAssetEffect::ImportThatPlacedItsFile(asset) => self
                    .perform_effect(&ProjectAssetOperation::DeleteAsset { asset })
                    .map(|_| ()),
                PerformedAssetEffect::Operation(ProjectAssetOperation::ImportAsset { asset }) => {
                    self.remove_the_sidecar_of(&asset)
                }
                PerformedAssetEffect::Operation(ProjectAssetOperation::DeleteAsset { asset }) => {
                    self.write_sidecar(&asset)
                }
                PerformedAssetEffect::DeleteThatRemovedItsFile(asset) => {
                    self.restore_the_sidecar_of(&asset);
                    self.content_store.place_content_at(
                        &asset.content_sha256,
                        &asset.path.on_disk_under(&self.project_directory),
                    )
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
    /// scan records, an asset whose bytes the content store holds as its sidecar alone.
    fn tracked_files(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        let content_store_asset_paths: BTreeSet<&str> = assets
            .values()
            .filter(|asset| asset.kind != ProjectAssetKind::Stream)
            .map(|asset| asset.path.as_str())
            .collect();
        let mut tracked_files = BTreeMap::new();
        for project_file in project_files_the_history_records(&self.project_directory)? {
            if content_store_asset_paths.contains(project_file.as_str()) {
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

/// The remote `source` names: a project directory's own history, or a Git remote's URL as
/// written.
fn the_remote_a_source_names(source: &str) -> ProjectHistoryRemote {
    let source_path = Path::new(source);
    let url = if source_path.join(PROJECT_FILE_NAME).is_file() {
        source_path
            .join(PROJECT_HISTORY_DIRECTORY)
            .display()
            .to_string()
    } else {
        source.to_owned()
    };
    ProjectHistoryRemote {
        url,
        bearer_token: None,
    }
}

fn project_file_in(
    files: &BTreeMap<String, Vec<u8>>,
    source: &str,
) -> Result<TatolabProjectFile, ProjectRepositoryRefusal> {
    let bytes = files.get(PROJECT_FILE_NAME).ok_or_else(|| {
        ProjectRepositoryRefusal::DependencySourceHoldsNoProject {
            dependency_source: source.to_owned(),
        }
    })?;
    serde_json::from_slice(bytes).map_err(|why| ProjectRepositoryRefusal::ProjectFileUnreadable {
        path: PathBuf::from(source).join(PROJECT_FILE_NAME),
        why: why.to_string(),
    })
}

fn apply_operation_to_dependencies(
    project_file: &mut TatolabProjectFile,
    operation: &ProjectDependencyOperation,
) -> Result<(), ProjectRepositoryRefusal> {
    match operation {
        ProjectDependencyOperation::AddDependency { dependency } => {
            if dependency.project == project_file.project {
                return Err(ProjectRepositoryRefusal::ProjectDependsOnItself(
                    dependency.project,
                ));
            }
            if project_file
                .dependencies
                .iter()
                .any(|held| held.project == dependency.project)
            {
                return Err(ProjectRepositoryRefusal::DependencyAlreadyHeld(
                    dependency.project,
                ));
            }
            project_file.dependencies.push(dependency.clone());
            project_file
                .dependencies
                .sort_by(|left, right| left.project.cmp(&right.project));
        }
        ProjectDependencyOperation::RemoveDependency { dependency } => {
            let before = project_file.dependencies.len();
            project_file
                .dependencies
                .retain(|held| held.project != dependency.project);
            if project_file.dependencies.len() == before {
                return Err(ProjectRepositoryRefusal::NoSuchDependency(
                    dependency.project,
                ));
            }
        }
        ProjectDependencyOperation::MoveDependencyPin { project, from, to } => {
            let held = project_file
                .dependencies
                .iter_mut()
                .find(|held| held.project == *project)
                .ok_or(ProjectRepositoryRefusal::NoSuchDependency(*project))?;
            if held.revision != *from {
                return Err(ProjectRepositoryRefusal::DependencyPinnedElsewhere {
                    project: *project,
                    pinned: held.revision.clone(),
                    expected: from.clone(),
                });
            }
            held.revision = to.clone();
        }
    }
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
