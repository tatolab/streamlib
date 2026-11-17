// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use crate::PROJECT_RUNTIME_DIRECTORY;
use crate::canonical_project_json::canonical_project_json_bytes;
use crate::core_project_import::every_import_a_project_resolves;
use crate::project_asset::{
    ASSET_SIDECAR_FILE_SUFFIX, DIRECTORIES_A_PROJECT_SCAN_SKIPS, GRAPH_DOCUMENT_FILE_SUFFIX,
    PROJECT_IMPORTS_DIRECTORY, ProjectAsset, ProjectAssetKind, ProjectAssetSidecar,
    ProjectRelativeAssetPath, project_files_ending_with, project_files_the_history_records,
    read_every_asset_sidecar,
};
use crate::project_asset_file_bytes::{
    AssetFileBytesRecorded, PROJECT_UPLOADS_DIRECTORY, content_sha256_of,
    copy_the_file_into_the_uploads_hashing_it, is_a_sha256_digest,
    the_bytes_recorded_of_the_file_at, the_upload_file_of,
};
use crate::project_file::{
    PROJECT_FILE_NAME, ProjectImport, THIS_RUNTIME_VERSION, TatolabProjectFile,
};
use crate::project_file_access::{
    ProjectFileListed, ProjectRelativeFilePath, project_files_a_listing_shows,
};
use crate::project_history_repository::{
    PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER, ProjectHistoryCommit,
    ProjectHistoryCommitWritten, ProjectHistoryRepository,
};
use crate::project_import_copy::{
    PROJECT_IMPORT_STAGING_DIRECTORY, copy_the_files_a_project_copy_carries,
    files_an_import_carries, files_edited_since_the_copy, refuse_an_import_folder_name,
    sha256_of_each_file_under, stage_a_copy_of_the_project_source, the_import_folder_in,
    the_import_folder_named_after, unpack_a_project_archive_as_a_new_project,
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
    ProjectAssetContent, ProjectAssetImportSource, ProjectAssetOperation, ProjectFileContent,
    ProjectFileOperation, ProjectImportFiles, ProjectImportOperation, ProjectOperation,
    ProjectSettingsAction, ProjectStreamDocumentOperation, ProjectTransaction,
    ProjectTransactionAsSentAndAnswered, ProjectTransactionAuthor,
    ProjectTransactionEnvelopeHeader, ProjectTransactionLineage, ProjectTransactionReversal,
    ProjectTransactionUndoState, StreamGraphOperationOnAStream,
};
use crate::stream_document_upgrade::recorded_transaction::upgrade_recorded_transaction_in_place;
use crate::stream_graph_document::{StreamGraphDocument, assets_a_nodes_input_values_reference};

mod asset_effects;
mod asset_operations;
mod assets_and_transactions;
mod creating_and_opening;
mod imports_and_dependencies;
mod project_files;
mod stream_documents;
mod undo_and_redo;

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

/// Where a file write's bytes wait, relative to the project, until its transaction moves them
/// into place.
const PROJECT_FILE_WRITE_STAGING_DIRECTORY: &str = ".tatolab/staged-file-writes";

/// What the host hands a project to lock the Python requirements of the project in a
/// directory with, refused in the resolver's own words when they cannot be resolved.
pub type ProjectPythonRequirementsLock = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// What the host hands a project to learn where the core project its runtime carries is, when
/// it carries one.
pub type CoreProjectDirectoryTheRuntimeCarries = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

/// A project the runtime opened: the one writer of its files, its imports and its history.
pub struct ProjectRepository {
    project_directory: PathBuf,
    project_file: TatolabProjectFile,
    assets_by_uuid: BTreeMap<Uuid, ProjectAsset>,
    history: ProjectHistoryRepository,
    python_requirements_lock: Option<ProjectPythonRequirementsLock>,
    core_project_directory_the_runtime_carries: Option<CoreProjectDirectoryTheRuntimeCarries>,
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

/// A file's path — a graph document's or another of the project's — and the bytes it held
/// before a transaction wrote it, `None` when it did not exist.
type ProjectFileAsItWasBeforeAWrite = (PathBuf, Option<Vec<u8>>);

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

/// The transaction a commit's trailer records, one of an older format read in the forms this
/// runtime records; one of a newer format is refused by name.
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
    let transaction: ProjectTransaction =
        if envelope.format_version < PROJECT_TRANSACTION_FORMAT_VERSION {
            let mut recorded: serde_json::Value = serde_json::from_str(transaction_json)?;
            upgrade_recorded_transaction_in_place(&mut recorded);
            serde_json::from_value(recorded)?
        } else {
            serde_json::from_str(transaction_json)?
        };
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

/// `path` resolved through its nearest ancestor that exists, the rest appended as written.
fn the_nearest_existing_ancestor_resolved(
    path: &Path,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let mut not_yet_existing = Vec::new();
    let mut ancestor = path;
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(resolved) => {
                return Ok(not_yet_existing
                    .into_iter()
                    .rev()
                    .fold(resolved, |resolved, name| resolved.join(name)));
            }
            Err(_) => {
                let (Some(parent), Some(name)) = (ancestor.parent(), ancestor.file_name()) else {
                    return Ok(path.to_path_buf());
                };
                not_yet_existing.push(name.to_owned());
                ancestor = parent;
            }
        }
    }
}

/// Copy the folder `source` and everything under it to `destination`, which must not exist.
fn copy_a_folder_whole(source: &Path, destination: &Path) -> Result<(), ProjectRepositoryRefusal> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let copied_to = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_a_folder_whole(&entry.path(), &copied_to)?;
        } else {
            std::fs::copy(entry.path(), copied_to)?;
        }
    }
    Ok(())
}

fn refuse_project_settings_the_project_cannot_hold(
    project_file: &TatolabProjectFile,
    assets: &BTreeMap<Uuid, ProjectAsset>,
) -> Result<(), ProjectRepositoryRefusal> {
    if project_file.name.trim().is_empty() {
        return Err(ProjectRepositoryRefusal::BlankProjectName);
    }
    let Some(cover) = project_file.cover else {
        return Ok(());
    };
    let cover_asset = assets
        .get(&cover)
        .ok_or(ProjectRepositoryRefusal::NoSuchAsset(cover))?;
    if cover_asset.kind != ProjectAssetKind::Media {
        return Err(ProjectRepositoryRefusal::CoverIsNotAMediaAsset {
            asset: cover,
            kind: match cover_asset.kind {
                ProjectAssetKind::Media => "media",
                ProjectAssetKind::Scene => "scene",
                ProjectAssetKind::Data => "data",
                ProjectAssetKind::Stream => "stream",
            },
        });
    }
    Ok(())
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

fn refuse_input_values_referencing_an_asset_the_project_does_not_hold(
    assets: &BTreeMap<Uuid, ProjectAsset>,
    node: Uuid,
    input_values: &serde_json::Map<String, Value>,
) -> Result<(), ProjectRepositoryRefusal> {
    match assets_a_nodes_input_values_reference(input_values)
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

/// Refuse `file_name` unless it names one visible file, with no folder.
fn refuse_an_import_file_name(file_name: &str) -> Result<(), ProjectRepositoryRefusal> {
    let refuse = |why: &'static str| ProjectRepositoryRefusal::ImportFileNameRefused {
        file_name: file_name.to_owned(),
        why,
    };
    if file_name.is_empty() {
        return Err(refuse("it is empty"));
    }
    if file_name.contains('/') || file_name.contains('\\') {
        return Err(refuse("it names a folder; send `asset_path` to choose one"));
    }
    if file_name.starts_with('.') {
        return Err(refuse("a hidden file is never an asset"));
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

/// Refuse a write over a file that no longer holds the bytes the caller last read, named by
/// `sha256_of_the_content_last_read`: other bytes, or no file at all.
fn refuse_a_file_changed_since_it_was_read(
    path: &ProjectRelativeFilePath,
    sha256_of_the_content_last_read: &str,
    bytes_on_disk_now: Option<&[u8]>,
) -> Result<(), ProjectRepositoryRefusal> {
    if !is_a_sha256_digest(sha256_of_the_content_last_read) {
        return Err(ProjectRepositoryRefusal::ExpectedSha256IsNotADigest(
            sha256_of_the_content_last_read.to_owned(),
        ));
    }
    let Some(bytes_on_disk_now) = bytes_on_disk_now else {
        return Err(ProjectRepositoryRefusal::ProjectFileRemovedSinceItWasRead(
            path.to_string(),
        ));
    };
    let sha256_on_disk_now = content_sha256_of(bytes_on_disk_now);
    if sha256_on_disk_now.eq_ignore_ascii_case(sha256_of_the_content_last_read) {
        return Ok(());
    }
    Err(ProjectRepositoryRefusal::ProjectFileChangedSinceItWasRead {
        path: path.to_string(),
        sha256_read: sha256_of_the_content_last_read.to_ascii_lowercase(),
        sha256_on_disk_now,
    })
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

/// One author's transactions an undo reverses and a redo reapplies, each newest last.
#[derive(Default)]
struct UndoAndRedoStacksOfOneAuthor {
    undoable: Vec<AppliedProjectTransaction>,
    redoable: Vec<AppliedProjectTransaction>,
}

impl UndoAndRedoStacksOfOneAuthor {
    /// Follow `applied`, the author's next transaction: an edit clears what could be redone.
    fn replay(&mut self, applied: AppliedProjectTransaction) {
        match applied.transaction.lineage() {
            ProjectTransactionLineage::NewEdit => {
                self.redoable.clear();
                self.undoable.push(applied);
            }
            ProjectTransactionLineage::UndoOf(undone) => {
                if let Some(position) = self
                    .undoable
                    .iter()
                    .rposition(|held| held.transaction.transaction == undone)
                {
                    self.redoable.push(self.undoable.remove(position));
                }
            }
            ProjectTransactionLineage::RedoOf(redone) => {
                if let Some(position) = self
                    .redoable
                    .iter()
                    .rposition(|held| held.transaction.transaction == redone)
                {
                    self.redoable.remove(position);
                    self.undoable.push(applied);
                }
            }
        }
    }
}
