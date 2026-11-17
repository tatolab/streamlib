// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DescribeRuntimeToolResult, ListStreamsToolResult, LogsToolResult,
    ProjectTransactionsResourceContents,
};

use super::{EVERY_SURFACE, EVERY_SURFACE_BUT_THE_AGENT_SURFACE};
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExposure, ControlOperationGroup, ControlOperationStability,
};
use crate::control_operation_requests::{
    DescribeRuntimeRequest, DiscoverRequest, GraphRequest, ListStreamsRequest, LogsRequest,
    OneProjectByItsDirectoryRequest, SubscribeInputValueChangesRequest,
    SubscribeProjectChangesRequest, SubscribeProjectLogRecordsRequest,
    SubscribeStreamActionChangesRequest, SubscribeStreamGraphChangesRequest,
};
use crate::control_operation_results::{
    DiscoverResult, SubscribeInputValueChangesResult, SubscribeProjectChangesResult,
    SubscribeProjectLogRecordsResult, SubscribeStreamActionChangesResult,
    SubscribeStreamGraphChangesResult,
};

/// `discover`: the contract a runtime serves, which a client checks before any other call.
pub struct Discover;

impl ControlOperation for Discover {
    type Request = DiscoverRequest;
    type Result = DiscoverResult;
    const METHOD_NAME: &'static str = "discover";
    const ONE_LINE_DOC: &'static str = "Answer the contract version this runtime serves, its version and build id, and every method it serves.";
    const AGENT_USAGE_NOTE: &'static str = "Answer `contract_version`, the version of the contract this runtime serves, its `runtime_version` and `build_id`, and `operations`, every method it serves. A client sends the `contract_version` it was built against; another version is refused naming both, and the client is not served.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_project_changes`: the connection is sent each change on some projects' feeds.
pub struct SubscribeProjectChanges;

impl ControlOperation for SubscribeProjectChanges {
    type Request = SubscribeProjectChangesRequest;
    type Result = SubscribeProjectChangesResult;
    const METHOD_NAME: &'static str = "subscribe_project_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `project_changed` notification for each change on the named projects' feeds.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the change feeds of `projects`, by UUID: from then on, each transaction applied to one, each action the runtime refuses on one, each media facts read it finishes, each making current of its `.venv` (`venv_made_current`) and each burst of files changed in its folder, an editor's saves among them, named under `files`, is sent as a `project_changed` notification, whoever made it, until the connection closes. A connection that falls behind, or a project whose folder changed in ways the OS did not name, is sent `missed`, and the client reads the change feed and lists the project's files and node types again. Served only on a connection that stays open; refused for an empty `projects`. Answers every project the connection is subscribed to.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_project_log_records`: the connection is told each time some projects' logs have new
/// records.
pub struct SubscribeProjectLogRecords;

impl ControlOperation for SubscribeProjectLogRecords {
    type Request = SubscribeProjectLogRecordsRequest;
    type Result = SubscribeProjectLogRecordsResult;
    const METHOD_NAME: &'static str = "subscribe_project_log_records";
    const ONE_LINE_DOC: &'static str = "Send this connection each new record of the named projects' logs as `project_log_records_written` notifications.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the logs of the projects in `project_directories`, each an absolute path: from then on, each time the runtime writes new records to one's log, which it does at most every 100 ms, `project_log_records_written` carries them, at most 500 to a notification, naming its `project_directory` as the connection spelled it. Each notification's `after` is the previous one's `next_after`, starting at the log's end as the project is subscribed; a client that read the log with `logs` to another cursor reads `logs` from its cursor `until` the notification's `next_after` instead, and appends the notifications after that. Served only on a connection that stays open; refused for an empty `project_directories`, or one that is relative or names no folder on this machine. Answers every project the connection is subscribed to, by its directory as the connection spelled it.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `read_project_changes`: read a project's change feed as it stands.
pub struct ReadProjectChanges;

impl ControlOperation for ReadProjectChanges {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectTransactionsResourceContents;
    const METHOD_NAME: &'static str = "read_project_changes";
    const ONE_LINE_DOC: &'static str = "Read a project's change feed as it stands: its newest transactions, refused actions and media facts reads.";
    const AGENT_USAGE_NOTE: &'static str = "Read the change feed of the project in `project_directory`, opening it as `list_assets` does, as the resource `list_assets` names as `transactions_resource_uri` renders it: the project's UUID as `project`, its `project_directory`, and under `transactions` its newest 64 transactions, the actions refused and the assets whose media facts were read since the runtime started, newest first, each tagged by `status`. `subscribe_project_changes` sends a `project_changed` notification for each change after it, and `missed` when changes went unsent, after which the feed is read again and the project's files listed again. Refused for a folder with no `tatolab-project.json`.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_stream_graph_changes`: the connection is sent each change in some loaded streams'
/// live graphs.
pub struct SubscribeStreamGraphChanges;

impl ControlOperation for SubscribeStreamGraphChanges {
    type Request = SubscribeStreamGraphChangesRequest;
    type Result = SubscribeStreamGraphChangesResult;
    const METHOD_NAME: &'static str = "subscribe_stream_graph_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `stream_graph_changed` notification for each change in the named loaded streams' live graphs.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the live graphs of `streams`, by stream id, each loaded: from then on, each edit of one's graph is sent as a `stream_graph_changed` notification with `change` `edited`, each change of a node's state with `node_state_changed`, its `node` by graph `id` and its `node_state`, and each start, stop, pause or resume of its playback, or failure of one, with `playback_state_changed`, until the connection closes. A connection that falls behind, and a stream loaded again or unloaded, is sent `missed`, and the client reads the stream's `graph` again; the subscription follows the stream id across a reload. Served only on a connection that stays open; refused for an empty `streams` or a stream not loaded. Answers every stream the connection is subscribed to.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_input_value_changes`: the connection is sent each change of the value an input of
/// some loaded streams reads.
pub struct SubscribeInputValueChanges;

impl ControlOperation for SubscribeInputValueChanges {
    type Request = SubscribeInputValueChangesRequest;
    type Result = SubscribeInputValueChangesResult;
    const METHOD_NAME: &'static str = "subscribe_input_value_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection an `input_value_changed` notification for each change of the value an input of the named loaded streams reads.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to the input values of `streams`, by stream id, each loaded: from then on, each change of the value an input reads while unlinked, set live or saved, is sent as an `input_value_changed` notification naming its `node` and `input`, with the `value` and `value_comes_from` `graph` renders for that input, until the connection closes; the caller of a set is sent it too. Changes are sent at most once each 33 ms for one input, the newest kept, and the last value is always sent. A connection that follows a stream's graph and has not subscribed here is sent `stream_graph_changed` `edited` for a value change instead, at most once each 500 ms for each stream; one that subscribed is sent no `edited` for a value set live, and `stream_graph_changed` `missed` when it may have missed changes, so it reads `graph` again. Served only on a connection that stays open; refused for an empty `streams` or a stream not loaded. Answers every stream the connection is subscribed to.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `subscribe_stream_action_changes`: the connection is sent each change of a stream action.
pub struct SubscribeStreamActionChanges;

impl ControlOperation for SubscribeStreamActionChanges {
    type Request = SubscribeStreamActionChangesRequest;
    type Result = SubscribeStreamActionChangesResult;
    const METHOD_NAME: &'static str = "subscribe_stream_action_changes";
    const ONE_LINE_DOC: &'static str = "Send this connection a `stream_action_changed` notification each time a stream action changes.";
    const AGENT_USAGE_NOTE: &'static str = "Subscribe the connection to every stream action: from then on, each time one is queued, moves on or ends, its id is sent as a `stream_action_changed` notification with `change` `changed`, and `describe_stream_action` describes it, until the connection closes. A connection that falls behind is sent `missed`, and the client describes each action it follows again. Served only on a connection that stays open. Answers `actions_under_way`, the id of each action queued or running as the subscription began.";
    const SINCE_CONTRACT_VERSION: u32 = 2;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE_BUT_THE_AGENT_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `describe_runtime`: this runtime's version, build and uptime.
pub struct DescribeRuntime;

impl ControlOperation for DescribeRuntime {
    type Request = DescribeRuntimeRequest;
    type Result = DescribeRuntimeToolResult;
    const METHOD_NAME: &'static str = "describe_runtime";
    const ONE_LINE_DOC: &'static str = "Describe this runtime: its version, its build id, how long it has been up and where it logs.";
    const AGENT_USAGE_NOTE: &'static str = "Describe this runtime: its crate `version`, its `build_id` (`<version>+<git sha>.<per-build nonce>`, telling two builds of one version apart), `up_for_ms`, the monotonic milliseconds since its engine started — a restart resets it — and `runtime_log_directory`, the folder its own log is written to, `null` when it keeps none. Names no stream.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `list_streams`: every stream the runtime holds, with its state.
pub struct ListStreams;

impl ControlOperation for ListStreams {
    type Request = ListStreamsRequest;
    type Result = ListStreamsToolResult;
    const METHOD_NAME: &'static str = "list_streams";
    const ONE_LINE_DOC: &'static str =
        "List every stream the runtime holds, by stream id, with its location and state.";
    const AGENT_USAGE_NOTE: &'static str = "List every stream the runtime holds: its stream id as `stream`, its `display_name`, its `location` — its project directory, its project's UUID as `project` and its stream asset's UUID as `stream_asset`, `null` for a record an earlier runtime wrote — and its state: `running` (playing, or recorded playing and re-loaded playing at the runtime's next start), `stopped` (open with its playback stopped, its graph loaded to read and edit while its node count is set, until `start_playback`), `paused` (open with its playback paused by `pause_playback`, until `resume_playback`), `closed` (its project closed by `close_project`, not loaded, until `open_project`), `preparing` (an action that loads it — an `open_project`, `reload_project` or `start_playback` — is queued or under way on it, named by `action`, on its `step`: `reading_the_graph_documents`, `making_the_venv` or `starting`, `null` while it waits), `stopping` (a `stop_playback`, `close_project` or `remove_project` of its project is queued or under way on it, named by `action`, on the step `stopping`, `null` while it waits), or `failed` (its code, its venv, its load or its start failed, and held stopped from its document when that reads, its graph there to read and edit, until a play or a `reload_project` retries it; implicated in the runtime's last two crashes in a row, and held at a start from its document with none of its node code read until a `reload_project` of its project tries it; or recorded by an earlier runtime, never loaded, with `failed_because` saying to run it again and `remove_project` forgetting it), with the node count of its loaded graph (`null` when it is not loaded), `failed_because`, why a failed stream failed (`null` otherwise), and `capture_devices_whose_access_was_never_asked_for`, each of `camera` and `microphone` that a node of the loaded stream refused because nobody has asked the user for its access, as when the runtime re-loaded the stream on its own at its start (empty otherwise): a `close_project` then `open_project` of its project asks.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `graph`: a loaded stream's live graph, or every loaded stream's.
pub struct Graph;

impl ControlOperation for Graph {
    type Request = GraphRequest;
    type Result = Value;
    const METHOD_NAME: &'static str = "graph";
    const ONE_LINE_DOC: &'static str = "Export a loaded stream's live graph, or every loaded stream's, with each node's and link's state.";
    const AGENT_USAGE_NOTE: &'static str = "Export a loaded stream's current graph as JSON: the stream it was loaded as, its nodes by name with their types, config and ports, its links by node and port, with each node's and link's live state and counters beside them, and this machine's name (`machine`), the first part of `<machine>/<stream id>/<node>/<port>`. A fragment — a graph generator's stored output — renders as one node of `kind` `fragment` whose ports are its Graph Input and Output nodes' and whose `fragment` holds its own nodes and links, a fragment inside it rendered the same way; its nodes run under their path of names, `look/soft/blur`. Without `stream`, `{machine, streams}` holds every loaded stream's graph.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `logs`: read a project's log records after a cursor, oldest first.
pub struct Logs;

impl ControlOperation for Logs {
    type Request = LogsRequest;
    type Result = LogsToolResult;
    const METHOD_NAME: &'static str = "logs";
    const ONE_LINE_DOC: &'static str = "Read a project's log records after a cursor, oldest first.";
    const AGENT_USAGE_NOTE: &'static str = "Read a project's log records after the cursor `after`, oldest first, each as the project's JSONL log on disk holds it: every stream of the project and every load of it writes that one log, each record stamped with its `stream` — none for a load's own lines, uv's among them — across the runtime's runs, whether the stream is still recorded or not. `stream` narrows to one stream's records, and `action` to the records of one stream action — every record its load wrote, uv's among them, and the records of the streams it started. Pass the result's `next_after` back as `after` to read on; `records_no_longer_held` is true when retention deleted the segment `after` named, so the page resumes at the oldest record held. A project that has logged nothing answers no records.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}
