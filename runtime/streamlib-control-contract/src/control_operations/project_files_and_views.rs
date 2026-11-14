// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    CloseViewerToolResult, CreateNodeFileToolResult, ListProjectFilesToolResult,
    ListProjectNodeTypesToolResult, ListProjectViewsToolResult, OpenViewerToolResult,
    ProjectFileTransactionToolResult, ReadProjectFileToolResult, WriteStudioViewOutputToolResult,
};

use super::EVERY_SURFACE;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExposure, ControlOperationGroup, ControlOperationStability,
};
use crate::control_operation_requests::{
    CloseViewerRequest, CreateNodeFileRequest, ListProjectFilesRequest,
    ListProjectNodeTypesRequest, ListProjectViewsRequest, OpenViewerRequest,
    ReadProjectFileRequest, WriteProjectFileRequest, WriteStudioViewOutputRequest,
};

/// `list_project_files`: list the files of a project that are not assets.
pub struct ListProjectFiles;

impl ControlOperation for ListProjectFiles {
    type Request = ListProjectFilesRequest;
    type Result = ListProjectFilesToolResult;
    const METHOD_NAME: &'static str = "list_project_files";
    const ONE_LINE_DOC: &'static str = "List the files of a project that are not assets.";
    const AGENT_USAGE_NOTE: &'static str = "List the project's files, or those under `under`, each by its path and length in bytes, sorted by path. Hidden folders such as `.tatolab/`, Python virtual environments, `node_modules/`, `__pycache__/` and symbolic links are not listed.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `read_project_file`: read one file of a project.
pub struct ReadProjectFile;

impl ControlOperation for ReadProjectFile {
    type Request = ReadProjectFileRequest;
    type Result = ReadProjectFileToolResult;
    const METHOD_NAME: &'static str = "read_project_file";
    const ONE_LINE_DOC: &'static str = "Read one file of a project.";
    const AGENT_USAGE_NOTE: &'static str = "Read one of the project's files whole, answering its bytes as `content_base64`, standard base64 with padding, its `byte_len`, and its `content_sha256`, which a `write_project_file` sends back as `expected_sha256`. Refused for a path that is not a file of the project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `create_node_file`: create a node file in a project from the template, as one transaction.
pub struct CreateNodeFile;

impl ControlOperation for CreateNodeFile {
    type Request = CreateNodeFileRequest;
    type Result = CreateNodeFileToolResult;
    const METHOD_NAME: &'static str = "create_node_file";
    const ONE_LINE_DOC: &'static str =
        "Create a node file in a project from the template, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Create a new node file of the project from the runtime's node template, as one transaction an undo reverses: a `@node.process` class in the current `tatolab.stream` idiom, named after the file, with one Image input `video_in` whose frames it hands on unchanged to one Image output `video_out`, each with its description. It loads as a node type of the project with no edits and `list_project_node_types` lists it at once. Answers the `path`, the `node_type` it declares — `<module>:<class>`, what `add_node` takes — and the `transaction` and `commit`. Refused for a path that already holds a file, that is not a `.py` file importable as a Python module, or whose class name the template cannot declare, and for every path `write_project_file` refuses.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `write_project_file`: write one file of a project, as one transaction.
pub struct WriteProjectFile;

impl ControlOperation for WriteProjectFile {
    type Request = WriteProjectFileRequest;
    type Result = ProjectFileTransactionToolResult;
    const METHOD_NAME: &'static str = "write_project_file";
    const ONE_LINE_DOC: &'static str = "Write one file of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Write one of the project's files whole, as one transaction: a new file is created with its folders, an existing one overwritten. An undo puts back what the file held, or takes a new file out. Answers the `path`, the `content_sha256` of the bytes written, and the `transaction` and `commit`. With `expected_sha256`, refused when the file changed or was taken out since it was read, so a save never overwrites another change. Content the file already holds is refused, as it changes nothing. Refused for what another tool changes — a stream's graph document, an asset's sidecar or bytes (`replace_asset`), `tatolab-project.json`, a file under `imports/` — and for a file inside a hidden folder or a Python virtual environment, which history does not record.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::File;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_project_node_types`: list the node types a project offers, from its own node files and the
/// packages it installs.
pub struct ListProjectNodeTypes;

impl ControlOperation for ListProjectNodeTypes {
    type Request = ListProjectNodeTypesRequest;
    type Result = ListProjectNodeTypesToolResult;
    const METHOD_NAME: &'static str = "list_project_node_types";
    const ONE_LINE_DOC: &'static str = "List the node types a project offers, from its own node files and the packages it installs.";
    const AGENT_USAGE_NOTE: &'static str = "List the node types and graph generators a project offers, read from source without running any of its code: its own node code outside `imports/`, each import's as its folder holds it now, and each installed distribution that declares `tatolab.nodes` entry points, each under `sources` by `source` (`project`, `dependency` or `installed_distribution`). A node type has its `type` (what `add_node` takes; an import's is led by its folder, `imports/<folder>/<module>:<class>`, and imported from there), `kind` (`process`, `function` or `shader`), `description`, `category` (the group its `@node.meta` files it under, a path where the group is inside another, as `Effects/Stylize`; null when it declares none, and for a Rust function node or an entry point whose declaration source does not show), and `inputs` and `outputs` where its declaration states them, each port's `family` null where source alone does not say. A graph generator has its `generator` (what `import_graph_generator` takes), its docstring as `description`, and its `promoted_config`. A module that does not parse is listed under its source's `unreadable_modules` with why and where; the rest still lists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::NodeType;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_project_views`: list the Tato Studio views a project brings.
pub struct ListProjectViews;

impl ControlOperation for ListProjectViews {
    type Request = ListProjectViewsRequest;
    type Result = ListProjectViewsToolResult;
    const METHOD_NAME: &'static str = "list_project_views";
    const ONE_LINE_DOC: &'static str = "List the Tato Studio views a project brings.";
    const AGENT_USAGE_NOTE: &'static str = "List every Tato Studio view a project brings — each folder of its own `views/`, then of each import's `imports/<folder>/views/` — read from the folder's `tatolab_view.json` without running anything in it. Each view under `views` has its `view_type`, `<project name>:<view folder>`, which is a node type of each stream the project runs when the view opens and its `placement` is `node`; the `project`, `project_name` and `import_folder` (null for the project's own) of the project bringing it; its `view_folder_in_the_project`, the folder relative to the project, as `views/<folder>` or `imports/<import>/views/<folder>`, which names its files to a project file read; its `title`, `description`, `icon`, `preview` and `placement` (`area` or `node`); its declared `inputs` and `outputs`, each a `name`, a `family`, a `description` and an Array port's `range`; and its `manifest` as written. A folder whose manifest is missing, is not JSON, or does not read against view contract 1.0 is listed with `opens` false and why in `not_opened_because`; the rest still lists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `write_studio_view_output`: write a value a Tato Studio view produced onto its node's output
/// port.
pub struct WriteStudioViewOutput;

impl ControlOperation for WriteStudioViewOutput {
    type Request = WriteStudioViewOutputRequest;
    type Result = WriteStudioViewOutputToolResult;
    const METHOD_NAME: &'static str = "write_studio_view_output";
    const ONE_LINE_DOC: &'static str =
        "Write a value a Tato Studio view produced onto its node's output port.";
    const AGENT_USAGE_NOTE: &'static str = "Hand a value a Tato Studio panel wrote to the view node `node` of the stream `stream`, which publishes it on its output `port` to every link and viewer reading that port, stamped with stream time now. A view node is a node of a view type, `<project name>:<view folder>`, which each view of the stream's project or of one of its imports is when it opens and its `placement` is `node`; its ports are the view's declared inputs and outputs, and a panel reads its inputs with `open_viewer` on the port linked into each. A write is live data: the last value written is what a reader keeps, many panels may write one node and the last write wins, and nothing is recorded in the graph document or the project's history. Array, Channels and Records outputs are written; an Image or Packets output is refused. Refused for a stream whose project is stopped, naming `start_playback`; a node of another type; a node that is not running; and a value its port's family does not take. A refusal because the node does not run comes with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::View;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `open_viewer`: open a viewer of a node's output port, held open for the connection that opened
/// it.
pub struct OpenViewer;

impl ControlOperation for OpenViewer {
    type Request = OpenViewerRequest;
    type Result = OpenViewerToolResult;
    const METHOD_NAME: &'static str = "open_viewer";
    const ONE_LINE_DOC: &'static str =
        "Open a viewer of a node's output port, held open for the connection that opened it.";
    const AGENT_USAGE_NOTE: &'static str = "Open a viewer of any output port of a running node, adding no node and changing no graph: the runtime holds the port's channel open and answers the `viewer` id, the `iceoryx2_domain_root`, the `channel` whose reserved subscriber slot the viewer takes from its own process, and the `wake_service` the node notifies on each write, whose one listener the viewer takes so it waits for a message rather than reading on a timer. The answer's `viewer_reads` says which messages the viewer reads: `every_message` for a Channels port, in order as far back as the channel holds them for it, so a viewer can play the sound, and `latest_message` for every other family, skipping what it falls behind on; either way the channel overwrites what the viewer has not read, so it never slows the stream; an Image message names a pooled frame rather than carrying its pixels, and the viewer imports that frame's surface from the `surface_share_address` the way a helper process does. The viewer stays open until `close_viewer` or the end of the connection that opened it, so it is opened only over a connection that stays open: `/mcp/stdio` or `/control/ws`. Refused for a node that is not running, with `structuredContent.refusal.not_running_because` naming why: `playback_stopped`, `playback_paused` (a node added while its project is paused), `preparing` or `node_failed`, the runtime's words beside it in `runtime_words`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `close_viewer`: close a viewer this connection opened.
pub struct CloseViewer;

impl ControlOperation for CloseViewer {
    type Request = CloseViewerRequest;
    type Result = CloseViewerToolResult;
    const METHOD_NAME: &'static str = "close_viewer";
    const ONE_LINE_DOC: &'static str = "Close a viewer this connection opened.";
    const AGENT_USAGE_NOTE: &'static str =
        "Close a viewer this connection opened, letting go of its port's channel.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}
