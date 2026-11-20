// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use streamlib_node_schema::pin_type::PinType;

use super::NodeTypeCategoryFrom;
use super::graph_wire_types::RefusedGraphLinkOnTheWire;

/// The `remove_project` tool's result: the removal of the whole project queued on its queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct RemoveProjectToolResult {
    /// The queued removal, as `describe_stream_action` takes it.
    pub action: String,
    /// The project whose streams it unloads and forgets, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `delete_project` tool's result: the deletion of the whole project queued on its queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DeleteProjectToolResult {
    /// The queued deletion, as `describe_stream_action` takes it.
    pub action: String,
    /// The project whose streams it forgets and whose folder it moves to the Trash, symlinks
    /// resolved.
    pub project_directory: PathBuf,
}

/// The `duplicate_project` tool's result: the duplicate, a project of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DuplicateProjectToolResult {
    /// The duplicate's folder.
    pub project_directory: PathBuf,
    /// The duplicate's UUID, new to it.
    pub project: String,
    /// The duplicate's name.
    pub project_name: String,
    /// The commit on the duplicate's history that records it was duplicated.
    pub commit: String,
}

/// The result of `rename_project`, `set_project_description` and `set_project_cover`: how
/// people see the project after the transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectSettingsTransactionToolResult {
    /// The project's UUID, which no settings change touches.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// What the project is, `null` for no description.
    pub project_description: Option<String>,
    /// The image asset the project is shown by, by its UUID, `null` for no cover.
    pub project_cover: Option<String>,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

/// The `create_project` tool's result: the load it queued of the project it created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CreateProjectToolResult {
    /// The queued action's id, which `describe_stream_action` and `logs` take.
    pub action: String,
    /// The id the new project's `streams/main.graph.json` loads as.
    pub stream: String,
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `list_samples` tool's result: every sample project the runtime unit carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListSamplesToolResult {
    /// Each sample, in name order.
    pub samples: Vec<ListSamplesToolResultSample>,
}

/// One sample project the runtime unit carries, which `create_project` takes by `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListSamplesToolResultSample {
    /// The name `create_project`'s `sample` takes.
    pub name: String,
    /// The sample's title.
    pub title: String,
    /// What the sample shows, in a sentence or two; empty when it says nothing.
    pub summary: String,
}

/// The `import_project` tool's result: the project it unpacked from a `.zip` of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ImportProjectToolResult {
    /// The new project's directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The new project's UUID, its own and never the zipped project's.
    pub project: String,
    /// The project's name, as the zip's `tatolab-project.json` named it.
    pub name: String,
    /// The first commit of the new project's history, which records every file it unpacked.
    pub commit: String,
}

/// One asset of a project as the asset tools report it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectAssetOnTheWire {
    /// The asset's UUID, which the runtime made at import and which a rename or move keeps.
    pub asset: String,
    /// `stream`, `media`, `scene` or `data`.
    pub kind: String,
    /// Its path in the project, `/`-separated, relative to the project directory.
    pub path: String,
    /// The SHA-256 of its bytes as its sidecar records them, in lowercase hex.
    pub content_sha256: String,
    /// The size of its bytes.
    pub content_byte_len: u64,
    /// The settings its import took.
    pub import_settings: serde_json::Map<String, serde_json::Value>,
}

/// What the runtime read from a media, scene or data asset's bytes; a fact it could not read is
/// `null`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetMediaFactsOnTheWire {
    /// How long the media plays, in whole milliseconds.
    pub duration_ms: Option<u64>,
    /// The first video track's facts, `null` for a file with no video.
    pub video: Option<AssetVideoFactsOnTheWire>,
    /// The first audio track's facts, `null` for a file with no audio.
    pub audio: Option<AssetAudioFactsOnTheWire>,
    /// A still image's facts, `null` for anything else.
    pub image: Option<AssetImageFactsOnTheWire>,
    /// A colour lookup table's facts, `null` for anything else.
    pub lut: Option<AssetLutFactsOnTheWire>,
    /// A 3D scene's facts, `null` for anything else.
    pub scene: Option<AssetSceneFactsOnTheWire>,
}

/// A video track's facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetVideoFactsOnTheWire {
    /// The coded picture's width in pixels.
    pub pixel_width: Option<u32>,
    /// The coded picture's height in pixels.
    pub pixel_height: Option<u32>,
    /// The average frame rate, in frames per second.
    pub frames_per_second: Option<f64>,
    /// The codec, such as `h264`, `h265`, `av1`, `vp9` or `prores`.
    pub codec: Option<String>,
}

/// An audio track's facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetAudioFactsOnTheWire {
    /// Samples per second of each channel.
    pub sample_rate_hz: Option<u32>,
    /// How many channels it carries.
    pub channel_count: Option<u32>,
    /// The codec, such as `aac`, `mp3`, `opus`, `flac` or `pcm`.
    pub codec: Option<String>,
}

/// A still image's facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetImageFactsOnTheWire {
    /// Its width in pixels.
    pub pixel_width: u32,
    /// Its height in pixels.
    pub pixel_height: u32,
}

/// A colour lookup table's facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetLutFactsOnTheWire {
    /// Entries per channel of a 1D table, `null` for a 3D one.
    pub lut_1d_size: Option<u32>,
    /// Entries along each edge of a 3D table's cube, `null` for a 1D one.
    pub lut_3d_size: Option<u32>,
    /// The input value each channel's first entry maps, `[0, 0, 0]` when the file states none.
    pub domain_min: [f64; 3],
    /// The input value each channel's last entry maps, `[1, 1, 1]` when the file states none.
    pub domain_max: [f64; 3],
}

/// A 3D scene's facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetSceneFactsOnTheWire {
    /// How many prims the scene defines.
    pub prim_count: Option<u64>,
}

/// An asset's preview picture, which `read_asset_preview` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetPreviewOnTheWire {
    /// The picture's width in pixels.
    pub pixel_width: u32,
    /// The picture's height in pixels.
    pub pixel_height: u32,
    /// The picture's media type, `image/png`.
    pub media_type: String,
}

/// One asset with what the runtime has read from its bytes, as `list_assets` and
/// `describe_asset` answer it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectAssetWithItsMediaFactsOnTheWire {
    /// The asset.
    #[serde(flatten)]
    pub asset: ProjectAssetOnTheWire,
    /// `read`, `reading` or `unreadable`; `null` for a stream asset, whose graph `graph` serves.
    pub media_facts_state: Option<String>,
    /// Why the runtime could not read the asset's bytes, in its own words, while `unreadable`.
    pub media_facts_unreadable_because: Option<String>,
    /// What the runtime read, while `read`.
    pub media_facts: Option<AssetMediaFactsOnTheWire>,
    /// The asset's preview picture, `null` for an asset with none or not yet read.
    pub preview: Option<AssetPreviewOnTheWire>,
}

/// The `read_asset_preview` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ReadAssetPreviewToolResult {
    /// The asset's UUID.
    pub asset: String,
    /// The picture's width in pixels.
    pub pixel_width: u32,
    /// The picture's height in pixels.
    pub pixel_height: u32,
    /// The picture's media type, `image/png`.
    pub media_type: String,
    /// The picture's length in bytes.
    pub byte_len: u64,
    /// The picture's bytes, standard base64 with padding.
    pub content_base64: String,
}

/// The `list_assets` tool's result: the project's assets, ordered by path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListAssetsToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// What the project is, `null` when nobody has described it.
    pub project_description: Option<String>,
    /// The image asset the project is shown by, by its UUID, `null` when it has no cover.
    pub project_cover: Option<String>,
    /// When the project's history last changed, in whole seconds since the Unix epoch, for
    /// display only; `null` before its first commit.
    pub project_last_edited_at: Option<i64>,
    /// The assets, narrowed to one kind when the call named one, each with the media facts
    /// already read and no file read for the listing.
    pub assets: Vec<ProjectAssetWithItsMediaFactsOnTheWire>,
    /// The project's change feed: the resource a client reads for its newest transactions and
    /// names in `subscriptions/listen` to be told of each new one.
    pub transactions_resource_uri: String,
}

/// One transaction a project's change feed holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectTransactionOnTheWire {
    /// The format the transaction was recorded in, `1` for one recorded before formats were.
    pub format_version: u32,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
    /// The commit history stood at when it applied, absent for a project's first.
    pub parent_commit: Option<String>,
    /// `person`, `agent` or `runtime`.
    pub author_kind: String,
    /// The author's name as history shows it.
    pub author_name: String,
    /// What it changed, in order, each tagged by its `operation`.
    pub operations: Vec<serde_json::Value>,
    /// What undoes it, in order.
    pub inverse_operations: Vec<serde_json::Value>,
    /// The transaction this one undoes, when it is an undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
    /// The transaction this one redoes, when it is a redo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redoes: Option<String>,
    /// The key the caller sent it under, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The actions its caller sent, exactly as sent, each tagged by its `operation`; absent for
    /// an undo, a redo, or a change the runtime made itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<serde_json::Value>,
    /// Where an edit or a redo stands with its author's undo and redo now: `applied`,
    /// `undone_redoable` or `undone_not_redoable`, the same for an edit and every redo of it;
    /// absent for an undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_state: Option<String>,
    /// When the runtime applied it, RFC 3339 in UTC to the second, for display only.
    pub applied_at: String,
}

/// An action on a project the runtime refused, as its change feed holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct RefusedProjectActionOnTheWire {
    /// `person`, `agent` or `runtime`.
    pub author_kind: String,
    /// The author's name as an applied transaction would carry it.
    pub author_name: String,
    /// `commit` or `live`, as the action asked.
    pub mode: String,
    /// The operations the action asked for, in order, as the request named them.
    pub operations: Vec<serde_json::Value>,
    /// The key the caller sent it under, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// What the runtime refused, in its own words.
    pub refusal: ProjectActionRefusalOnTheWire,
    /// When the runtime refused it, RFC 3339 in UTC to the second, for display only.
    pub refused_at: String,
}

/// One entry of a project's change feed, tagged by its `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProjectChangeFeedEntryOnTheWire {
    /// A transaction the project's history records.
    Applied(ProjectTransactionOnTheWire),
    /// An action the runtime refused since it started, which no history records.
    Refused(RefusedProjectActionOnTheWire),
    /// The runtime finished reading an asset's media facts since it started; no history
    /// records it.
    MediaFactsRead(AssetMediaFactsReadOnTheWire),
}

/// An asset whose media facts the runtime finished reading, as a project's change feed holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetMediaFactsReadOnTheWire {
    /// The asset's UUID.
    pub asset: String,
    /// The SHA-256 of the bytes read, in lowercase hex.
    pub content_sha256: String,
    /// `read` or `unreadable`.
    pub media_facts_state: String,
    /// When the read finished, RFC 3339 in UTC to the second.
    pub read_at: String,
}

/// A refused call as every door answers it: the `data` of a JSON-RPC error, and both
/// `structuredContent.refusal` and the text of a failing tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectActionRefusalOnTheWire {
    /// What kind of refusal it is, for a caller to branch on; `refused` when a runtime that
    /// answers no code sent it.
    #[serde(default)]
    pub refusal_code: ControlOperationRefusalCode,
    /// The action refused, by the name of the operation or tool it was sent to.
    pub action: String,
    /// The position, from 0, of the operation refused among those the action sent, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_operation_index: Option<usize>,
    /// The node the refused operation names, as the caller named it: a UUID or an `as` label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_node: Option<String>,
    /// The link the refused operation names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_link: Option<Box<RefusedGraphLinkOnTheWire>>,
    /// The node's input or config field the refused operation names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_field: Option<String>,
    /// The JSON pointer, into the params the caller sent, of the value refused; absent when no
    /// one value inside them is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_params_pointer: Option<String>,
    /// The place in a project file the refusal is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_file: Option<ProjectFilePlaceOnTheWire>,
    /// Why the node the action reached for does not run, when that is why it was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_running_because: Option<NodeNotRunningBecauseOnTheWire>,
    /// A call that would help, exactly as `call_operation` takes it, when the runtime knows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_call: Option<SuggestedOperationCall>,
    /// Why, in the runtime's own words, never rewritten.
    pub runtime_words: String,
}

impl ProjectActionRefusalOnTheWire {
    /// The refusal of `action`, coded `refusal_code`, in `runtime_words`, naming no place.
    pub fn of_the_action(
        action: impl Into<String>,
        refusal_code: ControlOperationRefusalCode,
        runtime_words: impl Into<String>,
    ) -> Self {
        Self {
            refusal_code,
            action: action.into(),
            refused_operation_index: None,
            refused_node: None,
            refused_link: None,
            refused_field: None,
            refused_params_pointer: None,
            refused_file: None,
            not_running_because: None,
            suggested_call: None,
            runtime_words: runtime_words.into(),
        }
    }

    /// This refusal as the JSON every door carries.
    pub fn the_refusal_as_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|why| {
            unreachable!("a refusal holds only strings, numbers and string-keyed maps: {why}")
        })
    }
}

/// What kind of refusal a refused call is. The set grows: a code a client does not know reads
/// as `refused`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ControlOperationRefusalCode {
    /// The params do not match the operation's params schema; `refused_params_pointer` names the
    /// value, when one is.
    ParamsDoNotMatchSchema,
    /// The runtime serves no operation under the name sent.
    OperationNotFound,
    /// The node the call reached for does not run; `not_running_because` says why.
    NodeNotRunning,
    /// The runtime could not carry the request out, through no doing of the caller.
    RuntimeFailed,
    /// The node has no input or setting of the name sent; the runtime's words list the ones it
    /// has.
    PortNotFound,
    /// The input takes only a link, so it holds no value.
    InputTakesNoValue,
    /// The value sent is not one the input takes: of another type, or outside its range. The
    /// runtime's words say what it takes.
    InputValueNotValid,
    /// No stream the runtime holds, or no stream asset the project holds, is the one named.
    StreamNotFound,
    /// The stream holds no node of the UUID or `as` label sent; `refused_node` names it, when the
    /// call named one.
    NodeNotFound,
    /// No node type of the name sent is one the stream can add.
    NodeTypeNotFound,
    /// Another node of the graph already has the name sent.
    NodeNameTaken,
    /// The link sent is already in the graph.
    LinkAlreadyExists,
    /// The two ends of the link sent carry pin types that do not connect, or connect only
    /// through a convert node `connect` does not place; the runtime's words name both ends, both
    /// types and each node type that converts between them.
    LinkPinTypesDoNotConnect,
    /// The two ends of the link sent connect only through a conversion a module or the project's
    /// own node declares, which `connect` never places; the runtime's words name each such node
    /// type. With exactly one, `suggested_call` is the `transact` that places it between them,
    /// only when both ends are UUID-named nodes the edited level holds.
    LinkNeedsAModuleConversion,
    /// The link sent ends at an input that is a setting, which takes a stored value and never a
    /// link; `refused_field` names it.
    InputTakesNoWire,
    /// The project holds no file or folder at the path sent; `refused_file` names it.
    FileNotFound,
    /// The file changed, or was taken out, since it was read, so the write was not made;
    /// `refused_file` names it. Read it again to see what it holds now.
    FileChangedSinceItWasRead,
    /// A check of the project's code was still running at its bound and was killed; the
    /// runtime's words give the bound, and `refused_file` the file it was checking.
    CheckRanOutOfTime,
    /// A check of the project's views and controls found no extension SDK kit to check them
    /// against; the runtime's words name each place it looked.
    ExtensionSdkNotFound,
    /// The request was understood and not done; changing it can help. The runtime's words say
    /// what to change.
    // `#[serde(other)]` holds only on the last variant, so a new code goes above this one.
    #[default]
    #[serde(other)]
    Refused,
}

/// A place in a project file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectFilePlaceOnTheWire {
    /// The file's path in the project, `/`-separated; its absolute path when the refusal is
    /// about a file the runtime could not read from disk.
    pub path: String,
    /// The line, from 1, when the place is narrower than the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The column, from 1, when the place is narrower than the line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// A call a refusal suggests: the arguments `call_operation` takes, or a listed tool by its
/// name with these params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SuggestedOperationCall {
    /// The operation to call, by name.
    pub operation: String,
    /// Its params.
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// Why a node a refused action reached for does not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeNotRunningBecauseOnTheWire {
    /// Its project's playback is stopped.
    PlaybackStopped,
    /// Its project's playback is paused, and the node was added since.
    PlaybackPaused,
    /// Its stream is starting, or has not compiled the node yet.
    Preparing,
    /// The node failed.
    NodeFailed,
}

/// What a project's transactions resource holds: its newest transactions and the actions refused
/// since the runtime started, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectTransactionsResourceContents {
    /// The project's UUID.
    pub project: String,
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// Its newest applied transactions and refused actions, newest first.
    pub transactions: Vec<ProjectChangeFeedEntryOnTheWire>,
}

/// The `describe_asset` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DescribeAssetToolResult {
    /// The asset, with what the runtime has read from its bytes.
    pub asset: ProjectAssetWithItsMediaFactsOnTheWire,
    /// Whether a file is at the asset's path.
    pub file_at_its_path: bool,
}

/// The result of an asset tool that changed one asset and committed the change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AssetTransactionToolResult {
    /// The asset as the transaction left it; a deleted asset as it was.
    pub asset: ProjectAssetOnTheWire,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
    /// The stream action loading the stream a created stream asset holds, while its project
    /// runs; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// One file of a project as `list_project_files` answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectFileOnTheWire {
    /// Where the file sits in the project, `/`-separated.
    pub path: String,
    /// The file's length in bytes.
    pub byte_len: u64,
}

/// The `list_project_files` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListProjectFilesToolResult {
    /// Each file listed, sorted by path.
    pub files: Vec<ProjectFileOnTheWire>,
}

/// The `read_project_file` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ReadProjectFileToolResult {
    /// The file read, as the call named it.
    pub path: String,
    /// The file's length in bytes.
    pub byte_len: u64,
    /// The file's bytes, standard base64 with padding.
    pub content_base64: String,
    /// The SHA-256 of the file's bytes, lowercase hex, which a `write_project_file` sends back
    /// as `expected_sha256`.
    pub content_sha256: String,
}

/// The `write_project_file` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectFileTransactionToolResult {
    /// The file written.
    pub path: String,
    /// The SHA-256 of the bytes written, lowercase hex, which the next `write_project_file`
    /// sends back as `expected_sha256`.
    pub content_sha256: String,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

/// The `create_node_file` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CreateNodeFileToolResult {
    /// The file created.
    pub path: String,
    /// The node type the file declares, `<module>:<class>`, as `add_node` takes it.
    pub node_type: String,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

/// A project this one imports, as a dependency tool answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectDependencyOnTheWire {
    /// The imported project's UUID.
    pub project: String,
    /// The folder under `imports/` that holds its copy.
    pub folder: String,
    /// Where it was copied from, as `add_dependency` or `update_dependency` was given it.
    pub source: String,
    /// The SHA-256 naming the version its copy carried.
    pub version: String,
}

/// The result of `add_dependency` and `remove_dependency`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DependencyTransactionToolResult {
    /// The import as the transaction left it; a removed one as it was.
    pub dependency: ProjectDependencyOnTheWire,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

/// The result of `update_dependency`: the copy it committed, or, when the import's folder was
/// edited since its copy and the overwrite was not confirmed, each edited file and no change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct UpdateDependencyToolResult {
    /// The import as the update left it.
    pub dependency: ProjectDependencyOnTheWire,
    /// Each file of the import's folder edited since its copy, by its path inside the folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_files: Vec<String>,
    /// Whether the update waits on `overwrite_edited_files` to overwrite `edited_files`.
    pub overwrite_confirmation_needed: bool,
    /// The transaction's UUID, when the update committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
    /// The commit on the project history's `main` that records it, when the update committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// One import as `list_dependencies` answers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListedProjectDependencyOnTheWire {
    /// The import as `tatolab-project.json` records it.
    pub dependency: ProjectDependencyOnTheWire,
    /// Each file of its folder edited since its copy, by its path inside the folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edited_files: Vec<String>,
    /// With `check_for_newer`, the version its source holds now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_on_the_source: Option<String>,
    /// With `check_for_newer`, whether `version_on_the_source` is not the copy's `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_version_exists: Option<bool>,
    /// Why the source could not be read, or the folder's files listed, in the runtime's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_refused_because: Option<String>,
}

/// The result of `list_dependencies`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListDependenciesToolResult {
    /// Each import of the project, in the order `tatolab-project.json` lists them.
    pub dependencies: Vec<ListedProjectDependencyOnTheWire>,
}

/// The `list_project_views` tool's result: every Tato Studio view a project and its imports
/// bring, read from each `views/<folder>/tatolab_view.json` without running anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListProjectViewsToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The project's own views by folder, then each import's in the order its project file
    /// records them.
    pub views: Vec<ProjectViewListed>,
}

/// One folder of a `views/`: the view type it declares, and whether it opens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectViewListed {
    /// The view type's id, `<project name>:<view folder>`, the node type a node of the view is.
    pub view_type: String,
    /// The UUID of the project whose `views/` holds it.
    pub project: String,
    /// That project's name.
    pub project_name: String,
    /// That project's folder under `imports/`; `None` for the listed project's own view.
    pub import_folder: Option<String>,
    /// The view's folder relative to the listed project, as `views/deck` or
    /// `imports/levels/views/meter`: the path a project file read names its files under.
    pub view_folder_in_the_project: String,
    /// Whether its manifest reads against the view contract this runtime reads.
    pub opens: bool,
    /// Why it does not open, in the runtime's own words; `None` when it opens.
    pub not_opened_because: Option<String>,
    /// The name the view-type menu lists; `None` when it does not open.
    pub title: Option<String>,
    /// What the view is for.
    pub description: Option<String>,
    /// Its icon, a path inside its folder.
    pub icon: Option<String>,
    /// Its preview picture, a path inside its folder.
    pub preview: Option<String>,
    /// `area` or `node`; `None` when it does not open.
    pub placement: Option<String>,
    /// The inputs a `node` view declares.
    pub inputs: Vec<ProjectViewDeclaredPort>,
    /// The outputs a `node` view declares.
    pub outputs: Vec<ProjectViewDeclaredPort>,
    /// Its `tatolab_view.json` as written, when it is JSON.
    pub manifest: Option<serde_json::Value>,
}

/// An input or output a view declares, as a node declares a port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProjectViewDeclaredPort {
    /// The port's name.
    pub name: String,
    /// The port family's wire name: the family its pin type's messages belong to, where it
    /// names one. Served until the contract's version changes, for a client that reads no
    /// `pin_type`.
    pub family: String,
    /// The pin type the port carries; absent on a port its manifest declares by `family` alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_type: Option<PinType>,
    /// What the port carries.
    pub description: Option<String>,
    /// The lowest and highest value an Array port carries.
    pub range: Option<[f64; 2]>,
}

/// The `list_project_node_types` tool's result: what a project and each source it draws node
/// code from declares, read from source without running it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListProjectNodeTypesToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The project first, then each import, then each installed distribution.
    pub sources: Vec<NodeTypeSourceReadWithoutRunningIt>,
}

/// Where a listed node type's code comes from, under `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum NodeTypeSourceOrigin {
    /// The project's own node code, outside `imports/`.
    Project,
    /// An import's node code as its folder under `imports/` holds it now.
    Dependency {
        /// The imported project's UUID.
        project: String,
        /// Its folder under `imports/`.
        folder: String,
        /// The version its copy carried.
        version: String,
        /// The imported project's name as the project file in its folder holds it; `null` when
        /// that file does not read.
        project_name: Option<String>,
    },
    /// An installed distribution that declares `tatolab.nodes` entry points.
    InstalledDistribution {
        /// The distribution's name.
        distribution: String,
        /// The distribution's version.
        distribution_version: String,
    },
}

/// One source's node types, graph generators, and the modules of it that could not be read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeSourceReadWithoutRunningIt {
    /// Where the code comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceOrigin,
    /// Each node type declared, by type.
    pub node_types: Vec<NodeTypeReadFromSource>,
    /// Each `@graph_generator` declared, by import path.
    pub graph_generators: Vec<GraphGeneratorReadFromSource>,
    /// Each module that does not parse, with why and where; the rest of the source still lists.
    pub unreadable_modules: Vec<NodeModuleNotReadable>,
}

/// One node type a project offers and the source its code comes from, as a change on the
/// project's feed names it: the source's fields beside the node type's, as
/// `list_project_node_types` lists each.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeOfOneProjectSource {
    /// Where its code comes from.
    #[serde(flatten)]
    pub origin: NodeTypeSourceOrigin,
    /// The node type as its declaration states it.
    #[serde(flatten)]
    pub node_type: NodeTypeReadFromSource,
}

/// One node type as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypeReadFromSource {
    /// The type `add_node` takes: the import path `<module>:<qualname>`.
    #[serde(rename = "type")]
    pub node_type: String,
    /// `process`, `function` or `shader`; `None` for an entry point whose declaration source does not show.
    pub kind: Option<String>,
    /// The description its `@node.meta` declares; empty when it declares none, and where only the node's native half states it.
    pub description: String,
    /// The group it is filed under, a path where the group is inside another, as `Effects/Stylize`: the project's own, else an import's, else the one its `@node.meta` declares; null when none files it, and for a Rust function node or an entry point whose declaration source does not show and no map files it, since only the node's native half states it.
    pub category: Option<String>,
    /// Who filed it under `category`; null when nothing files it.
    #[serde(default)]
    pub category_from: Option<NodeTypeCategoryFrom>,
    /// The input ports, `None` where only the node's native half states them.
    pub inputs: Option<Vec<NodeTypePortReadFromSource>>,
    /// The output ports, `None` where only the node's native half states them.
    pub outputs: Option<Vec<NodeTypePortReadFromSource>>,
    /// The file declaring it, relative to its source's root.
    pub path: Option<String>,
    /// The line of its `class`, `def` or assignment.
    pub line: Option<u32>,
}

/// One port as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeTypePortReadFromSource {
    /// The port's name, cast as every exposed name is.
    pub name: String,
    /// The port family's wire name, `None` where source alone does not say.
    pub family: Option<String>,
    /// The port's declared description.
    pub description: String,
    /// A function or shader port's annotation as written; `None` for a process node's port.
    pub annotation: Option<String>,
    /// A function input's default pin value, where it is a literal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "a_present_value_even_null"
    )]
    pub pin_default: Option<serde_json::Value>,
    /// A function input's default as written, where it is not a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_default_as_written: Option<String>,
}

/// One `@graph_generator` as its declaration states it in source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphGeneratorReadFromSource {
    /// The import path `import_graph_generator` takes: `<module>:<function>`.
    pub generator: String,
    /// Its docstring.
    pub description: String,
    /// Each parameter after the builder: a setting the fragment promotes.
    pub promoted_config: Vec<GraphGeneratorPromotedSettingReadFromSource>,
    /// The file declaring it, relative to its source's root.
    pub path: String,
    /// The line of its `def`.
    pub line: u32,
}

/// One setting a graph generator promotes, as its parameter states it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphGeneratorPromotedSettingReadFromSource {
    /// The parameter's name.
    pub name: String,
    /// Its annotation as written.
    pub annotation: Option<String>,
    /// Its default, where it is a literal.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "a_present_value_even_null"
    )]
    pub default: Option<serde_json::Value>,
    /// Its default as written, where it is not a literal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_as_written: Option<String>,
}

/// A module of a source that could not be read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeModuleNotReadable {
    /// The module's import name.
    pub module: String,
    /// Its file, relative to its source's root.
    pub path: String,
    /// The line the parser stopped at.
    pub line: Option<u32>,
    /// The column the parser stopped at.
    pub column: Option<u32>,
    /// Why it was not read, in the parser's words.
    pub why: String,
}

/// A key that is present deserializes to `Some`, `null` included, so a default of `None` is kept.
fn a_present_value_even_null<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}
