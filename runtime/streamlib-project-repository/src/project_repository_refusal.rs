// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use uuid::Uuid;

/// Why the runtime refused to open a project or to apply a transaction to it, in its own words.
#[derive(Debug, thiserror::Error)]
pub enum ProjectRepositoryRefusal {
    #[error("{project_directory} holds no project: it has no {project_file_name}")]
    NotAProject {
        project_directory: PathBuf,
        project_file_name: &'static str,
    },
    #[error("{project_directory} already holds a project")]
    AlreadyAProject { project_directory: PathBuf },
    #[error("{project_directory} already exists, so no project was unpacked there")]
    ProjectDirectoryAlreadyExists { project_directory: PathBuf },
    #[error(
        "{import_source} is a folder: a project folder opens where it is, and only a `.zip` of one is unpacked as a new project"
    )]
    ProjectArchiveIsAFolder { import_source: String },
    #[error("the project in {project_directory} already has a first commit")]
    ProjectAlreadyCommitted { project_directory: PathBuf },
    #[error("the project directory {0} is not an absolute path")]
    ProjectDirectoryNotAbsolute(PathBuf),
    #[error("the project file {path} could not be read: {why}")]
    ProjectFileUnreadable { path: PathBuf, why: String },
    #[error("the asset sidecar {path} could not be read: {why}")]
    AssetSidecarUnreadable { path: PathBuf, why: String },
    #[error("`{first}` and `{second}` both name the asset {asset}")]
    AssetNamedTwice {
        asset: Uuid,
        first: String,
        second: String,
    },
    #[error("`{path}` is not an asset path: {why}")]
    AssetPathRefused { path: String, why: &'static str },
    #[error("the project holds no asset {0}")]
    NoSuchAsset(Uuid),
    #[error("`{path}` is already the path of the asset {holder}")]
    AssetPathHeldByAnotherAsset { path: String, holder: Uuid },
    #[error("`{path}` already holds a file that is not the import's source")]
    AssetPathHoldsAnotherFile { path: String },
    #[error("a stream asset is a graph document the stream verbs create; it is never imported")]
    StreamAssetsAreNotImported,
    #[error(
        "an upload imported with no `asset_path` needs a `file_name`: its bytes carry no name of their own"
    )]
    UploadImportedWithNoPathNamesNoFileName,
    #[error("`{file_name}` is not a file name: {why}")]
    ImportFileNameRefused {
        file_name: String,
        why: &'static str,
    },
    #[error("the file path `{path}` is refused: {why}")]
    ProjectFilePathRefused { path: String, why: &'static str },
    #[error("the project holds no file at `{0}`")]
    NoSuchProjectFile(String),
    #[error("the project already holds `{0}`, and a new file is never written over it")]
    ProjectFileAlreadyExists(String),
    #[error("the project holds no folder at `{0}`")]
    NoSuchProjectFolder(String),
    #[error("the file `{path}` is not written with write_project_file: {why}")]
    ProjectFileWrittenByAnotherAction { path: String, why: &'static str },
    #[error("the file `{path}` changed since its transaction was recorded")]
    ProjectFileChangedSinceItWasRecorded { path: String },
    #[error(
        "`{path}` changed since it was read, so this write would overwrite that change; nothing was written. Read the file again to see what it holds now"
    )]
    ProjectFileChangedSinceItWasRead {
        path: String,
        sha256_read: String,
        sha256_on_disk_now: String,
    },
    #[error(
        "`{0}` was taken out of the project since it was read; nothing was written. Write it again with no `expected_sha256` to bring it back"
    )]
    ProjectFileRemovedSinceItWasRead(String),
    #[error(
        "`expected_sha256` `{0}` is not a SHA-256 digest: send the 64 hexadecimal digits `read_project_file` answered as `content_sha256`"
    )]
    ExpectedSha256IsNotADigest(String),
    #[error("`{0}` already holds this content, so the write has nothing to change")]
    ProjectFileAlreadyHoldsThisContent(String),
    #[error(
        "the stream asset {0} is a graph document, which graph edits change; its bytes are never replaced"
    )]
    StreamAssetsAreNotReplaced(Uuid),
    #[error("the stream asset {0} has no import settings: its settings are in its graph document")]
    StreamAssetHasNoImportSettings(Uuid),
    #[error("the asset {asset} cannot be deleted while {referencing_documents:?} reference it")]
    AssetStillReferenced {
        asset: Uuid,
        referencing_documents: Vec<String>,
    },
    #[error("a project's name cannot be blank")]
    BlankProjectName,
    #[error("the cover must be a media asset, and {asset} is a {kind} asset")]
    CoverIsNotAMediaAsset { asset: Uuid, kind: &'static str },
    #[error("the asset {0} cannot be deleted while it is the project's cover")]
    AssetIsTheProjectCover(Uuid),
    #[error("{into} already exists, so the project was not duplicated into it")]
    DuplicateIntoAnExistingPath { into: PathBuf },
    #[error("a project cannot be duplicated into {into}, which is inside the project itself")]
    DuplicateIntoTheProjectItself { into: PathBuf },
    #[error("the project holds no stream asset {0}")]
    NoSuchStream(Uuid),
    #[error("{0:?} cannot be a stream's display name: it shows nothing")]
    DisplayNameShowsNothing(String),
    #[error("the graph document {path} refused the change: {why}")]
    GraphOperationRefused { path: String, why: String },
    #[error("node {node}'s config references the asset {asset}, which the project does not hold")]
    ConfigReferencesNoAsset { node: Uuid, asset: Uuid },
    #[error("{author} has no transaction left to undo")]
    NothingToUndo { author: String },
    #[error("{author} has undone nothing that is left to redo")]
    NothingToRedo { author: String },
    #[error("the Python project file {path} could not be read: {why}")]
    PyprojectUnreadable { path: PathBuf, why: String },
    #[error(
        "`{first_folder}` and `{second_folder}` both declare the Python distribution \
         `{distribution_name}`; a project imports one module of each name"
    )]
    ImportsDeclareOneDistribution {
        distribution_name: String,
        first_folder: String,
        second_folder: String,
    },
    #[error(
        "the project's Python requirements, with this import change, cannot be resolved, so \
         nothing changed: {resolver_refusal}"
    )]
    ImportRequirementsUnresolvable { resolver_refusal: String },
    #[error("the import source {path} could not be read: {why}")]
    ImportSourceUnreadable { path: PathBuf, why: String },
    #[error("the project's uploads hold no upload {0}")]
    NoSuchUpload(String),
    #[error("the upload {content_sha256} holds bytes whose SHA-256 is {held_sha256}")]
    UploadHoldsOtherBytes {
        content_sha256: String,
        held_sha256: String,
    },
    #[error("a transaction must hold at least one change")]
    EmptyTransaction,
    #[error("the transaction's author needs a name")]
    AuthorWithoutAName,
    #[error("the graph document {path} could not be read, so what it references is unknown: {why}")]
    GraphDocumentUnreadable { path: String, why: String },
    #[error("`{name}` cannot be both a tracked file and a tracked folder in history")]
    TrackedFileAndFolderShareAName { name: String },
    #[error("the project {0} cannot import itself")]
    ProjectImportsItself(Uuid),
    #[error("the project already imports {0}; copy it again with update_dependency")]
    ImportAlreadyHeld(Uuid),
    #[error("the project imports no project {0}")]
    NoSuchImport(Uuid),
    #[error("`imports/{folder}` already holds another import; name another `folder`")]
    ImportFolderAlreadyHeld { folder: String },
    #[error(
        "{path} is where the runtime links the Core Nodes project it carries, and something \
         else stands there; move it away and load the project again"
    )]
    CoreProjectImportFolderTaken { path: std::path::PathBuf },
    #[error("`{folder}` is not an import's folder: {why}")]
    ImportFolderRefused { folder: String, why: &'static str },
    #[error(
        "the import source {import_source} is a relative path; a folder or a zip on this machine \
         is named by its absolute path"
    )]
    ImportSourceNotAnAbsolutePath { import_source: String },
    #[error("{import_source} holds no project to import: {what_is_missing}")]
    ImportSourceHoldsNoProject {
        import_source: String,
        what_is_missing: String,
    },
    #[error("{import_source} holds the project {held}, not the import {expected}")]
    ImportSourceHoldsAnotherProject {
        import_source: String,
        expected: Uuid,
        held: Uuid,
    },
    #[error("the import {project} in {import_folder} holds no asset {asset}")]
    ImportHoldsNoAsset {
        project: Uuid,
        asset: Uuid,
        import_folder: PathBuf,
    },
    #[error("the asset {asset} is missing: it needs the file {file}")]
    AssetFileMissing { asset: Uuid, file: PathBuf },
    #[error(
        "`imports/{folder}` was edited since it was copied: {edited_files:?}; copying it again \
         overwrites them, so confirm with `overwrite_edited_files`"
    )]
    ImportEditedSinceItsCopy {
        folder: String,
        edited_files: Vec<String>,
    },
    #[error(
        "the project still uses the import {project}: {referencing_nodes:?} reference it; remove \
         them first"
    )]
    ImportStillReferenced {
        project: Uuid,
        referencing_nodes: Vec<String>,
    },
    #[error(
        "the graph document {path} was edited after the transaction being reversed wrote it; \
         taking it out would lose those edits"
    )]
    StreamDocumentEditedSinceItWasWritten { path: String },
    #[error("an idempotency key cannot be blank; mint one per user gesture, or send none")]
    BlankIdempotencyKey,
    #[error(
        "an import's `source` must be an absolute path or a SHA-256 in 64 lowercase hex digits; \
         got `{0}`"
    )]
    ImportSourceNeitherAPathNorAnUpload(String),
    #[error(
        "the transaction {transaction} in the project's history is in format {format_version}, \
         which a newer runtime wrote; this runtime reads formats up to {newest_format_version}"
    )]
    TransactionFormatNewerThanThisRuntime {
        transaction: Uuid,
        format_version: u32,
        newest_format_version: u32,
    },
    #[error("the runtime unit carries no sample `{sample}`; it carries {carried}")]
    NoSuchSampleProject { sample: String, carried: String },
    #[error("the sample's {path} holds no stream asset UUID under `asset`")]
    SampleProjectMainStreamHoldsNoAsset { path: PathBuf },
    #[error(
        "`{project_directory}` already has {already_present}, and the sample's files would overwrite it; create the project in an empty directory"
    )]
    SampleProjectCopyWouldOverwrite {
        project_directory: PathBuf,
        already_present: String,
    },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[cfg(feature = "history")]
    #[error("the project's history refused the change: {0}")]
    History(#[from] git2::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}
