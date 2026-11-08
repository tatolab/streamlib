// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_asset::{
    ASSET_SIDECAR_FILE_SUFFIX, ProjectAsset, ProjectAssetKind, ProjectAssetSidecar,
    ProjectRelativeAssetPath,
};
use crate::project_content_store::ProjectContentStore;
use crate::project_file::{PROJECT_FILE_NAME, TatolabProjectFile};
use crate::project_history_repository::{
    PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectHistoryRepository,
};
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_transaction::{
    AppliedProjectTransaction, ProjectAssetChangeRequest, ProjectAssetImportSource,
    ProjectAssetOperation, ProjectTransaction, ProjectTransactionAuthor,
};

/// What ends a stream asset's graph document: `streams/main.graph.json`.
pub const GRAPH_DOCUMENT_FILE_SUFFIX: &str = ".graph.json";

const DIRECTORIES_A_PROJECT_SCAN_SKIPS: &[&str] = &["node_modules", "__pycache__"];

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
}

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

    /// Every asset, ordered by UUID.
    pub fn assets(&self) -> impl Iterator<Item = &ProjectAsset> {
        self.assets_by_uuid.values()
    }

    /// The asset `asset` names, or a refusal naming it.
    pub fn asset(&self, asset: Uuid) -> Result<&ProjectAsset, ProjectRepositoryRefusal> {
        self.assets_by_uuid
            .get(&asset)
            .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))
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
        if changes.is_empty() {
            return Err(ProjectRepositoryRefusal::EmptyTransaction);
        }
        let mut staged_assets = self.assets_by_uuid.clone();
        let mut operations = Vec::with_capacity(changes.len());
        for change in changes {
            if let Some(operation) = self.operation_for_change(&staged_assets, change)? {
                apply_operation_to_assets(&mut staged_assets, &operation);
                operations.push(operation);
            }
        }
        if operations.is_empty() {
            return Err(ProjectRepositoryRefusal::EmptyTransaction);
        }
        let transaction = ProjectTransaction {
            transaction: Uuid::new_v4(),
            author: author.clone(),
            parent_commit: self.history.head_commit()?,
            inverse_operations: operations
                .iter()
                .rev()
                .map(ProjectAssetOperation::inverse)
                .collect(),
            operations,
        };

        let mut performed_effects = Vec::with_capacity(transaction.operations.len());
        for operation in &transaction.operations {
            match self.perform_effect(operation) {
                Ok(performed) => performed_effects.push(performed),
                Err(refusal) => {
                    self.undo_performed_effects(performed_effects);
                    return Err(refusal);
                }
            }
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
                self.undo_performed_effects(performed_effects);
                return Err(refusal);
            }
        };
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
                let kind = kind
                    .unwrap_or_else(|| ProjectAssetKind::inferred_from_file_name(path.file_name()));
                if kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetsAreNotImported);
                }
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
                let referencing_documents = self.graph_documents_referencing(asset)?;
                if !referencing_documents.is_empty() {
                    return Err(ProjectRepositoryRefusal::AssetStillReferenced {
                        asset,
                        referencing_documents,
                    });
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
                std::fs::remove_file(self.sidecar_on_disk(&asset.path))?;
                let file = asset.path.on_disk_under(&self.project_directory);
                if file.exists() {
                    std::fs::remove_file(file)?;
                }
                Ok(PerformedAssetEffect::Operation(operation.clone()))
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
                    std::fs::remove_file(self.sidecar_on_disk(&asset.path)).map_err(Into::into)
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

    fn write_sidecar(&self, asset: &ProjectAsset) -> Result<(), ProjectRepositoryRefusal> {
        write_file_whole(&self.sidecar_on_disk(&asset.path), &asset.sidecar_bytes()?)
    }

    fn sidecar_on_disk(&self, path: &ProjectRelativeAssetPath) -> PathBuf {
        self.project_directory.join(path.sidecar_path())
    }

    fn tracked_files(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        let mut tracked_files = BTreeMap::new();
        tracked_files.insert(
            PROJECT_FILE_NAME.to_owned(),
            self.project_file.canonical_bytes()?,
        );
        for asset in assets.values() {
            tracked_files.insert(asset.path.sidecar_path(), asset.sidecar_bytes()?);
        }
        for graph_document in
            project_files_ending_with(&self.project_directory, GRAPH_DOCUMENT_FILE_SUFFIX)?
        {
            let bytes = std::fs::read(self.project_directory.join(&graph_document))?;
            tracked_files.insert(graph_document, bytes);
        }
        Ok(tracked_files)
    }

    fn graph_documents_referencing(
        &self,
        asset: Uuid,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        let asset_text = asset.to_string();
        let mut referencing = Vec::new();
        for graph_document in
            project_files_ending_with(&self.project_directory, GRAPH_DOCUMENT_FILE_SUFFIX)?
        {
            let bytes = std::fs::read(self.project_directory.join(&graph_document))?;
            let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
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
        .map(ProjectAssetOperation::summary)
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

fn refuse_a_path_another_asset_holds(
    assets: &BTreeMap<Uuid, ProjectAsset>,
    path: &ProjectRelativeAssetPath,
    except: Option<Uuid>,
) -> Result<(), ProjectRepositoryRefusal> {
    match assets
        .values()
        .find(|asset| &asset.path == path && Some(asset.asset) != except)
    {
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

fn read_every_asset_sidecar(
    project_directory: &Path,
) -> Result<BTreeMap<Uuid, ProjectAsset>, ProjectRepositoryRefusal> {
    let mut assets_by_uuid: BTreeMap<Uuid, ProjectAsset> = BTreeMap::new();
    for sidecar_path in project_files_ending_with(project_directory, ASSET_SIDECAR_FILE_SUFFIX)? {
        let on_disk = project_directory.join(&sidecar_path);
        let unreadable = |why: String| ProjectRepositoryRefusal::AssetSidecarUnreadable {
            path: on_disk.clone(),
            why,
        };
        let asset_path = ProjectRelativeAssetPath::parse(
            sidecar_path
                .strip_suffix(ASSET_SIDECAR_FILE_SUFFIX)
                .unwrap_or(&sidecar_path),
        )
        .map_err(|refusal| unreadable(refusal.to_string()))?;
        let bytes = std::fs::read(&on_disk)?;
        let sidecar: ProjectAssetSidecar =
            serde_json::from_slice(&bytes).map_err(|error| unreadable(error.to_string()))?;
        if let Some(earlier) = assets_by_uuid.get(&sidecar.asset) {
            return Err(ProjectRepositoryRefusal::AssetNamedByTwoSidecars {
                asset: sidecar.asset,
                first: earlier.path.sidecar_path(),
                second: sidecar_path,
            });
        }
        assets_by_uuid.insert(
            sidecar.asset,
            ProjectAsset::from_sidecar(asset_path, sidecar),
        );
    }
    Ok(assets_by_uuid)
}

fn project_files_ending_with(
    project_directory: &Path,
    suffix: &str,
) -> Result<Vec<String>, ProjectRepositoryRefusal> {
    let mut found = Vec::new();
    let mut directories_to_read = vec![(project_directory.to_path_buf(), String::new())];
    while let Some((directory, relative_directory)) = directories_to_read.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let relative = if relative_directory.is_empty() {
                name.clone()
            } else {
                format!("{relative_directory}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&name.as_str()) {
                    directories_to_read.push((entry.path(), relative));
                }
            } else if file_type.is_file() && name.ends_with(suffix) {
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn write_file_whole(path: &Path, bytes: &[u8]) -> Result<(), ProjectRepositoryRefusal> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension(format!("partial-{}", Uuid::new_v4()));
    std::fs::write(&partial, bytes)?;
    if let Err(error) = std::fs::rename(&partial, path) {
        let _ = std::fs::remove_file(&partial);
        return Err(error.into());
    }
    Ok(())
}

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
