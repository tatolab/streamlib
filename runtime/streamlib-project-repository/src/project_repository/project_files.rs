// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl ProjectRepository {
    /// Write each staged graph document at the path its stream asset holds once the
    /// transaction applies, answering what each held before so a refusal can restore it.
    /// Every file of the project under `under`, or the whole project, as a listing shows it.
    pub fn list_project_files(
        &self,
        under: Option<&str>,
    ) -> Result<Vec<ProjectFileListed>, ProjectRepositoryRefusal> {
        let under = under.map(ProjectRelativeFilePath::parse).transpose()?;
        project_files_a_listing_shows(&self.project_directory, under.as_ref())
    }

    /// The bytes of the project's file at `path`, as they are on disk now.
    pub fn read_project_file(&self, path: &str) -> Result<Vec<u8>, ProjectRepositoryRefusal> {
        let path = ProjectRelativeFilePath::parse(path)?;
        let core_project_directory_the_runtime_carries = self
            .core_project_directory_the_runtime_carries
            .as_ref()
            .and_then(|where_it_is| where_it_is());
        path.refuse_a_symbolic_link_on_the_way_to_a_read_under(
            &self.project_directory,
            core_project_directory_the_runtime_carries.as_deref(),
        )?;
        let on_disk = path.on_disk_under(&self.project_directory);
        if !on_disk.is_file() {
            return Err(ProjectRepositoryRefusal::NoSuchProjectFile(
                path.to_string(),
            ));
        }
        Ok(std::fs::read(on_disk)?)
    }

    /// Write `content` to the project's file at `path` by `author` as one transaction, whose
    /// inverse puts back what the file held or takes a new file out; refused, the file left as
    /// it is, when `sha256_of_the_content_last_read` names other bytes than the file holds now.
    /// Sent again under the same `idempotency_key`, it answers the first transaction.
    pub fn write_project_file(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        path: &str,
        content: &[u8],
        sha256_of_the_content_last_read: Option<&str>,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let path = ProjectRelativeFilePath::parse(path)?;
        self.refuse_a_file_another_action_writes(&self.every_asset()?, &path)?;
        let on_disk = path.on_disk_under(&self.project_directory);
        let replaced_a_file = on_disk.is_file();
        let bytes_on_disk_now = if replaced_a_file {
            Some(std::fs::read(&on_disk)?)
        } else {
            None
        };
        if let Some(sha256_of_the_content_last_read) = sha256_of_the_content_last_read {
            refuse_a_file_changed_since_it_was_read(
                &path,
                sha256_of_the_content_last_read,
                bytes_on_disk_now.as_deref(),
            )?;
        }
        if bytes_on_disk_now.as_deref() == Some(content) {
            return Err(
                ProjectRepositoryRefusal::ProjectFileAlreadyHoldsThisContent(path.to_string()),
            );
        }
        self.record_the_file_when_history_holds_other_bytes(&path, bytes_on_disk_now.as_deref())?;
        let staged_at = format!("{PROJECT_FILE_WRITE_STAGING_DIRECTORY}/{}", Uuid::new_v4());
        let staged_copy = self.project_directory.join(&staged_at);
        write_file_whole(&staged_copy, content)?;
        let applied = self.apply_and_commit_project_operations(
            author,
            idempotency_key,
            Vec::new(),
            vec![ProjectOperation::File(
                ProjectFileOperation::WriteProjectFile {
                    path,
                    content: ProjectFileContent::StagedCopy { staged_at },
                    replaced_a_file,
                },
            )],
            ProjectTransactionLineage::NewEdit,
        );
        if staged_copy.exists() {
            let _ = std::fs::remove_file(&staged_copy);
        }
        applied.map(ProjectActionApplyOutcome::AppliedNow)
    }

    /// Move each node of a `tatolab-ops` type in the project's stream documents to the Core Nodes
    /// type that replaced it, as one transaction by the runtime under
    /// [`TATOLAB_OPS_NODES_MOVED_TO_CORE_NODES_KEY`]. `None` when no document holds such a node,
    /// or history already holds the move, undone since or not, so an undo of it holds.
    pub fn move_each_tatolab_ops_node_to_core_nodes(
        &mut self,
    ) -> Result<Option<ProjectActionApplyOutcome>, ProjectRepositoryRefusal> {
        let move_key =
            ProjectActionIdempotencyKey::from_caller(TATOLAB_OPS_NODES_MOVED_TO_CORE_NODES_KEY)?;
        if self.commit_of_each_idempotency_key.contains_key(&move_key) {
            return Ok(None);
        }
        let assets = self.every_asset()?;
        let mut operations = Vec::new();
        for asset in assets.values() {
            if asset.kind != ProjectAssetKind::Stream {
                continue;
            }
            let Ok((_, document)) = self.read_stream_graph_document(&assets, asset.asset) else {
                continue;
            };
            operations.extend(operations_moving_each_tatolab_ops_node_to_core_nodes(
                asset.asset,
                &document,
            ));
        }
        if operations.is_empty() {
            return Ok(None);
        }
        self.apply_project_operations(
            &ProjectTransactionAuthor::the_runtime(),
            Some(&move_key),
            Vec::new(),
            operations,
            ProjectTransactionLineage::NewEdit,
        )
        .map(Some)
    }

    /// Write a new file of the project as one transaction, as [`Self::write_project_file`]
    /// does, refused when anything is already at `path`.
    pub fn create_project_file(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        path: &str,
        content: &[u8],
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        refuse_an_author_without_a_name(author)?;
        if let Some(applied_before) =
            self.the_transaction_applied_under_a_key_the_caller_sent(idempotency_key)?
        {
            return Ok(ProjectActionApplyOutcome::AppliedBeforeUnderItsKey(
                applied_before,
            ));
        }
        let parsed_path = ProjectRelativeFilePath::parse(path)?;
        let on_disk = parsed_path.on_disk_under(&self.project_directory);
        if on_disk.symlink_metadata().is_ok() {
            return Err(ProjectRepositoryRefusal::ProjectFileAlreadyExists(
                parsed_path.as_str().to_owned(),
            ));
        }
        self.write_project_file(author, idempotency_key, path, content, None)
    }

    /// Commit the project's files as they are now when history holds other bytes at `path`
    /// than `bytes_on_disk_now`, so a write's inverse puts back what the write replaced.
    pub(super) fn record_the_file_when_history_holds_other_bytes(
        &self,
        path: &ProjectRelativeFilePath,
        bytes_on_disk_now: Option<&[u8]>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let head = self.commit_the_files_when_history_holds_none()?;
        let bytes_history_holds = self.history.files_at_commit(&head)?.remove(path.as_str());
        if bytes_history_holds.as_deref() == bytes_on_disk_now {
            return Ok(());
        }
        self.history.commit_tracked_files(
            &self.tracked_files(&self.assets_by_uuid)?,
            &ProjectTransactionAuthor::the_runtime(),
            &format!("record {path} as it was before write_project_file"),
        )?;
        Ok(())
    }

    /// The files an undo or a redo with file operations expects on disk: what the undone
    /// transaction left, or what stood before the redone one; `None` for a new edit.
    pub(super) fn files_a_reversal_finds_on_disk(
        &self,
        lineage: ProjectTransactionLineage,
        operations: &[ProjectOperation],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, ProjectRepositoryRefusal> {
        let reversed = match lineage {
            ProjectTransactionLineage::NewEdit => return Ok(None),
            ProjectTransactionLineage::UndoOf(reversed)
            | ProjectTransactionLineage::RedoOf(reversed) => reversed,
        };
        if !operations
            .iter()
            .any(|operation| matches!(operation, ProjectOperation::File(_)))
        {
            return Ok(None);
        }
        let Some(applied) = self
            .each_applied_transaction_newest_first()?
            .find(|applied| {
                applied
                    .as_ref()
                    .map_or(true, |applied| applied.transaction.transaction == reversed)
            })
            .transpose()?
        else {
            return Ok(None);
        };
        let commit = match lineage {
            ProjectTransactionLineage::UndoOf(_) => Some(applied.commit),
            _ => applied.transaction.parent_commit,
        };
        commit
            .map(|commit| self.history.files_at_commit(&commit))
            .transpose()
    }

    /// Refuse a path the file verbs never write: what another action changes, or a file the
    /// project's history does not record, whose write could not be undone. Paths compare
    /// without regard to case, as macOS's default filesystem resolves them.
    pub(super) fn refuse_a_file_another_action_writes(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        path: &ProjectRelativeFilePath,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let refuse =
            |why: &'static str| ProjectRepositoryRefusal::ProjectFileWrittenByAnotherAction {
                path: path.to_string(),
                why,
            };
        let spelled_in_lowercase = path.as_str().to_lowercase();
        let spelled = spelled_in_lowercase.as_str();
        if spelled.ends_with(GRAPH_DOCUMENT_FILE_SUFFIX) {
            return Err(refuse(
                "a graph document is changed by graph edits and the stream verbs",
            ));
        }
        if spelled.ends_with(ASSET_SIDECAR_FILE_SUFFIX) {
            return Err(refuse("an asset's sidecar is changed by the asset verbs"));
        }
        if spelled == PROJECT_FILE_NAME {
            return Err(refuse(
                "the project file is changed by the project and dependency verbs",
            ));
        }
        if spelled.split('/').next() == Some(PROJECT_IMPORTS_DIRECTORY) {
            return Err(refuse(
                "an import's files are edited by opening `imports/<folder>/` as a project of its own",
            ));
        }
        if assets.values().any(|asset| {
            asset.kind != ProjectAssetKind::Stream && asset.path.as_str().to_lowercase() == spelled
        }) {
            return Err(refuse("an asset's bytes are replaced with replace_asset"));
        }
        let mut folder = self.project_directory.clone();
        for segment in path.folders() {
            folder.push(segment);
            if segment.starts_with('.')
                || DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&segment)
                || folder.join("pyvenv.cfg").exists()
            {
                return Err(refuse(
                    "it is inside a folder the project's history does not record, so its write \
                     could not be undone",
                ));
            }
        }
        path.refuse_a_symbolic_link_on_the_way_under(&self.project_directory)
    }

    /// Refuse `file_operation` unless the file is as it expects: there for a write over it or
    /// a removal, absent for a new file, and for an undo or a redo holding the bytes
    /// `files_a_reversal_finds_on_disk` records.
    pub(super) fn refuse_a_file_operation_on_a_file_as_it_is_now(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        file_operation: &ProjectFileOperation,
        files_a_reversal_finds_on_disk: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let path = file_operation.path();
        self.refuse_a_file_another_action_writes(assets, path)?;
        let on_disk = path.on_disk_under(&self.project_directory);
        let is_there = on_disk.is_file();
        if let Some(files_a_reversal_finds_on_disk) = files_a_reversal_finds_on_disk {
            let bytes_on_disk_now = if is_there {
                Some(std::fs::read(&on_disk)?)
            } else {
                None
            };
            if files_a_reversal_finds_on_disk.get(path.as_str()) != bytes_on_disk_now.as_ref() {
                return Err(
                    ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRecorded {
                        path: path.to_string(),
                    },
                );
            }
        }
        let expected_there = match file_operation {
            ProjectFileOperation::WriteProjectFile {
                replaced_a_file, ..
            } => *replaced_a_file,
            ProjectFileOperation::RemoveProjectFile { .. } => true,
        };
        if is_there != expected_there {
            return Err(
                ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRecorded {
                    path: path.to_string(),
                },
            );
        }
        Ok(())
    }

    /// Write or take out each file `operations` names, answering what each held so a refusal
    /// can restore it.
    pub(super) fn perform_file_operations(
        &self,
        operations: &[ProjectOperation],
    ) -> Result<Vec<ProjectFileAsItWasBeforeAWrite>, ProjectRepositoryRefusal> {
        let mut written_files = Vec::new();
        for operation in operations {
            let ProjectOperation::File(file_operation) = operation else {
                continue;
            };
            let on_disk = file_operation.path().on_disk_under(&self.project_directory);
            let bytes_before = std::fs::read(&on_disk).ok();
            let performed = match file_operation {
                ProjectFileOperation::WriteProjectFile {
                    path,
                    content: ProjectFileContent::StagedCopy { staged_at },
                    ..
                } => std::fs::read(self.project_directory.join(staged_at))
                    .map_err(|_| ProjectRepositoryRefusal::NoSuchProjectFile(path.to_string()))
                    .and_then(|bytes| write_file_whole(&on_disk, &bytes)),
                ProjectFileOperation::WriteProjectFile {
                    path,
                    content: ProjectFileContent::RecordedAtCommit { commit },
                    ..
                } => self.history.files_at_commit(commit).and_then(|mut files| {
                    let bytes = files.remove(path.as_str()).ok_or_else(|| {
                        ProjectRepositoryRefusal::NoSuchProjectFile(path.to_string())
                    })?;
                    write_file_whole(&on_disk, &bytes)
                }),
                ProjectFileOperation::RemoveProjectFile { .. } => {
                    std::fs::remove_file(&on_disk).map_err(ProjectRepositoryRefusal::from)
                }
            };
            if let Err(refusal) = performed {
                self.restore_written_documents(written_files);
                return Err(refusal);
            }
            written_files.push((on_disk, bytes_before));
        }
        Ok(written_files)
    }
}
