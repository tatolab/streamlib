// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl ProjectRepository {
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
            Some(
                ProjectOperation::StreamGraph(_)
                | ProjectOperation::Import(_)
                | ProjectOperation::File(_)
                | ProjectOperation::ProjectSettings(_),
            )
            | None => Err(ProjectRepositoryRefusal::EmptyTransaction),
        }
    }

    pub(super) fn every_asset(
        &self,
    ) -> Result<BTreeMap<Uuid, ProjectAsset>, ProjectRepositoryRefusal> {
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

    pub(super) fn each_applied_transaction_newest_first(
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

    /// Apply the settings change `action` by `author` as one transaction and commit it; sent
    /// again under the same `idempotency_key`, it answers the first transaction.
    pub fn apply_project_settings_action(
        &mut self,
        author: &ProjectTransactionAuthor,
        idempotency_key: Option<&ProjectActionIdempotencyKey>,
        action: ProjectSettingsAction,
    ) -> Result<ProjectActionApplyOutcome, ProjectRepositoryRefusal> {
        let operation =
            ProjectOperation::ProjectSettings(action.operation_against(&self.project_file));
        self.apply_project_operations(
            author,
            idempotency_key,
            vec![ProjectAction::Settings(action)],
            vec![operation],
            ProjectTransactionLineage::NewEdit,
        )
    }

    pub(super) fn operations_for_changes(
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

    pub(super) fn read_stream_graph_document(
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

    pub(super) fn apply_and_commit_project_operations(
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
        let files_a_reversal_finds_on_disk =
            self.files_a_reversal_finds_on_disk(lineage, &operations)?;
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
                ProjectOperation::File(file_operation) => {
                    self.refuse_a_file_operation_on_a_file_as_it_is_now(
                        &staged_assets,
                        file_operation,
                        files_a_reversal_finds_on_disk.as_ref(),
                    )?;
                }
                ProjectOperation::ProjectSettings(project_settings_operation) => {
                    project_settings_operation.write_into(&mut staged_project_file);
                }
                ProjectOperation::Asset(asset_operation) => {
                    if let ProjectAssetOperation::DeleteAsset { asset } = asset_operation {
                        if staged_project_file.cover == Some(asset.asset) {
                            return Err(ProjectRepositoryRefusal::AssetIsTheProjectCover(
                                asset.asset,
                            ));
                        }
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
                    for (node, input_values) in graph_operation.input_values_it_sets() {
                        refuse_input_values_referencing_an_asset_the_project_does_not_hold(
                            &staged_assets,
                            node,
                            input_values,
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
        refuse_project_settings_the_project_cannot_hold(&staged_project_file, &staged_assets)?;
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
            })
            .and_then(|mut written_documents| {
                match self.perform_file_operations(&transaction.operations) {
                    Ok(written_files) => {
                        written_documents.extend(written_files);
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
}
