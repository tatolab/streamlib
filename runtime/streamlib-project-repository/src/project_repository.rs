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
    ASSET_SIDECAR_FILE_SUFFIX, GRAPH_DOCUMENT_FILE_SUFFIX, ProjectAsset, ProjectAssetForkOrigin,
    ProjectAssetKind, ProjectAssetSidecar, ProjectRelativeAssetPath, project_files_ending_with,
    project_files_the_history_records, read_every_asset_sidecar, the_dependency_asset_record_path,
    the_dependency_assets_recorded_at,
};
use crate::project_content_store::{
    PROJECT_CONTENT_STORE_DIRECTORY, ProjectContentStore, content_sha256_of,
};
use crate::project_dependency_source::{
    ProjectDependencySource, the_project_directory_an_installed_package_ships,
};
use crate::project_file::{PROJECT_FILE_NAME, PinnedProjectDependency, TatolabProjectFile};
use crate::project_history_repository::{
    PROJECT_HISTORY_BRANCH, PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectHistoryRepository,
};
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_stream_document::{STREAM_DOCUMENT_ASSET_KEY, project_stream_documents};
use crate::project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectDependencyOperation, ProjectOperation,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionLineage, ProjectTransactionReversal, StreamGraphOperationOnAStream,
};
use crate::stream_graph_document::{
    STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentNode, StreamGraphFragmentPlacement, StreamGraphOperation,
    asset_references_made_references_into_the_dependency, assets_a_node_config_references,
    config_with_a_dependency_asset_reference_made_local,
    the_refresh_of_each_fragment_holding_a_stream,
};

/// Where each dependency's history is fetched to, relative to the project: one bare Git
/// repository per depended-on project, named by its UUID.
pub const PROJECT_DEPENDENCIES_DIRECTORY: &str = ".tatolab/dependencies";

/// The top-level key of a forked graph document that records the stream it was copied from.
pub const FORKED_FROM_KEY: &str = "forked_from";

/// A pinned dependency as `dependency_listing` answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectDependencyListed {
    /// The pin as the project file holds it.
    pub dependency: PinnedProjectDependency,
    /// When a check for newer was asked, the newest commit of the source's `main`, or why the
    /// source could not be fetched.
    pub newest_on_the_source: Option<Result<String, String>>,
}

/// What a fork of a dependency's asset makes, before it is applied.
#[derive(Clone, Debug, PartialEq)]
pub struct DependencyAssetFork {
    /// The copy's new asset UUID.
    pub copy: Uuid,
    /// The copy written into this project: a stream's graph document, or another asset's
    /// sidecar beside its file, both recording what it was forked from.
    pub create_the_copy: ProjectOperation,
    /// Each node of this project that referenced the dependency's asset, in its config or a
    /// stored fragment, made to reference the copy.
    pub references_made_local: Vec<StreamGraphOperationOnAStream>,
}

impl DependencyAssetFork {
    /// The fork as the operations of one transaction: the copy first, then each reference.
    pub fn operations(&self) -> Vec<ProjectOperation> {
        std::iter::once(self.create_the_copy.clone())
            .chain(
                self.references_made_local
                    .iter()
                    .cloned()
                    .map(ProjectOperation::StreamGraph),
            )
            .collect()
    }
}

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
                forked_from: None,
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
            if let ProjectOperation::Dependency(ProjectDependencyOperation::RemoveDependency {
                dependency,
            }) = operation
            {
                self.refuse_removing_a_dependency_a_graph_references(
                    dependency.project,
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

    /// Depend on the project `source` holds — a [`ProjectDependencySource`] — pinned to the
    /// commit `revision` names there, as one transaction by `author`: its history is fetched
    /// into [`PROJECT_DEPENDENCIES_DIRECTORY`] and the pin recorded in the project file. A Git
    /// remote is sent `bearer_token`, which nothing keeps.
    pub fn add_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        source: &str,
        revision: &str,
        bearer_token: Option<&str>,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        refuse_a_project_directory_source_named_by_a_relative_path(source)?;
        let (commit, project_file_at_the_pin) =
            self.a_dependency_history_fetched_into_place(source, bearer_token, |fetched| {
                let commit = fetched.commit_a_revision_names(revision)?;
                let project_file_at_the_pin =
                    project_file_in(&fetched.files_at_commit(&commit)?, source)?;
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
                Ok((
                    project_file_at_the_pin.project,
                    (commit, project_file_at_the_pin),
                ))
            })?;
        self.record_and_copy_the_dependency_assets_at(
            &self.dependency_history(project_file_at_the_pin.project)?,
            project_file_at_the_pin.project,
            source,
            &commit,
        )?;
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

    /// Fetch the dependency `project` from its source again, a Git remote with `bearer_token`,
    /// and move its pin to the commit `revision` names there, as one transaction by `author`.
    pub fn update_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        project: Uuid,
        revision: &str,
        bearer_token: Option<&str>,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        let pin_move =
            self.the_pin_move_a_dependency_update_makes(project, revision, bearer_token)?;
        let ProjectDependencyOperation::MoveDependencyPin { to, .. } = &pin_move else {
            return Err(ProjectRepositoryRefusal::NoSuchDependency(project));
        };
        let refreshes =
            self.the_refresh_of_each_dependency_fragment_at(project, to, &BTreeSet::new())?;
        self.apply_project_operations(
            author,
            std::iter::once(ProjectOperation::Dependency(pin_move))
                .chain(refreshes.into_iter().map(ProjectOperation::StreamGraph))
                .collect(),
            ProjectTransactionLineage::NewEdit,
        )
    }

    /// The refresh of each top-level fragment holding a stream of the dependency `project`, in
    /// every stream of this project but those of `skipping_streams`, from that stream as the
    /// dependency's history holds it at `commit`, each node keeping its UUID by name.
    pub fn the_refresh_of_each_dependency_fragment_at(
        &self,
        project: Uuid,
        commit: &str,
        skipping_streams: &BTreeSet<Uuid>,
    ) -> Result<Vec<StreamGraphOperationOnAStream>, ProjectRepositoryRefusal> {
        let project_text = project.to_string();
        let assets = self.every_asset()?;
        let mut dependency_stream_documents: BTreeMap<Uuid, Value> = BTreeMap::new();
        let mut refreshes = Vec::new();
        for holder in assets.values().filter(|asset| {
            asset.kind == ProjectAssetKind::Stream && !skipping_streams.contains(&asset.asset)
        }) {
            let (holder_path, holder_document) =
                self.read_stream_graph_document(&assets, holder.asset)?;
            let mut held_streams: Vec<Value> = Vec::new();
            for fragment_node in holder_document.fragment_nodes() {
                let held_stream = &fragment_node.config[STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY];
                if fragment_node.node_type == STREAM_FRAGMENT_NODE_TYPE
                    && held_stream["from"].as_str() == Some(project_text.as_str())
                    && !held_streams.contains(held_stream)
                {
                    held_streams.push(held_stream.clone());
                }
            }
            for held_stream in held_streams {
                let Some(dependency_stream) = held_stream["asset"]
                    .as_str()
                    .and_then(|asset| Uuid::parse_str(asset).ok())
                else {
                    continue;
                };
                let dependency_stream_document =
                    match dependency_stream_documents.entry(dependency_stream) {
                        Entry::Occupied(read) => read.into_mut(),
                        Entry::Vacant(unread) => unread.insert(
                            self.dependency_stream_at_commit(project, dependency_stream, commit)?
                                .1,
                        ),
                    };
                let graph_operations = the_refresh_of_each_fragment_holding_a_stream(
                    &holder_document,
                    &held_stream,
                    dependency_stream_document,
                )
                .map_err(|why| {
                    ProjectRepositoryRefusal::GraphOperationRefused {
                        path: holder_path.to_string(),
                        why,
                    }
                })?;
                refreshes.extend(graph_operations.into_iter().map(|graph_operation| {
                    StreamGraphOperationOnAStream {
                        stream: holder.asset,
                        graph_operation,
                    }
                }));
            }
        }
        Ok(refreshes)
    }

    /// Fetch the dependency `project` from its source again, a Git remote with `bearer_token`,
    /// and answer the move of its pin to the commit `revision` names there, applying nothing.
    pub fn the_pin_move_a_dependency_update_makes(
        &self,
        project: Uuid,
        revision: &str,
        bearer_token: Option<&str>,
    ) -> Result<ProjectDependencyOperation, ProjectRepositoryRefusal> {
        let pinned = self.dependency(project)?;
        let history = self.dependency_history_fetched_with(project, bearer_token)?;
        history.fetch_every_branch_and_tag_from(
            &ProjectDependencySource::named_by(&pinned.source)
                .the_remote_to_fetch_from(&self.project_directory, bearer_token)?,
        )?;
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
        self.record_and_copy_the_dependency_assets_at(&history, project, &pinned.source, &commit)?;
        Ok(ProjectDependencyOperation::MoveDependencyPin {
            project,
            from: pinned.revision,
            to: commit,
        })
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
        self.dependency_files_at_commit(project, &pinned.revision)
    }

    /// Every file of the dependency `project` at `commit` of its fetched history, by path.
    pub fn dependency_files_at_commit(
        &self,
        project: Uuid,
        commit: &str,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        self.dependency_history(project)?.files_at_commit(commit)
    }

    /// The graph document of the dependency `project`'s stream asset `stream` at `commit` of
    /// its fetched history, with its path there.
    pub fn dependency_stream_at_commit(
        &self,
        project: Uuid,
        stream: Uuid,
        commit: &str,
    ) -> Result<(ProjectRelativeAssetPath, Value), ProjectRepositoryRefusal> {
        for (path, bytes) in self.dependency_files_at_commit(project, commit)? {
            if !path.ends_with(GRAPH_DOCUMENT_FILE_SUFFIX) {
                continue;
            }
            let Ok(graph_document) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if graph_document
                .get(STREAM_DOCUMENT_ASSET_KEY)
                .and_then(Value::as_str)
                .and_then(|asset| Uuid::parse_str(asset).ok())
                == Some(stream)
            {
                return Ok((ProjectRelativeAssetPath::parse(&path)?, graph_document));
            }
        }
        Err(ProjectRepositoryRefusal::DependencyHoldsNoStream {
            project,
            stream,
            commit: commit.to_owned(),
        })
    }

    /// What a fork of the dependency `project`'s asset `asset`, at its pin, into this project
    /// makes: a stream's graph document copied to `path` under a new stream asset UUID, or
    /// another asset's sidecar written beside its file at `path` under a new UUID, its bytes
    /// those the fetch already copied into this project's content store, each recording the
    /// asset, project and commit it came from under `forked_from`; and every node of this
    /// project's streams that referenced it, at any level of a stored fragment, made to
    /// reference the copy. Refused when a node referencing it holds no UUID to name it by.
    pub fn the_fork_of_a_dependency_asset(
        &self,
        project: Uuid,
        asset: Uuid,
        path: Option<&str>,
    ) -> Result<DependencyAssetFork, ProjectRepositoryRefusal> {
        let pinned = self.dependency(project)?;
        let copy = Uuid::new_v4();
        let create_the_copy =
            match self.dependency_stream_at_commit(project, asset, &pinned.revision) {
                Ok((path_in_the_dependency, document)) => self.the_copy_of_a_dependency_stream(
                    &pinned,
                    asset,
                    copy,
                    path_in_the_dependency,
                    document,
                    path,
                )?,
                Err(ProjectRepositoryRefusal::DependencyHoldsNoStream { .. }) => {
                    self.the_copy_of_a_dependency_asset(&pinned, asset, copy, path)?
                }
                Err(refusal) => return Err(refusal),
            };
        Ok(DependencyAssetFork {
            copy,
            create_the_copy,
            references_made_local: self
                .each_reference_to_a_dependency_asset_made_local(project, asset, copy)?,
        })
    }

    fn the_copy_of_a_dependency_stream(
        &self,
        pinned: &PinnedProjectDependency,
        stream: Uuid,
        copy: Uuid,
        path_in_the_dependency: ProjectRelativeAssetPath,
        mut document: Value,
        path: Option<&str>,
    ) -> Result<ProjectOperation, ProjectRepositoryRefusal> {
        let path = match path {
            Some(path) => ProjectRelativeAssetPath::parse(path)?,
            None => {
                let file_name = path_in_the_dependency.file_name();
                let stream_name = file_name
                    .strip_suffix(GRAPH_DOCUMENT_FILE_SUFFIX)
                    .unwrap_or(file_name);
                ProjectRelativeAssetPath::parse(&format!(
                    "streams/{stream_name}-fork{GRAPH_DOCUMENT_FILE_SUFFIX}"
                ))?
            }
        };
        let document_object = document.as_object_mut().ok_or_else(|| {
            ProjectRepositoryRefusal::GraphDocumentUnreadable {
                path: path_in_the_dependency.to_string(),
                why: "it is no JSON object".to_owned(),
            }
        })?;
        if let Some(nodes) = document_object.get_mut("nodes") {
            asset_references_made_references_into_the_dependency(nodes, pinned.project);
        }
        document_object.insert(
            STREAM_DOCUMENT_ASSET_KEY.to_owned(),
            Value::String(copy.to_string()),
        );
        document_object.insert(
            FORKED_FROM_KEY.to_owned(),
            serde_json::json!({
                "asset": stream.to_string(),
                "from": pinned.project.to_string(),
                "revision": pinned.revision,
            }),
        );
        Ok(ProjectOperation::StreamDocument(
            ProjectStreamDocumentOperation::CreateStreamDocument {
                stream: copy,
                path,
                document,
            },
        ))
    }

    fn the_copy_of_a_dependency_asset(
        &self,
        pinned: &PinnedProjectDependency,
        asset: Uuid,
        copy: Uuid,
        path: Option<&str>,
    ) -> Result<ProjectOperation, ProjectRepositoryRefusal> {
        let recorded_assets = match the_dependency_assets_recorded_at(
            &self.project_directory,
            pinned.project,
            &pinned.revision,
        ) {
            Err(ProjectRepositoryRefusal::DependencyAssetsNotRecordedAtThePin { .. }) => self
                .record_and_copy_the_dependency_assets_at(
                    &self.dependency_history(pinned.project)?,
                    pinned.project,
                    &pinned.source,
                    &pinned.revision,
                )?,
            recorded => recorded?,
        };
        let held = recorded_assets
            .into_iter()
            .find(|held| held.asset == asset)
            .ok_or_else(|| ProjectRepositoryRefusal::DependencyHoldsNoAsset {
                project: pinned.project,
                asset,
                commit: pinned.revision.clone(),
            })?;
        self.content_store
            .stored_content(&held.content_sha256)
            .map_err(
                |_| ProjectRepositoryRefusal::DependencyAssetContentNotFetched {
                    project: pinned.project,
                    asset,
                    dependency_source: pinned.source.clone(),
                },
            )?;
        let path = match path {
            Some(path) => ProjectRelativeAssetPath::parse(path)?,
            None => {
                let file_name = held.path.file_name();
                let fork_file_name = match file_name.rsplit_once('.') {
                    Some((stem, extension)) if !stem.is_empty() => {
                        format!("{stem}-fork.{extension}")
                    }
                    _ => format!("{file_name}-fork"),
                };
                let directory = held.path.directory();
                ProjectRelativeAssetPath::parse(&if directory.is_empty() {
                    fork_file_name
                } else {
                    format!("{directory}/{fork_file_name}")
                })?
            }
        };
        refuse_a_graph_document_path(&path)?;
        self.refuse_a_path_through_a_symbolic_link(&path)?;
        refuse_a_path_another_asset_holds(&self.every_asset()?, &path, None)?;
        if path.on_disk_under(&self.project_directory).exists() {
            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                path: path.to_string(),
            });
        }
        Ok(ProjectOperation::Asset(
            ProjectAssetOperation::ImportAsset {
                asset: ProjectAsset {
                    asset: copy,
                    kind: held.kind,
                    path,
                    content_sha256: held.content_sha256,
                    content_byte_len: held.content_byte_len,
                    import_settings: held.import_settings,
                    forked_from: Some(ProjectAssetForkOrigin {
                        asset,
                        from: pinned.project,
                        revision: pinned.revision.clone(),
                    }),
                },
            },
        ))
    }

    /// The operation pointing each node of this project's streams that references the
    /// dependency `project`'s asset `asset`, at any level of a stored fragment, at `copy`.
    fn each_reference_to_a_dependency_asset_made_local(
        &self,
        project: Uuid,
        asset: Uuid,
        copy: Uuid,
    ) -> Result<Vec<StreamGraphOperationOnAStream>, ProjectRepositoryRefusal> {
        let assets = self.every_asset()?;
        let mut references_made_local = Vec::new();
        for this_projects_stream in assets
            .values()
            .filter(|held| held.kind == ProjectAssetKind::Stream)
        {
            let (stream_path, stream_document) =
                self.read_stream_graph_document(&assets, this_projects_stream.asset)?;
            if let Some((node_name, _)) =
                stream_document
                    .nodes_holding_no_uuid()
                    .into_iter()
                    .find(|(_, node_object)| {
                        config_with_a_dependency_asset_reference_made_local(
                            node_object,
                            project,
                            asset,
                            copy,
                        )
                        .is_some()
                    })
            {
                return Err(ProjectRepositoryRefusal::ForkReachesANodeHoldingNoUuid {
                    path: stream_path.to_string(),
                    node_name,
                });
            }
            for node in stream_document.nodes() {
                if let Some(graph_operation) =
                    the_operation_pointing_a_node_at_a_forked_copy(&node, project, asset, copy)
                {
                    references_made_local.push(StreamGraphOperationOnAStream {
                        stream: this_projects_stream.asset,
                        graph_operation,
                    });
                }
            }
        }
        Ok(references_made_local)
    }

    /// Fork the dependency `project`'s asset `asset` at its pin into this project, as one
    /// transaction by `author`: [`Self::the_fork_of_a_dependency_asset`], applied.
    pub fn fork_dependency_asset(
        &mut self,
        author: &ProjectTransactionAuthor,
        project: Uuid,
        asset: Uuid,
        path: Option<&str>,
    ) -> Result<(DependencyAssetFork, AppliedProjectTransaction), ProjectRepositoryRefusal> {
        let fork = self.the_fork_of_a_dependency_asset(project, asset, path)?;
        let applied = self.apply_project_operations(
            author,
            fork.operations(),
            ProjectTransactionLineage::NewEdit,
        )?;
        Ok((fork, applied))
    }

    /// Record every asset with a sidecar the dependency `project` holds at `commit` of
    /// `history`, for a reader with no history, and copy into this project's content store the
    /// bytes of each that it lacks and that the content store of `source`, when it is a
    /// project on this machine, holds. Bytes neither store holds are left for a read of them
    /// to refuse by name.
    fn record_and_copy_the_dependency_assets_at(
        &self,
        history: &ProjectHistoryRepository,
        project: Uuid,
        source: &str,
        commit: &str,
    ) -> Result<Vec<ProjectAsset>, ProjectRepositoryRefusal> {
        let mut recorded_assets = Vec::new();
        for (file_path, bytes) in history.files_at_commit(commit)? {
            let Some(asset_path) = file_path.strip_suffix(ASSET_SIDECAR_FILE_SUFFIX) else {
                continue;
            };
            let read = ProjectRelativeAssetPath::parse(asset_path).and_then(|asset_path| {
                let sidecar: ProjectAssetSidecar = serde_json::from_slice(&bytes)?;
                Ok(ProjectAsset::from_sidecar(asset_path, sidecar))
            });
            match read {
                Ok(recorded_asset) => recorded_assets.push(recorded_asset),
                Err(refusal) => tracing::warn!(
                    dependency = %project,
                    %commit,
                    sidecar = %file_path,
                    %refusal,
                    "a dependency's sidecar that does not parse names no asset of it"
                ),
            }
        }
        let record_path =
            the_dependency_asset_record_path(&self.project_directory, project, commit);
        if let Some(record_directory) = record_path.parent() {
            std::fs::create_dir_all(record_directory)?;
        }
        let partial_record_path =
            record_path.with_extension(format!("incoming-{}", Uuid::new_v4()));
        std::fs::write(
            &partial_record_path,
            canonical_project_json_bytes(&recorded_assets)?,
        )?;
        if let Err(not_placed) = std::fs::rename(&partial_record_path, &record_path) {
            let _ = std::fs::remove_file(&partial_record_path);
            return Err(not_placed.into());
        }

        let source_content_store_directory = match ProjectDependencySource::named_by(source) {
            ProjectDependencySource::ProjectDirectory(source_project_directory) => {
                Some(source_project_directory.join(PROJECT_CONTENT_STORE_DIRECTORY))
            }
            ProjectDependencySource::PythonPackageShippingAProject { distribution } => {
                the_project_directory_an_installed_package_ships(
                    &self.project_directory,
                    &distribution,
                )
                .ok()
                .map(|shipped| shipped.join(PROJECT_CONTENT_STORE_DIRECTORY))
            }
            ProjectDependencySource::GitRemote(_) => None,
        };
        for recorded_asset in &recorded_assets {
            if self
                .content_store
                .stored_content(&recorded_asset.content_sha256)
                .is_ok()
            {
                continue;
            }
            let Some(source_content) = source_content_store_directory
                .as_ref()
                .map(|directory| directory.join(&recorded_asset.content_sha256))
                .filter(|source_content| source_content.is_file())
            else {
                tracing::warn!(
                    dependency = %project,
                    asset = %recorded_asset.asset,
                    %source,
                    "a dependency's asset bytes are not on this machine, so reading them is refused"
                );
                continue;
            };
            let stored = self.content_store.store_the_file_at(&source_content)?;
            if stored.content_sha256 != recorded_asset.content_sha256 {
                tracing::warn!(
                    dependency = %project,
                    asset = %recorded_asset.asset,
                    expected = %recorded_asset.content_sha256,
                    held = %stored.content_sha256,
                    "a dependency's content store holds other bytes under an asset's hash"
                );
            }
        }
        Ok(recorded_assets)
    }

    /// Every dependency the project file pins, each with, when `check_for_newer`, the newest
    /// commit of its source's `main`, fetched with `bearer_token` for a Git remote; a source
    /// that cannot be fetched answers why beside its pin instead.
    pub fn dependency_listing(
        &self,
        check_for_newer: bool,
        bearer_token: Option<&str>,
    ) -> Vec<ProjectDependencyListed> {
        self.project_file
            .dependencies
            .iter()
            .map(|pinned| {
                let newest_on_the_source = check_for_newer.then(|| {
                    self.newest_revision_of_a_dependencys_source(pinned, bearer_token)
                        .map_err(|refusal| refusal.to_string())
                });
                ProjectDependencyListed {
                    dependency: pinned.clone(),
                    newest_on_the_source,
                }
            })
            .collect()
    }

    fn newest_revision_of_a_dependencys_source(
        &self,
        pinned: &PinnedProjectDependency,
        bearer_token: Option<&str>,
    ) -> Result<String, ProjectRepositoryRefusal> {
        let history = self.dependency_history_fetched_with(pinned.project, bearer_token)?;
        history.fetch_every_branch_and_tag_from(
            &ProjectDependencySource::named_by(&pinned.source)
                .the_remote_to_fetch_from(&self.project_directory, bearer_token)?,
        )?;
        history.commit_a_revision_names(&format!("refs/heads/{PROJECT_HISTORY_BRANCH}"))
    }

    /// Stop depending on `project`, as one transaction by `author`: its pin leaves the project
    /// file and its fetched history stays, so undo can put the pin back. Refused while a node
    /// of this project's streams still references one of its assets.
    pub fn remove_dependency(
        &mut self,
        author: &ProjectTransactionAuthor,
        project: Uuid,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        let dependency = self.dependency(project)?;
        self.apply_project_operations(
            author,
            vec![ProjectOperation::Dependency(
                ProjectDependencyOperation::RemoveDependency { dependency },
            )],
            ProjectTransactionLineage::NewEdit,
        )
    }

    /// Refuse removing the dependency `project` while a node of a stream, as the transaction has
    /// staged its document or as it is on disk, references one of its assets, at any depth of
    /// its config or the fragment it holds.
    fn refuse_removing_a_dependency_a_graph_references(
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
                if value_references_an_asset_of_the_dependency(node, &project_text) {
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
            Err(ProjectRepositoryRefusal::DependencyStillReferenced {
                project,
                referencing_nodes,
            })
        }
    }

    /// The fetched history of the dependency `project`, fetched again from the source its pin
    /// records when it is missing, as in a clone of the project, the pin left where it is.
    fn dependency_history(
        &self,
        project: Uuid,
    ) -> Result<ProjectHistoryRepository, ProjectRepositoryRefusal> {
        self.dependency_history_fetched_with(project, None)
    }

    /// The fetched history of the dependency `project`, fetched again from the source its pin
    /// records, a Git remote with `bearer_token`, when it is missing, as in a clone.
    fn dependency_history_fetched_with(
        &self,
        project: Uuid,
        bearer_token: Option<&str>,
    ) -> Result<ProjectHistoryRepository, ProjectRepositoryRefusal> {
        let history_directory = self.dependency_history_directory(project);
        if !history_directory.exists() {
            let pinned = self.dependency(project)?;
            self.a_dependency_history_fetched_into_place(
                &pinned.source,
                bearer_token,
                |fetched| {
                    let commit = fetched.commit_a_revision_names(&pinned.revision)?;
                    let project_file_at_the_pin =
                        project_file_in(&fetched.files_at_commit(&commit)?, &pinned.source)?;
                    if project_file_at_the_pin.project != project {
                        return Err(
                            ProjectRepositoryRefusal::DependencySourceHoldsAnotherProject {
                                dependency_source: pinned.source.clone(),
                                expected: project,
                                held: project_file_at_the_pin.project,
                            },
                        );
                    }
                    Ok((project, ()))
                },
            )?;
            tracing::info!(
                project = %self.project_directory.display(),
                dependency = %project,
                "fetched a pinned dependency's missing history again from its source"
            );
            let history = ProjectHistoryRepository::open_or_create_at(&history_directory)?;
            let pinned = self.dependency(project)?;
            self.record_and_copy_the_dependency_assets_at(
                &history,
                project,
                &pinned.source,
                &pinned.revision,
            )?;
            return Ok(history);
        }
        ProjectHistoryRepository::open_or_create_at(&history_directory)
    }

    fn dependency_history_directory(&self, project: Uuid) -> PathBuf {
        self.project_directory
            .join(PROJECT_DEPENDENCIES_DIRECTORY)
            .join(project.to_string())
    }

    /// Fetch every branch and tag of `source` into a directory of its own and answer what
    /// `read_the_fetched_history` reads of it, then put it in place as the fetched history of
    /// the project that read names; nothing is left behind when the fetch or the read refuses.
    fn a_dependency_history_fetched_into_place<WhatTheFetchedHistoryHolds>(
        &self,
        source: &str,
        bearer_token: Option<&str>,
        read_the_fetched_history: impl FnOnce(
            &ProjectHistoryRepository,
        ) -> Result<
            (Uuid, WhatTheFetchedHistoryHolds),
            ProjectRepositoryRefusal,
        >,
    ) -> Result<WhatTheFetchedHistoryHolds, ProjectRepositoryRefusal> {
        let fetching_directory = self
            .project_directory
            .join(PROJECT_DEPENDENCIES_DIRECTORY)
            .join(format!(".fetching-{}", Uuid::new_v4()));
        let fetched =
            ProjectHistoryRepository::open_or_create_at(&fetching_directory).and_then(|fetching| {
                fetching.fetch_every_branch_and_tag_from(
                    &ProjectDependencySource::named_by(source)
                        .the_remote_to_fetch_from(&self.project_directory, bearer_token)?,
                )?;
                read_the_fetched_history(&fetching)
            });
        let (project, what_the_fetched_history_holds) = match fetched {
            Ok(fetched) => fetched,
            Err(refusal) => {
                let _ = std::fs::remove_dir_all(&fetching_directory);
                return Err(refusal);
            }
        };
        let dependency_history_directory = self.dependency_history_directory(project);
        let _ = std::fs::remove_dir_all(&dependency_history_directory);
        if let Err(not_placed) = std::fs::rename(&fetching_directory, &dependency_history_directory)
        {
            let _ = std::fs::remove_dir_all(&fetching_directory);
            return Err(not_placed.into());
        }
        Ok(what_the_fetched_history_holds)
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
                        forked_from: None,
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
                        forked_from: None,
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

/// The operation pointing `node` at the copy `copy` of the dependency `project`'s stream
/// `stream`, wherever it references that stream: its config alone, or, for a fragment node
/// whose stored nodes reference it at any level, a refresh of the fragment with both rewritten.
fn the_operation_pointing_a_node_at_a_forked_copy(
    node: &StreamGraphDocumentNode,
    project: Uuid,
    stream: Uuid,
    copy: Uuid,
) -> Option<StreamGraphOperation> {
    let config_made_local =
        config_with_a_dependency_asset_reference_made_local(&node.config, project, stream, copy);
    let fragment_made_local = node.fragment.as_deref().and_then(|fragment| {
        config_with_a_dependency_asset_reference_made_local(fragment, project, stream, copy)
    });
    match (node.fragment.as_deref(), fragment_made_local) {
        (Some(fragment), Some(fragment_made_local)) => {
            Some(StreamGraphOperation::RefreshFragment {
                node: node.id,
                from: StreamGraphFragmentPlacement {
                    generator: node.node_type.clone(),
                    config: node.config.clone(),
                    fragment: fragment.clone(),
                },
                to: StreamGraphFragmentPlacement {
                    generator: node.node_type.clone(),
                    config: config_made_local.unwrap_or_else(|| node.config.clone()),
                    fragment: fragment_made_local,
                },
            })
        }
        _ => config_made_local.map(|to| StreamGraphOperation::SetNodeConfig {
            node: node.id,
            from: node.config.clone(),
            to,
        }),
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

fn value_references_an_asset_of_the_dependency(value: &Value, project_text: &str) -> bool {
    match value {
        Value::Object(object) => {
            (object.contains_key("asset")
                && object.get("from").and_then(Value::as_str) == Some(project_text))
                || object
                    .values()
                    .any(|entry| value_references_an_asset_of_the_dependency(entry, project_text))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| value_references_an_asset_of_the_dependency(item, project_text)),
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

/// Refuse a `source` naming a project directory by a path relative to this process's working
/// directory, which a later update would resolve somewhere else.
fn refuse_a_project_directory_source_named_by_a_relative_path(
    source: &str,
) -> Result<(), ProjectRepositoryRefusal> {
    let source_path = Path::new(source);
    if !source_path.is_absolute() && source_path.join(PROJECT_FILE_NAME).is_file() {
        return Err(
            ProjectRepositoryRefusal::DependencySourceNotAnAbsolutePath {
                dependency_source: source.to_owned(),
            },
        );
    }
    Ok(())
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
                .sort_by_key(|dependency| dependency.project);
        }
        ProjectDependencyOperation::RemoveDependency { dependency } => {
            let held = project_file
                .dependencies
                .iter()
                .find(|held| held.project == dependency.project)
                .ok_or(ProjectRepositoryRefusal::NoSuchDependency(
                    dependency.project,
                ))?;
            if held.revision != dependency.revision {
                return Err(ProjectRepositoryRefusal::DependencyPinnedElsewhere {
                    project: dependency.project,
                    pinned: held.revision.clone(),
                    expected: dependency.revision.clone(),
                });
            }
            project_file
                .dependencies
                .retain(|held| held.project != dependency.project);
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
