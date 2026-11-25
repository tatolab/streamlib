// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::project_views::place_each_own_view_of_a_copy_by_its_type_in_the_copy;
use streamlib_runtime_client_contract::process_umask_kept_from_iceoryx2_listener_binds::create_directory_and_its_missing_parents_at_the_process_umask;

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
        create_directory_and_its_missing_parents_at_the_process_umask(project_directory)?;
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

    pub(super) fn write_a_new_project(
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
            core_project_directory_the_runtime_carries: None,
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

    pub(super) fn commit_the_written_project_as_its_first(
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
        Self::open_with_its_history_in(
            project_directory,
            &project_directory.join(PROJECT_HISTORY_DIRECTORY),
        )
    }

    /// Open the project in `project_directory` as [`Self::open`] does, its history kept in
    /// `history_directory` rather than in the project, so opening writes nothing into it.
    pub fn open_with_its_history_in(
        project_directory: &Path,
        history_directory: &Path,
    ) -> Result<Self, ProjectRepositoryRefusal> {
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
        let history = ProjectHistoryRepository::open_in(history_directory)?;
        let commit_of_each_idempotency_key = the_commit_of_each_idempotency_key_in(&history)?;
        Ok(Self {
            project_directory: project_directory.to_path_buf(),
            history,
            project_file,
            assets_by_uuid,
            python_requirements_lock: None,
            core_project_directory_the_runtime_carries: None,
            asset_files_waiting_for_their_import: BTreeMap::new(),
            commit_of_each_idempotency_key,
        })
    }

    /// Duplicate the project into `into`, a path that must not exist yet, as a project of its
    /// own: every file a copy of it carries and its whole history, under a new UUID and the name
    /// `<name> copy`, each of its streams placing its own views by that UUID, the duplicate
    /// committed on that history by `author`. Its `.venv` and the
    /// rest of its runtime folder stay behind. Refused, nothing left at `into`, for a relative
    /// `into`, one that exists, and one inside this project.
    pub fn duplicate_into(
        &self,
        into: &Path,
        author: &ProjectTransactionAuthor,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        refuse_a_relative_project_directory(into)?;
        refuse_an_author_without_a_name(author)?;
        if into.symlink_metadata().is_ok() {
            return Err(ProjectRepositoryRefusal::DuplicateIntoAnExistingPath {
                into: into.to_path_buf(),
            });
        }
        let this_project_resolved = std::fs::canonicalize(&self.project_directory)?;
        if the_nearest_existing_ancestor_resolved(into)?.starts_with(&this_project_resolved) {
            return Err(ProjectRepositoryRefusal::DuplicateIntoTheProjectItself {
                into: into.to_path_buf(),
            });
        }
        let duplicated = self.write_the_duplicate_into(into, author);
        if duplicated.is_err() {
            let _ = std::fs::remove_dir_all(into);
        }
        duplicated
    }

    pub(super) fn write_the_duplicate_into(
        &self,
        into: &Path,
        author: &ProjectTransactionAuthor,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        copy_the_files_a_project_copy_carries(&self.project_directory, into, |not_copied| {
            ProjectRepositoryRefusal::Io(not_copied)
        })?;
        copy_a_folder_whole(
            self.history.history_directory(),
            &into.join(PROJECT_HISTORY_DIRECTORY),
        )?;
        let duplicate_project_file = TatolabProjectFile {
            project: Uuid::new_v4(),
            name: format!("{} copy", self.project_file.name),
            ..self.project_file.clone()
        };
        write_file_whole(
            &into.join(PROJECT_FILE_NAME),
            &duplicate_project_file.canonical_bytes()?,
        )?;
        place_each_own_view_of_a_copy_by_its_type_in_the_copy(
            into,
            self.project_file.project,
            &self.project_file.name,
            |_| true,
        )?;
        let duplicate = Self::open(into)?;
        duplicate.history.commit_tracked_files(
            &duplicate.tracked_files(&duplicate.assets_by_uuid)?,
            author,
            &format!(
                "duplicate_project {} from {}",
                duplicate_project_file.name, self.project_file.project
            ),
        )?;
        Ok(duplicate)
    }

    /// When the project last changed: the time of the newest commit on its history, an undo, a
    /// redo and its duplicate included and a run never, `None` before its first commit.
    pub fn last_edited_at_unix_seconds(&self) -> Result<Option<i64>, ProjectRepositoryRefusal> {
        self.history.head_committed_at_unix_seconds()
    }

    /// Let a read pass through the project's link to the core project when the link points at
    /// the directory `core_project_directory_the_runtime_carries` answers; a project never
    /// handed one reads through no link.
    pub fn read_the_core_project_the_runtime_carries_at(
        &mut self,
        core_project_directory_the_runtime_carries: CoreProjectDirectoryTheRuntimeCarries,
    ) {
        self.core_project_directory_the_runtime_carries =
            Some(core_project_directory_the_runtime_carries);
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
}
