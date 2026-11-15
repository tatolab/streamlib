// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The params of the operations on a project's assets and stream assets, and the asset kind they name.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use streamlib_project_repository::ProjectAssetKind;

use super::{
    ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, AUTHOR_ARGUMENT_DESCRIPTION,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorArgument,
};

/// What `asset` says.
pub const ASSET_ARGUMENT_DESCRIPTION: &str =
    "The asset, by the UUID `import_asset` or `list_assets` answered.";

/// What `stream_asset` says.
pub const STREAM_ASSET_ARGUMENT_DESCRIPTION: &str = "The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports as its location's `stream_asset`.";

/// What `stream_display_name` says.
pub const STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION: &str =
    "The name the stream is shown under, written in its graph document; refused when blank.";

/// What an asset is, as a caller names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAssetKindArgument {
    /// Video, audio or an image.
    Media,
    /// USD, glTF and other 3D scenes.
    Scene,
    /// Everything else a node reads: an FMU, a LUT, model weights, a font.
    Data,
    /// A stream's graph document; listed, never imported.
    Stream,
}

/// `list_assets`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAssetsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Only the assets of this kind.
    #[schemars(description = "Only the assets of this kind. Omit it for every asset.")]
    pub kind: Option<ProjectAssetKindArgument>,
}

/// `describe_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
}

/// `read_asset_preview`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadAssetPreviewRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
}

/// `import_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the bytes come from.
    #[schemars(
        description = "Where the bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    pub source: String,
    /// Where the asset sits in the project, `/`-separated and relative to the project directory,
    /// such as `media/intro.mp4`.
    #[schemars(
        description = "Where the asset sits in the project, `/`-separated and relative to the project directory, such as `media/intro.mp4`. Omit it to place the file in its kind's folder — `media/`, `scenes/` or `data/` — under `file_name`, numbered `<name>-2.<extension>`, `-3`, … past a name already taken there."
    )]
    pub asset_path: Option<String>,
    /// The asset's file name in its kind's folder when `asset_path` is omitted, such as
    /// `intro.mp4`.
    #[schemars(
        description = "The asset's file name in its kind's folder when `asset_path` is omitted, such as `intro.mp4`. Omit it for the source file's own name; an upload, which carries no name, needs it."
    )]
    pub file_name: Option<String>,
    /// `media`, `scene` or `data`.
    #[schemars(
        description = "`media`, `scene` or `data`. Omit it for the kind the file's extension implies, `data` when it implies none."
    )]
    pub kind: Option<ProjectAssetKindArgument>,
    /// The settings the import takes, kept in the asset's sidecar.
    #[schemars(description = "The settings the import takes, kept in the asset's sidecar.")]
    pub import_settings: Option<Map<String, Value>>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `relocate_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelocateAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// The asset's new path in the project, `/`-separated and relative to the project directory,
    /// such as `archive/opening.mp4`.
    #[schemars(
        description = "The asset's new path in the project, `/`-separated and relative to the project directory, such as `archive/opening.mp4`; its folders are made when missing."
    )]
    pub to_path: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `delete_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_asset_settings`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAssetSettingsRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// The settings that replace the asset's import settings whole.
    #[schemars(description = "The settings that replace the asset's import settings whole.")]
    pub import_settings: Map<String, Value>,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `replace_asset`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceAssetRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The asset, by the UUID `import_asset` or `list_assets` answered.
    #[schemars(description = ASSET_ARGUMENT_DESCRIPTION)]
    pub asset: String,
    /// Where the new bytes come from.
    #[schemars(
        description = "Where the new bytes come from: the absolute path of a file on this machine, or the SHA-256, in 64 lowercase hex digits, of an upload in the project's `.tatolab/uploads/`."
    )]
    pub source: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `create_stream`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateStreamRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// Where the stream's graph document sits in the project, `/`-separated and relative to the
    /// project directory and ending `.graph.json`, such as `streams/preview.graph.json`.
    #[schemars(
        description = "Where the stream's graph document sits in the project, `/`-separated and relative to the project directory and ending `.graph.json`, such as `streams/preview.graph.json`."
    )]
    pub path: String,
    /// The name the stream is shown under, written in its graph document.
    #[schemars(description = STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION)]
    pub display_name: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `delete_stream`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteStreamRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports
    /// as its location's `stream_asset`.
    #[schemars(description = STREAM_ASSET_ARGUMENT_DESCRIPTION)]
    pub stream_asset: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

/// `set_stream_display_name`'s params.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetStreamDisplayNameRequest {
    /// The project, by absolute path.
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    pub project_directory: PathBuf,
    /// The stream asset, by the UUID `create_stream` answered as `asset` or `list_streams` reports
    /// as its location's `stream_asset`.
    #[schemars(description = STREAM_ASSET_ARGUMENT_DESCRIPTION)]
    pub stream_asset: String,
    /// The name the stream is shown under, written in its graph document.
    #[schemars(description = STREAM_DISPLAY_NAME_ARGUMENT_DESCRIPTION)]
    pub display_name: String,
    /// Who the change is by, as history records it and undo follows it.
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    pub author: Option<ProjectTransactionAuthorArgument>,
    /// A key the caller mints once per user gesture, such as a slider drag committed on release.
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    pub idempotency_key: Option<String>,
}

impl From<ProjectAssetKindArgument> for ProjectAssetKind {
    fn from(kind: ProjectAssetKindArgument) -> Self {
        match kind {
            ProjectAssetKindArgument::Media => Self::Media,
            ProjectAssetKindArgument::Scene => Self::Scene,
            ProjectAssetKindArgument::Data => Self::Data,
            ProjectAssetKindArgument::Stream => Self::Stream,
        }
    }
}
