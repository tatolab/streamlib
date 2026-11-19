// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib_runtime_client_contract::process_umask_kept_from_iceoryx2_listener_binds::create_directory_and_its_missing_parents_at_the_process_umask;

impl ProjectRepository {
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

    /// Every import the project resolves: each one its project file records, then the core
    /// project the runtime links in.
    pub fn every_import_the_project_resolves(&self) -> Vec<ProjectImport> {
        every_import_a_project_resolves(&self.project_directory, &self.project_file)
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

    pub(super) fn version_the_source_holds_now(
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
    pub(super) fn refuse_removing_an_import_a_graph_references(
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
    pub(super) fn write_each_import_into_the_pyproject_if_they_changed(
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
            &self.every_import_the_project_resolves(),
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
    pub(super) fn lock_the_python_requirements_of_the_rewritten_pyproject(
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

    pub(super) fn restore_the_uv_lock(&self, uv_lock_as_it_was: Option<Vec<u8>>) {
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

    pub(super) fn restore_the_pyproject(&self, pyproject_as_it_was: &str) {
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

    pub(super) fn restore_the_project_file(&mut self, project_file_as_it_was: &TatolabProjectFile) {
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
    pub(super) fn perform_import_effect(
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
                    create_directory_and_its_missing_parents_at_the_process_umask(imports_folder)?;
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
    pub(super) fn set_aside_the_folder_of(
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
            create_directory_and_its_missing_parents_at_the_process_umask(staging)?;
        }
        std::fs::rename(&import_folder, &set_aside)?;
        Ok(Some(set_aside))
    }

    /// Write the files history recorded under `imports/<folder>/` at `commit` into a fresh
    /// staging folder, answering it. An asset's file history records by its sidecar alone is
    /// not among them, so it reads as missing until it is put back.
    pub(super) fn stage_the_import_folder_history_recorded_at(
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
            create_directory_and_its_missing_parents_at_the_process_umask(&staged_folder)?;
            for (path, bytes) in self.history.files_at_commit(commit)? {
                let Some(inside_the_import) = path.strip_prefix(&prefix) else {
                    continue;
                };
                let destination = staged_folder.join(inside_the_import);
                if let Some(parent) = destination.parent() {
                    create_directory_and_its_missing_parents_at_the_process_umask(parent)?;
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

    pub(super) fn undo_performed_import_effects(
        &self,
        performed_effects: Vec<PerformedImportEffect>,
    ) {
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

    pub(super) fn remove_what_import_effects_set_aside(
        &self,
        performed_effects: Vec<PerformedImportEffect>,
    ) {
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
    pub(super) fn commit_the_files_when_history_holds_none(
        &self,
    ) -> Result<String, ProjectRepositoryRefusal> {
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
}
