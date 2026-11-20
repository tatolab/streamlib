// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetTransactionToolResult, DescribeAssetToolResult, ListAssetsToolResult,
    ReadAssetPreviewToolResult,
};

use super::EVERY_SURFACE;
use super::worked_examples;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExample, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    CreateStreamRequest, DeleteAssetRequest, DeleteStreamRequest, DescribeAssetRequest,
    ImportAssetRequest, ListAssetsRequest, ReadAssetPreviewRequest, RelocateAssetRequest,
    ReplaceAssetRequest, SetAssetSettingsRequest, SetStreamDisplayNameRequest,
};

/// `list_assets`: list a project's assets, ordered by path.
pub struct ListAssets;

impl ControlOperation for ListAssets {
    type Request = ListAssetsRequest;
    type Result = ListAssetsToolResult;
    const METHOD_NAME: &'static str = "list_assets";
    const ONE_LINE_DOC: &'static str = "List a project's assets, ordered by path.";
    const AGENT_USAGE_NOTE: &'static str = "List a project's assets, ordered by path: each by its `asset` UUID, its `kind` (`stream`, `media`, `scene` or `data`), its `path` in the project, the SHA-256 and size of its bytes, its `import_settings`, and what the runtime has read from its bytes — `media_facts_state` (`read`, `reading`, `unreadable` with `media_facts_unreadable_because`, or `null` for a stream), `media_facts` and `preview`, as `describe_asset` answers them, each from what is already read, an asset not yet read queued for a read — with the project's UUID, `project_name`, `project_description`, `project_cover` — the UUID of the image asset it is shown by — and `project_last_edited_at`, when its history last changed in seconds since the Unix epoch, each `null` when it has none, and `transactions_resource_uri`, its change feed. `kind` narrows to one kind. Refused for a folder with no `tatolab-project.json`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::LIST_ASSETS);
}

/// `describe_asset`: describe one asset of a project.
pub struct DescribeAsset;

impl ControlOperation for DescribeAsset {
    type Request = DescribeAssetRequest;
    type Result = DescribeAssetToolResult;
    const METHOD_NAME: &'static str = "describe_asset";
    const ONE_LINE_DOC: &'static str = "Describe one asset of a project.";
    const AGENT_USAGE_NOTE: &'static str = "Describe one asset of a project: its UUID, kind, path, the SHA-256 and size of its bytes, its import settings, `file_at_its_path`, whether a file is at its path, and what the runtime has read from its bytes, which it reads once per SHA-256 after an import or a replace and keeps in the project's `.tatolab/`. `media_facts_state` is `read`, `reading` — a read queued or under way, after which the project's change feed carries a `media_facts_read` entry — `unreadable`, with why in `media_facts_unreadable_because`, or `null` for a stream asset. `media_facts` carries `duration_ms`; `video` with `pixel_width`, `pixel_height`, `frames_per_second` and `codec`; `audio` with `sample_rate_hz`, `channel_count` and `codec`; `image` with `pixel_width` and `pixel_height`; `lut` with `lut_1d_size` or `lut_3d_size`, `domain_min` and `domain_max`; and `scene` — each `null` when the file does not hold or state it. `preview` is the size and `media_type` of the picture `read_asset_preview` answers, `null` when the asset has none.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::DESCRIBE_ASSET);
}

/// `read_asset_preview`: read an asset's preview picture.
pub struct ReadAssetPreview;

impl ControlOperation for ReadAssetPreview {
    type Request = ReadAssetPreviewRequest;
    type Result = ReadAssetPreviewToolResult;
    const METHOD_NAME: &'static str = "read_asset_preview";
    const ONE_LINE_DOC: &'static str = "Read an asset's preview picture.";
    const AGENT_USAGE_NOTE: &'static str = "Read an asset's preview picture, which the runtime makes from its bytes and keeps in the project's `.tatolab/`: a PNG at most 512 pixels on its longest edge, answered as `content_base64`, standard base64 with padding, with its `media_type`, `pixel_width`, `pixel_height` and `byte_len`. An image has one; video, audio, a LUT, a scene and other data have none yet, as `describe_asset`'s `preview` being `null` says. Refused while the runtime is still reading the asset's bytes — `describe_asset`'s `media_facts_state` reads `reading`, and the project's change feed carries a `media_facts_read` entry once it is done — for an asset with no preview, and for a stream asset.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::READ_ASSET_PREVIEW);
}

/// `import_asset`: import a file as a new asset of a project, as one transaction.
pub struct ImportAsset;

impl ControlOperation for ImportAsset {
    type Request = ImportAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "import_asset";
    const ONE_LINE_DOC: &'static str =
        "Import a file as a new asset of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Import a file as a new asset of the project at `asset_path`, or, with no `asset_path`, in its kind's folder of the project's standard layout — `media/` for video, audio and images, `scenes/` for USD, glTF and other 3D scenes, `data/` for everything else — under `file_name` or the source file's own name, numbered `<name>-2.<extension>`, `-3`, … past a name an asset or another file already takes there. Answers the `asset`, with the UUID the runtime made for it, which every later relocation keeps, and the `path` it landed at, and the `transaction` and `commit` that record it. The bytes are copied once, to `asset_path`, and only the asset's sidecar, `<asset_path>.asset.json`, enters history. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there; a file already at `asset_path` is imported in place. Undoing the import removes the file at `asset_path`, one imported in place included, and nothing keeps its bytes. Refused whole, with nothing written, for a path another asset or another file holds, a path outside the project, and a stream, which is never imported.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::IMPORT_ASSET);
}

/// `relocate_asset`: move or rename an asset inside its project, as one transaction.
pub struct RelocateAsset;

impl ControlOperation for RelocateAsset {
    type Request = RelocateAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "relocate_asset";
    const ONE_LINE_DOC: &'static str =
        "Move or rename an asset inside its project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Put an asset at another path of the project, renaming it, moving it to another folder or both, its sidecar going with it, as one transaction; the asset keeps its UUID. Answers the asset as it now is, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::RELOCATE_ASSET);
}

/// `delete_asset`: delete an asset from a project, as one transaction.
pub struct DeleteAsset;

impl ControlOperation for DeleteAsset {
    type Request = DeleteAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "delete_asset";
    const ONE_LINE_DOC: &'static str = "Delete an asset from a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Stop tracking an asset as one transaction: its file and sidecar leave the project, and nothing keeps its bytes, so undoing the transaction brings back its sidecar and the asset reads missing until its file is at its path again. A stream asset's graph document leaves the project, recorded in the transaction so an undo writes it back, and its stream is unloaded and forgotten. Refused while a stream's graph document references the asset, the refusal naming each document. Answers the asset as it was, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::DELETE_ASSET);
}

/// `set_asset_settings`: replace an asset's import settings, as one transaction.
pub struct SetAssetSettings;

impl ControlOperation for SetAssetSettings {
    type Request = SetAssetSettingsRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "set_asset_settings";
    const ONE_LINE_DOC: &'static str = "Replace an asset's import settings, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Replace an asset's import settings whole, as one transaction. Answers the asset as it now is, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::SET_ASSET_SETTINGS);
}

/// `replace_asset`: replace an asset's bytes, as one transaction.
pub struct ReplaceAsset;

impl ControlOperation for ReplaceAsset {
    type Request = ReplaceAssetRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "replace_asset";
    const ONE_LINE_DOC: &'static str = "Replace an asset's bytes, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Put new bytes at an asset's path as one transaction: the asset keeps its UUID, path, kind and settings, so every node referencing it reads the new bytes at its next load, and its sidecar records their SHA-256 and size. `source` is a file on this machine by absolute path, copied there, or an upload in the project's `.tatolab/uploads/` by its SHA-256, moved there. The bytes replaced are kept as an upload under their SHA-256, so an undo puts them back with the earlier sidecar; history records the sidecar alone. A file replaced outside the runtime is not supported. Answers the asset as it now is, with the `transaction` and `commit`; bytes the sidecar already records apply nothing and are refused as an empty transaction. Refused for a stream asset, whose graph document graph edits change.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::REPLACE_ASSET);
}

/// `create_stream`: create a stream in a project, as one transaction.
pub struct CreateStream;

impl ControlOperation for CreateStream {
    type Request = CreateStreamRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "create_stream";
    const ONE_LINE_DOC: &'static str = "Create a stream in a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Create a stream in the project as one transaction: a new stream asset, its graph document written at `path` holding no nodes, its UUID, which the runtime makes, and its `display_name`. While the project runs, the new stream is loaded on the project's queue and the answer carries that load's `action`, which `describe_stream_action` follows. Answers the stream asset as `asset`, with the `transaction` and `commit`; an undo takes the document out and unloads the stream. Refused for a path another file holds or outside the project, and a blank `display_name`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CREATE_STREAM);
}

/// `delete_stream`: delete a stream from a project, as one transaction.
pub struct DeleteStream;

impl ControlOperation for DeleteStream {
    type Request = DeleteStreamRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "delete_stream";
    const ONE_LINE_DOC: &'static str = "Delete a stream from a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Delete a stream from the project as one transaction: its graph document leaves the project, recorded in the transaction so an undo writes it back and loads the stream again while the project runs, and its stream is unloaded and forgotten. Answers the stream asset as it was, with the `transaction` and `commit`. Refused for a UUID the project holds no stream asset under.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::DELETE_STREAM);
}

/// `set_stream_display_name`: set the name a stream is shown under, as one transaction.
pub struct SetStreamDisplayName;

impl ControlOperation for SetStreamDisplayName {
    type Request = SetStreamDisplayNameRequest;
    type Result = AssetTransactionToolResult;
    const METHOD_NAME: &'static str = "set_stream_display_name";
    const ONE_LINE_DOC: &'static str = "Set the name a stream is shown under, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Show a stream under another display name as one transaction, written in its graph document so every machine the project is on shows it and an undo writes the earlier one. A loaded stream is shown under it at once and keeps its stream id. Answers the stream asset as it now is, with the `transaction` and `commit`. Refused for a blank `display_name`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Asset;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::SET_STREAM_DISPLAY_NAME);
}
