// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use git2::{FileMode, ObjectType, Oid, Repository, Signature, Sort};

use crate::project_history_smart_http_transport::with_the_bearer_token_on_this_thread;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_transaction::ProjectTransactionAuthor;

const MAIN_BRANCH_REFSPEC: &str = "refs/heads/main:refs/heads/main";
const EVERY_BRANCH_REFSPEC: &str = "+refs/heads/*:refs/heads/*";
const EVERY_TAG_REFSPEC: &str = "+refs/tags/*:refs/tags/*";

/// A Git smart-HTTP remote a project's history is pushed to and fetched from, such as a
/// Cloudflare Artifacts repo: `https://<account>.artifacts.cloudflare.net/git/<namespace>/<repo>.git`.
#[derive(Clone)]
pub struct ProjectHistoryRemote {
    /// The remote's URL, carrying no credentials.
    pub url: String,
    /// The token sent as `Authorization: Bearer`; an Artifacts repo takes one scoped to it.
    pub bearer_token: Option<String>,
}

impl std::fmt::Debug for ProjectHistoryRemote {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectHistoryRemote")
            .field("url", &self.url)
            .field("bearer_token", &self.bearer_token.as_ref().map(|_| "set"))
            .finish()
    }
}

/// Where a project's history sits, relative to the project: a bare Git repository the runtime alone writes.
pub const PROJECT_HISTORY_DIRECTORY: &str = ".tatolab/history.git";

/// The branch every committed transaction lands on.
pub const PROJECT_HISTORY_BRANCH: &str = "main";

/// The commit-message trailer that carries a transaction's JSON, so history is the transaction log.
pub const PROJECT_TRANSACTION_COMMIT_TRAILER: &str = "Tatolab-Transaction";

/// One commit of a project's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectHistoryCommit {
    /// The commit's id, lowercase hex.
    pub commit: String,
    /// The commit's whole message.
    pub message: String,
    /// The author's name.
    pub author_name: String,
}

/// A project's history, a Git repository written through libgit2; the one place the crate touches Git.
pub struct ProjectHistoryRepository {
    history_directory: PathBuf,
    repository: Repository,
}

impl ProjectHistoryRepository {
    /// Make the history of a new project at `project_directory`.
    pub fn create_for_project(project_directory: &Path) -> Result<Self, ProjectRepositoryRefusal> {
        let history_directory = project_directory.join(PROJECT_HISTORY_DIRECTORY);
        let mut options = git2::RepositoryInitOptions::new();
        options
            .bare(true)
            .initial_head(PROJECT_HISTORY_BRANCH)
            .mkpath(true);
        let repository = Repository::init_opts(&history_directory, &options)?;
        Ok(Self {
            history_directory,
            repository,
        })
    }

    /// Open the history of the project at `project_directory`, making it when the project has none yet.
    pub fn open_for_project(project_directory: &Path) -> Result<Self, ProjectRepositoryRefusal> {
        let history_directory = project_directory.join(PROJECT_HISTORY_DIRECTORY);
        if !history_directory.exists() {
            return Self::create_for_project(project_directory);
        }
        let repository = Repository::open_bare(&history_directory)?;
        Ok(Self {
            history_directory,
            repository,
        })
    }

    /// Open the bare history at `history_directory`, making an empty one when none is there:
    /// the copy of another project's history a dependency is fetched into.
    pub fn open_or_create_at(history_directory: &Path) -> Result<Self, ProjectRepositoryRefusal> {
        let repository = if history_directory.exists() {
            Repository::open_bare(history_directory)?
        } else {
            let mut options = git2::RepositoryInitOptions::new();
            options
                .bare(true)
                .initial_head(PROJECT_HISTORY_BRANCH)
                .mkpath(true);
            Repository::init_opts(history_directory, &options)?
        };
        Ok(Self {
            history_directory: history_directory.to_path_buf(),
            repository,
        })
    }

    /// Fetch every branch and tag of `remote` into this history, each replacing the one of its
    /// name, and remove each branch and tag `remote` no longer holds; fetched commits are kept.
    pub fn fetch_every_branch_and_tag_from(
        &self,
        remote: &ProjectHistoryRemote,
    ) -> Result<(), ProjectRepositoryRefusal> {
        with_the_bearer_token_on_this_thread(remote.bearer_token.as_deref(), || {
            let mut anonymous_remote = self.repository.remote_anonymous(&remote.url)?;
            let mut fetch_options = git2::FetchOptions::new();
            fetch_options.prune(git2::FetchPrune::On);
            anonymous_remote.fetch(
                &[EVERY_BRANCH_REFSPEC, EVERY_TAG_REFSPEC],
                Some(&mut fetch_options),
                None,
            )
        })?;
        tracing::info!(remote = %remote.url, "fetched a dependency's history");
        Ok(())
    }

    /// The commit `revision` names in this history: a commit id, whole or abbreviated, a tag or
    /// a branch.
    pub fn commit_a_revision_names(
        &self,
        revision: &str,
    ) -> Result<String, ProjectRepositoryRefusal> {
        let named = self
            .repository
            .revparse_single(revision)
            .and_then(|object| object.peel_to_commit())
            .map_err(|_| ProjectRepositoryRefusal::NoSuchRevision {
                revision: revision.to_owned(),
                history: self.history_directory.clone(),
            })?;
        Ok(named.id().to_string())
    }

    /// The history's directory on disk.
    pub fn history_directory(&self) -> &Path {
        &self.history_directory
    }

    /// The commit `main` points at, `None` before the first commit.
    pub fn head_commit(&self) -> Result<Option<String>, ProjectRepositoryRefusal> {
        Ok(self.head_commit_oid()?.map(|oid| oid.to_string()))
    }

    /// Commit `tracked_files` — each a project-relative path and its bytes — as the whole tree on `main`.
    pub fn commit_tracked_files(
        &self,
        tracked_files: &BTreeMap<String, Vec<u8>>,
        author: &ProjectTransactionAuthor,
        message: &str,
    ) -> Result<String, ProjectRepositoryRefusal> {
        let tree_oid = self.write_tree(tracked_files)?;
        let tree = self.repository.find_tree(tree_oid)?;
        let signature = Signature::now(&author.name, &author.commit_email())?;
        let parent = match self.head_commit_oid()? {
            Some(parent_oid) => Some(self.repository.find_commit(parent_oid)?),
            None => None,
        };
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        let commit_oid = self.repository.commit(
            Some(&format!("refs/heads/{PROJECT_HISTORY_BRANCH}")),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )?;
        Ok(commit_oid.to_string())
    }

    /// Push `main` to `remote`, which must take it as a fast-forward.
    pub fn push_main_to(
        &self,
        remote: &ProjectHistoryRemote,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let mut refused_reference_updates = Vec::new();
        let pushed = with_the_bearer_token_on_this_thread(remote.bearer_token.as_deref(), || {
            let mut anonymous_remote = self.repository.remote_anonymous(&remote.url)?;
            let mut callbacks = git2::RemoteCallbacks::new();
            callbacks.push_update_reference(|reference, refused_because| {
                if let Some(refused_because) = refused_because {
                    refused_reference_updates.push(format!("{reference}: {refused_because}"));
                }
                Ok(())
            });
            let mut push_options = git2::PushOptions::new();
            push_options.remote_callbacks(callbacks);
            anonymous_remote.push(&[MAIN_BRANCH_REFSPEC], Some(&mut push_options))
        });
        pushed?;
        if !refused_reference_updates.is_empty() {
            return Err(ProjectRepositoryRefusal::HistoryRemoteRefused {
                url: remote.url.clone(),
                why: refused_reference_updates.join("; "),
            });
        }
        tracing::info!(remote = %remote.url, "pushed the project's history");
        Ok(())
    }

    /// Fetch `remote`'s `main` into this history's `main` as a fast-forward, answering the
    /// commit `main` then holds.
    pub fn fetch_main_from(
        &self,
        remote: &ProjectHistoryRemote,
    ) -> Result<Option<String>, ProjectRepositoryRefusal> {
        with_the_bearer_token_on_this_thread(remote.bearer_token.as_deref(), || {
            let mut anonymous_remote = self.repository.remote_anonymous(&remote.url)?;
            anonymous_remote.fetch(&[MAIN_BRANCH_REFSPEC], None, None)
        })?;
        tracing::info!(remote = %remote.url, "fetched the project's history");
        self.head_commit()
    }

    /// Every commit on `main`, newest first.
    pub fn commits_newest_first(
        &self,
    ) -> Result<Vec<ProjectHistoryCommit>, ProjectRepositoryRefusal> {
        let Some(head_oid) = self.head_commit_oid()? else {
            return Ok(Vec::new());
        };
        let mut walk = self.repository.revwalk()?;
        walk.set_sorting(Sort::TOPOLOGICAL)?;
        walk.push(head_oid)?;
        let mut commits = Vec::new();
        for oid in walk {
            let commit = self.repository.find_commit(oid?)?;
            commits.push(ProjectHistoryCommit {
                commit: commit.id().to_string(),
                message: commit.message().unwrap_or_default().to_owned(),
                author_name: commit.author().name().unwrap_or_default().to_owned(),
            });
        }
        Ok(commits)
    }

    /// Every file `main`'s newest commit holds, by project-relative path.
    pub fn files_at_head(&self) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        match self.head_commit_oid()? {
            None => Ok(BTreeMap::new()),
            Some(head_oid) => self.files_at_commit_oid(head_oid),
        }
    }

    /// Every file the commit `commit` holds, by project-relative path.
    pub fn files_at_commit(
        &self,
        commit: &str,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        self.files_at_commit_oid(Oid::from_str(commit)?)
    }

    fn files_at_commit_oid(
        &self,
        commit_oid: Oid,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        let mut files = BTreeMap::new();
        let tree = self.repository.find_commit(commit_oid)?.tree()?;
        let mut walk_error = None;
        tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
            if entry.kind() != Some(ObjectType::Blob) {
                return git2::TreeWalkResult::Ok;
            }
            match self.repository.find_blob(entry.id()) {
                Ok(blob) => {
                    files.insert(
                        format!("{directory}{}", entry.name().unwrap_or_default()),
                        blob.content().to_vec(),
                    );
                    git2::TreeWalkResult::Ok
                }
                Err(error) => {
                    walk_error = Some(error);
                    git2::TreeWalkResult::Abort
                }
            }
        })?;
        match walk_error {
            Some(error) => Err(error.into()),
            None => Ok(files),
        }
    }

    fn head_commit_oid(&self) -> Result<Option<Oid>, ProjectRepositoryRefusal> {
        match self
            .repository
            .find_reference(&format!("refs/heads/{PROJECT_HISTORY_BRANCH}"))
        {
            Ok(reference) => Ok(reference.target()),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn write_tree(
        &self,
        tracked_files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Oid, ProjectRepositoryRefusal> {
        let mut root = TrackedDirectory::default();
        for (path, bytes) in tracked_files {
            root.insert(path.split('/').collect::<Vec<_>>().as_slice(), bytes)?;
        }
        root.write_into(&self.repository)
    }
}

#[derive(Default)]
struct TrackedDirectory<'bytes> {
    files: BTreeMap<String, &'bytes [u8]>,
    directories: BTreeMap<String, TrackedDirectory<'bytes>>,
}

impl<'bytes> TrackedDirectory<'bytes> {
    fn insert(
        &mut self,
        segments: &[&str],
        bytes: &'bytes [u8],
    ) -> Result<(), ProjectRepositoryRefusal> {
        let conflict = |name: &str| ProjectRepositoryRefusal::TrackedFileAndFolderShareAName {
            name: name.to_owned(),
        };
        match segments {
            [] => Ok(()),
            [file_name] => {
                if self.directories.contains_key(*file_name) {
                    return Err(conflict(file_name));
                }
                self.files.insert((*file_name).to_owned(), bytes);
                Ok(())
            }
            [directory, rest @ ..] => {
                if self.files.contains_key(*directory) {
                    return Err(conflict(directory));
                }
                self.directories
                    .entry((*directory).to_owned())
                    .or_default()
                    .insert(rest, bytes)
            }
        }
    }

    fn write_into(&self, repository: &Repository) -> Result<Oid, ProjectRepositoryRefusal> {
        let mut builder = repository.treebuilder(None)?;
        for (file_name, bytes) in &self.files {
            let blob_oid = repository.blob(bytes)?;
            builder.insert(file_name, blob_oid, FileMode::Blob.into())?;
        }
        for (directory_name, directory) in &self.directories {
            let tree_oid = directory.write_into(repository)?;
            builder.insert(directory_name, tree_oid, FileMode::Tree.into())?;
        }
        Ok(builder.write()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_transaction::ProjectTransactionAuthorKind;

    fn agent() -> ProjectTransactionAuthor {
        ProjectTransactionAuthor {
            kind: ProjectTransactionAuthorKind::Agent,
            name: "builder".to_owned(),
        }
    }

    #[test]
    fn each_commit_holds_exactly_the_tracked_files_and_chains_onto_the_last() {
        let project_directory = tempfile::tempdir().unwrap();
        let history =
            ProjectHistoryRepository::create_for_project(project_directory.path()).unwrap();
        assert_eq!(history.head_commit().unwrap(), None);

        let mut tracked_files = BTreeMap::new();
        tracked_files.insert("tatolab-project.json".to_owned(), b"{}\n".to_vec());
        tracked_files.insert(
            "media/a/intro.mp4.asset.json".to_owned(),
            b"{\"a\":1}\n".to_vec(),
        );
        let first = history
            .commit_tracked_files(&tracked_files, &agent(), "first")
            .unwrap();
        tracked_files.remove("media/a/intro.mp4.asset.json");
        let second = history
            .commit_tracked_files(&tracked_files, &agent(), "second")
            .unwrap();

        assert_eq!(history.head_commit().unwrap(), Some(second.clone()));
        let commits = history.commits_newest_first().unwrap();
        assert_eq!(
            commits
                .iter()
                .map(|commit| commit.commit.clone())
                .collect::<Vec<_>>(),
            vec![second, first]
        );
        assert_eq!(commits[0].author_name, "builder");
        assert_eq!(history.files_at_head().unwrap(), tracked_files);
    }

    #[test]
    fn a_fetch_removes_each_branch_the_source_deleted_and_keeps_its_commits() {
        let source_project = tempfile::tempdir().unwrap();
        let source = ProjectHistoryRepository::create_for_project(source_project.path()).unwrap();
        let mut tracked_files = BTreeMap::new();
        tracked_files.insert("tatolab-project.json".to_owned(), b"{}\n".to_vec());
        let commit = source
            .commit_tracked_files(&tracked_files, &agent(), "first")
            .unwrap();
        let source_repository = Repository::open(source.history_directory()).unwrap();
        source_repository
            .branch(
                "feature",
                &source_repository
                    .find_commit(Oid::from_str(&commit).unwrap())
                    .unwrap(),
                false,
            )
            .unwrap();
        let fetched_into = tempfile::tempdir().unwrap();
        let fetched =
            ProjectHistoryRepository::open_or_create_at(&fetched_into.path().join("history.git"))
                .unwrap();
        let remote = ProjectHistoryRemote {
            url: source.history_directory().display().to_string(),
            bearer_token: None,
        };
        fetched.fetch_every_branch_and_tag_from(&remote).unwrap();
        assert_eq!(fetched.commit_a_revision_names("feature").unwrap(), commit);

        source_repository
            .find_branch("feature", git2::BranchType::Local)
            .unwrap()
            .delete()
            .unwrap();
        fetched.fetch_every_branch_and_tag_from(&remote).unwrap();

        assert!(fetched.commit_a_revision_names("feature").is_err());
        assert_eq!(fetched.commit_a_revision_names(&commit).unwrap(), commit);
    }

    #[test]
    fn a_file_and_a_folder_of_the_same_name_are_refused_either_way_round() {
        let project_directory = tempfile::tempdir().unwrap();
        let history =
            ProjectHistoryRepository::create_for_project(project_directory.path()).unwrap();
        for second_path in ["media", "media/intro.mp4/inner"] {
            let mut tracked_files = BTreeMap::new();
            tracked_files.insert("media/intro.mp4".to_owned(), b"a".to_vec());
            tracked_files.insert(second_path.to_owned(), b"b".to_vec());
            assert!(matches!(
                history.commit_tracked_files(&tracked_files, &agent(), "conflict"),
                Err(ProjectRepositoryRefusal::TrackedFileAndFolderShareAName { .. })
            ));
        }
        assert_eq!(history.head_commit().unwrap(), None);
    }
}
