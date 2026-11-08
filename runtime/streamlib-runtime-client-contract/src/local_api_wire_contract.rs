// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The names a runtime's local API speaks on its socket and its loopback HTTP listener on its
//! port, and the shape of each result a client parses, written once for the runtime that serves
//! them and every client that calls them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The route MCP's Streamable HTTP transport is served on.
pub const MCP_STREAMABLE_HTTP_ROUTE_PATH: &str = "/mcp";

/// The request target a client upgrades to MCP's stdio framing on.
pub const MCP_STDIO_UPGRADE_REQUEST_TARGET: &str = "/mcp/stdio";

/// The `Upgrade` protocol token [`MCP_STDIO_UPGRADE_REQUEST_TARGET`] switches to, matched
/// case-insensitively.
pub const MCP_STDIO_UPGRADE_PROTOCOL_TOKEN: &str = "mcp-stdio";

/// The `logs` tool's result: one project's log records after the cursor asked for, read from
/// the project's log on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogsToolResult {
    /// The project whose log these are.
    pub project_directory: PathBuf,
    /// The records after `after` that match the call's `stream` and `action`, oldest first, each
    /// as the project's JSONL log holds it.
    pub records: Vec<serde_json::Value>,
    /// The opaque cursor the next call passes as `after` to read on from this page.
    pub next_after: String,
    /// Whether retention deleted the segment `after` named, so the page resumes at the oldest
    /// record held.
    pub records_no_longer_held: bool,
}

/// Where a stream lives, as the local API reports it: its project directory, resolved, its
/// project's UUID and its stream asset's UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamLocationOnTheWire {
    /// The project directory's absolute path, symlinks resolved, as of the stream's last load.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: uuid::Uuid,
    /// The stream asset's UUID, from its graph document.
    pub stream_asset: uuid::Uuid,
}

/// The `stop_stream` tool's result: the stop queued on its stream's project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopStreamToolResult {
    /// The queued stop, as `describe_stream_action` takes it.
    pub action: String,
    /// The stream's id.
    pub stream: String,
}

/// The `start_stream` tool's result: the start queued on its stream's project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartStreamToolResult {
    /// The queued start, as `describe_stream_action` takes it.
    pub action: String,
    /// The stream's id.
    pub stream: String,
}

/// The `remove_project` tool's result: the removal of the whole project queued on its queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveProjectToolResult {
    /// The queued removal, as `describe_stream_action` takes it.
    pub action: String,
    /// The project whose streams it unloads and forgets, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `rename_stream` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameStreamToolResult {
    /// The stream's id, which a rename leaves as it was.
    pub stream: String,
    /// The label the stream is shown under now.
    pub display_name: String,
}

/// The `list_streams` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStreamsToolResult {
    /// Every stream the runtime holds.
    pub streams: Vec<ListStreamsToolResultStream>,
}

/// One stream of a [`ListStreamsToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListStreamsToolResultStream {
    /// The stream's id; every tool naming the stream takes it.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives; `null` for a stream a library host loaded, and for a
    /// record an earlier runtime wrote.
    pub location: Option<StreamLocationOnTheWire>,
    /// Whether it is running, stopped or failed.
    pub state: ListedStreamState,
    /// How many nodes its loaded graph holds; `null` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `null` when it is not.
    pub failed_because: Option<String>,
    /// Each capture device a node of the loaded stream refused because nobody has asked the user
    /// for its access — the runtime re-loaded it on its own at its start — once each; a
    /// `stop_stream` then `start_stream` of it asks. Empty when it is not loaded.
    pub capture_devices_whose_access_was_never_asked_for: Vec<CaptureDeviceOnTheWire>,
    /// The stream action preparing it, as `describe_stream_action` takes it; `null` unless it
    /// is `preparing`.
    pub action: Option<String>,
    /// The step that action is on; `null` while it waits in its project's queue, and unless the
    /// stream is `preparing`.
    pub step: Option<StreamActionStepOnTheWire>,
}

/// A capture device the platform gates behind a privacy prompt, as `list_streams` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureDeviceOnTheWire {
    /// The camera.
    Camera,
    /// The microphone.
    Microphone,
}

impl CaptureDeviceOnTheWire {
    /// The device as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            CaptureDeviceOnTheWire::Camera => "camera",
            CaptureDeviceOnTheWire::Microphone => "microphone",
        }
    }
}

/// The state of one stream `list_streams` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListedStreamState {
    /// Loaded; or recorded, neither stopped nor failed, and re-loaded at the runtime's next
    /// start.
    Running,
    /// Stopped by its owner, and left unloaded until `start_stream`.
    Stopped,
    /// Skipped at the runtime's start until `start_stream` retries it.
    Failed,
    /// A stream action of its project that loads it is queued or under way on it.
    Preparing,
    /// A stream action of its project that unloads it is queued or under way on it.
    Stopping,
}

impl ListedStreamState {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            ListedStreamState::Running => "running",
            ListedStreamState::Stopped => "stopped",
            ListedStreamState::Failed => "failed",
            ListedStreamState::Preparing => "preparing",
            ListedStreamState::Stopping => "stopping",
        }
    }
}

impl std::fmt::Display for ListedStreamState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

/// The `run_project` tool's result: the stream action it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunProjectToolResult {
    /// The queued action's id, which `describe_stream_action` and `logs` take.
    pub action: String,
    /// The project whose queue holds it, symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `create_project` tool's result: the load it queued of the project it created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectToolResult {
    /// The queued action's id, which `describe_stream_action` and `logs` take.
    pub action: String,
    /// The id the scaffold's `stream:main` loads as.
    pub stream: String,
    /// The project's directory, symlinks resolved.
    pub project_directory: PathBuf,
}

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionKindOnTheWire {
    /// Loads every graph document of a project.
    Load,
    /// Loads and starts one stopped or failed stream.
    Start,
    /// Unloads one stream and records it stopped.
    Stop,
    /// Unloads every stream of a project and forgets every record of it.
    Remove,
    /// Re-loads, at the runtime's start, a project's streams neither stopped nor failed.
    ReloadAtStart,
}

/// Where a stream action is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionStateOnTheWire {
    /// Waiting behind its project's earlier actions.
    Queued,
    /// Running one of its steps.
    InProgress,
    /// Finished; each stream it took in is running or failed.
    Done,
    /// Ended without taking in the project, for `failed_because`.
    Failed,
    /// Ended early by `cancel_stream_action`: dropped from its queue, or stopped at the step it
    /// was on.
    Cancelled,
}

impl StreamActionStateOnTheWire {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            StreamActionStateOnTheWire::Queued => "queued",
            StreamActionStateOnTheWire::InProgress => "in_progress",
            StreamActionStateOnTheWire::Done => "done",
            StreamActionStateOnTheWire::Failed => "failed",
            StreamActionStateOnTheWire::Cancelled => "cancelled",
        }
    }
}

/// The step a running stream action is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionStepOnTheWire {
    /// Making the project's `.venv` with the runtime's uv.
    MakingTheVenv,
    /// Reading the project's file and each stream asset's graph document.
    ReadingTheGraphDocuments,
    /// Loading and starting the streams the graph documents hold.
    Starting,
    /// Unloading the streams it works on.
    Stopping,
}

impl StreamActionStepOnTheWire {
    /// The step as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            StreamActionStepOnTheWire::MakingTheVenv => "making_the_venv",
            StreamActionStepOnTheWire::ReadingTheGraphDocuments => "reading_the_graph_documents",
            StreamActionStepOnTheWire::Starting => "starting",
            StreamActionStepOnTheWire::Stopping => "stopping",
        }
    }
}

/// The `describe_stream_action` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// The project it works on.
    pub project_directory: PathBuf,
    /// What it does.
    pub kind: StreamActionKindOnTheWire,
    /// Where it is in its life.
    pub state: StreamActionStateOnTheWire,
    /// The step it is on while `in_progress`; `null` otherwise.
    pub step: Option<StreamActionStepOnTheWire>,
    /// The streams it works on, once its project's graph documents named them.
    pub streams: Vec<DescribeStreamActionToolResultStream>,
    /// Why it failed; `null` unless it did.
    pub failed_because: Option<String>,
}

/// The `cancel_stream_action` tool's result: the action's state once the cancel was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// `cancelled` for an action dropped from its queue; `in_progress` for a running one, which
    /// ends `cancelled` once it stops at the step it is on.
    pub state: StreamActionStateOnTheWire,
}

/// One stream of a [`DescribeStreamActionToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeStreamActionToolResultStream {
    /// The stream's id.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// `running` or `failed` once the action took it in; `null` before.
    pub state: Option<ListedStreamState>,
}

/// The `open_viewer` tool's result: what a viewer subscribes to, from its own process, to read
/// the latest message an output port's node wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenViewerToolResult {
    /// The viewer's id, which `close_viewer` takes: `<stream>/<node_id>/<port>`.
    pub viewer: String,
    /// The stream's id.
    pub stream: String,
    /// The node's id, as `graph` lists it.
    pub node_id: String,
    /// The node's name: its label, not its identity.
    pub node_name: String,
    /// The output port's name.
    pub port: String,
    /// The root of the iceoryx2 domain the port's channel lives in.
    pub iceoryx2_domain_root: String,
    /// The port's channel: the iceoryx2 service whose reserved subscriber slot the viewer takes.
    pub channel: String,
    /// Where the viewer imports an Image frame's surface: the surface-sharing socket's path on
    /// Linux, the surface-sharing Mach service's name on macOS. Absent on macOS when the open
    /// named no `viewer_process_id`, since the service admits only a named process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_share_address: Option<String>,
}

/// The `close_viewer` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseViewerToolResult {
    /// The viewer closed.
    pub viewer: String,
}

/// The `set_stream_clock_scale` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetStreamClockScaleToolResult {
    /// The stream's id.
    pub stream: String,
    /// Stream seconds per machine second from now on.
    pub scale: f64,
}

/// The `describe_runtime` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeRuntimeToolResult {
    /// The runtime's crate version.
    pub version: String,
    /// `<crate version>+<git sha>.<per-build nonce>`, the build the helper check compares.
    pub build_id: String,
    /// Monotonic milliseconds since this runtime's engine started; a restart resets it.
    pub up_for_ms: u64,
    /// The folder the runtime's own log is written to, absolute for `tatolabd`; `None` when it
    /// keeps none.
    pub runtime_log_directory: Option<PathBuf>,
}

/// The port the loopback HTTP listener binds on `127.0.0.1` when nothing names another; on macOS
/// its bind is the machine runtime lock.
pub const LOOPBACK_HTTP_LISTENER_DEFAULT_PORT: u16 = 4446;

/// The loopback HTTP listener's route answering the pid of the process serving it, as a
/// [`MachineRuntimeLockHolderPidAnswer`].
pub const MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH: &str = "/machine-runtime-lock-holder";

/// `GET` [`MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH`]'s answer: the serving process's pid, which a
/// prober names through the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRuntimeLockHolderPidAnswer {
    /// The pid of the process serving the loopback HTTP listener.
    pub pid: i32,
}

/// One asset of a project as the asset tools report it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectAssetOnTheWire {
    /// The asset's UUID, which the runtime made at import and which a rename or move keeps.
    pub asset: String,
    /// `stream`, `media`, `scene` or `data`.
    pub kind: String,
    /// Its path in the project, `/`-separated, relative to the project directory.
    pub path: String,
    /// The SHA-256 its bytes are kept under in the project's content store, in lowercase hex.
    pub content_sha256: String,
    /// The size of its bytes.
    pub content_byte_len: u64,
    /// The settings its import took.
    pub import_settings: serde_json::Map<String, serde_json::Value>,
}

/// The `list_assets` tool's result: the project's assets, ordered by path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListAssetsToolResult {
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: String,
    /// The project's name.
    pub project_name: String,
    /// The assets, narrowed to one kind when the call named one.
    pub assets: Vec<ProjectAssetOnTheWire>,
    /// The project's change feed: the resource a client reads for its newest transactions and
    /// names in `subscriptions/listen` to be told of each new one.
    pub transactions_resource_uri: String,
}

/// One transaction a project's change feed holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectTransactionOnTheWire {
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
}

/// What a project's transactions resource holds: its newest transactions, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectTransactionsResourceContents {
    /// The project's UUID.
    pub project: String,
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// Its newest transactions, newest first.
    pub transactions: Vec<ProjectTransactionOnTheWire>,
}

/// The `describe_asset` tool's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DescribeAssetToolResult {
    /// The asset.
    pub asset: ProjectAssetOnTheWire,
    /// Whether the project's content store holds its bytes.
    pub content_held: bool,
}

/// The result of an asset tool that changed one asset and committed the change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetTransactionToolResult {
    /// The asset as the transaction left it; a deleted asset as it was.
    pub asset: ProjectAssetOnTheWire,
    /// The transaction's UUID.
    pub transaction: String,
    /// The commit on the project history's `main` that records it.
    pub commit: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_description_names_its_log_folder_or_null_when_it_keeps_none() {
        let described = |runtime_log_directory: Option<&str>| DescribeRuntimeToolResult {
            version: "0.55.1".to_owned(),
            build_id: "0.55.1+0123abc.9f8e7d".to_owned(),
            up_for_ms: 266_400_000,
            runtime_log_directory: runtime_log_directory.map(PathBuf::from),
        };
        for (description, expected_wire_text) in [
            (
                described(Some("/home/u/.local/state/tatolab/logs")),
                r#"{"version":"0.55.1","build_id":"0.55.1+0123abc.9f8e7d","up_for_ms":266400000,"runtime_log_directory":"/home/u/.local/state/tatolab/logs"}"#,
            ),
            (
                described(None),
                r#"{"version":"0.55.1","build_id":"0.55.1+0123abc.9f8e7d","up_for_ms":266400000,"runtime_log_directory":null}"#,
            ),
        ] {
            let wire_text = serde_json::to_string(&description).unwrap();
            assert_eq!(wire_text, expected_wire_text);
            assert_eq!(
                serde_json::from_str::<DescribeRuntimeToolResult>(&wire_text).unwrap(),
                description
            );
        }
    }

    #[test]
    fn the_machine_runtime_lock_holders_answer_is_its_pid_alone() {
        let answer = MachineRuntimeLockHolderPidAnswer { pid: 4242 };
        let wire_text = serde_json::to_string(&answer).unwrap();
        assert_eq!(wire_text, r#"{"pid":4242}"#);
        assert_eq!(
            serde_json::from_str::<MachineRuntimeLockHolderPidAnswer>(&wire_text).unwrap(),
            answer
        );
    }

    /// Each stream-action result in the order and spelling the runtime writes its keys.
    #[test]
    fn every_stream_action_result_serializes_its_keys_in_wire_order() {
        fn wire_text(result: &impl Serialize) -> String {
            serde_json::to_string(result).unwrap()
        }

        assert_eq!(
            wire_text(&StartStreamToolResult {
                action: "R-7".to_owned(),
                stream: "camera".to_owned(),
            }),
            r#"{"action":"R-7","stream":"camera"}"#
        );
        assert_eq!(
            wire_text(&StopStreamToolResult {
                action: "R-8".to_owned(),
                stream: "camera".to_owned(),
            }),
            r#"{"action":"R-8","stream":"camera"}"#
        );
        assert_eq!(
            wire_text(&RemoveProjectToolResult {
                action: "R-9".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
            }),
            r#"{"action":"R-9","project_directory":"/srv/project"}"#
        );
        assert_eq!(
            wire_text(&ListStreamsToolResult {
                streams: vec![
                    ListStreamsToolResultStream {
                        stream: "cam-main-0a1b2c".to_owned(),
                        display_name: "Desk camera".to_owned(),
                        location: Some(StreamLocationOnTheWire {
                            project_directory: PathBuf::from("/srv/cam"),
                            project: uuid::Uuid::from_u128(1),
                            stream_asset: uuid::Uuid::from_u128(2),
                        }),
                        state: ListedStreamState::Running,
                        node_count: Some(4),
                        failed_because: None,
                        capture_devices_whose_access_was_never_asked_for: vec![
                            CaptureDeviceOnTheWire::Camera,
                            CaptureDeviceOnTheWire::Microphone,
                        ],
                        action: None,
                        step: None,
                    },
                    ListStreamsToolResultStream {
                        stream: "main".to_owned(),
                        display_name: "main".to_owned(),
                        location: None,
                        state: ListedStreamState::Failed,
                        node_count: None,
                        failed_because: Some("an earlier runtime recorded it".to_owned()),
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        action: None,
                        step: None,
                    },
                    ListStreamsToolResultStream {
                        stream: "desk-0a1b2c".to_owned(),
                        display_name: "desk".to_owned(),
                        location: Some(StreamLocationOnTheWire {
                            project_directory: PathBuf::from("/srv/desk"),
                            project: uuid::Uuid::from_u128(3),
                            stream_asset: uuid::Uuid::from_u128(4),
                        }),
                        state: ListedStreamState::Preparing,
                        node_count: None,
                        failed_because: None,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        action: Some("Rabc-3".to_owned()),
                        step: Some(StreamActionStepOnTheWire::MakingTheVenv),
                    },
                ],
            }),
            r#"{"streams":[{"stream":"cam-main-0a1b2c","display_name":"Desk camera","location":{"project_directory":"/srv/cam","project":"00000000-0000-0000-0000-000000000001","stream_asset":"00000000-0000-0000-0000-000000000002"},"state":"running","node_count":4,"failed_because":null,"capture_devices_whose_access_was_never_asked_for":["camera","microphone"],"action":null,"step":null},{"stream":"main","display_name":"main","location":null,"state":"failed","node_count":null,"failed_because":"an earlier runtime recorded it","capture_devices_whose_access_was_never_asked_for":[],"action":null,"step":null},{"stream":"desk-0a1b2c","display_name":"desk","location":{"project_directory":"/srv/desk","project":"00000000-0000-0000-0000-000000000003","stream_asset":"00000000-0000-0000-0000-000000000004"},"state":"preparing","node_count":null,"failed_because":null,"capture_devices_whose_access_was_never_asked_for":[],"action":"Rabc-3","step":"making_the_venv"}]}"#
        );
        assert_eq!(
            wire_text(&RunProjectToolResult {
                action: "Rabc-3".to_owned(),
                project_directory: PathBuf::from("/srv/desk"),
            }),
            r#"{"action":"Rabc-3","project_directory":"/srv/desk"}"#
        );
        assert_eq!(
            wire_text(&CreateProjectToolResult {
                action: "Rabc-4".to_owned(),
                stream: "brisk-otter-main-0a1b2c".to_owned(),
                project_directory: PathBuf::from("/home/me/streams/brisk-otter"),
            }),
            r#"{"action":"Rabc-4","stream":"brisk-otter-main-0a1b2c","project_directory":"/home/me/streams/brisk-otter"}"#
        );
        assert_eq!(
            wire_text(&DescribeStreamActionToolResult {
                action: "Rabc-3".to_owned(),
                project_directory: PathBuf::from("/srv/desk"),
                kind: StreamActionKindOnTheWire::Load,
                state: StreamActionStateOnTheWire::InProgress,
                step: Some(StreamActionStepOnTheWire::Starting),
                streams: vec![DescribeStreamActionToolResultStream {
                    stream: "desk-main-0a1b2c".to_owned(),
                    display_name: "main".to_owned(),
                    state: None,
                }],
                failed_because: None,
            }),
            r#"{"action":"Rabc-3","project_directory":"/srv/desk","kind":"load","state":"in_progress","step":"starting","streams":[{"stream":"desk-main-0a1b2c","display_name":"main","state":null}],"failed_because":null}"#
        );
        assert_eq!(
            wire_text(&CancelStreamActionToolResult {
                action: "Rabc-3".to_owned(),
                state: StreamActionStateOnTheWire::Cancelled,
            }),
            r#"{"action":"Rabc-3","state":"cancelled"}"#
        );
        assert_eq!(
            wire_text(&LogsToolResult {
                project_directory: PathBuf::from("/srv/project"),
                records: vec![serde_json::json!({"message": "hello"})],
                next_after: "2:17".to_owned(),
                records_no_longer_held: false,
            }),
            r#"{"project_directory":"/srv/project","records":[{"message":"hello"}],"next_after":"2:17","records_no_longer_held":false}"#
        );
    }

    /// The `Display` spelling a client prints is the spelling the wire carries.
    #[test]
    fn each_state_displays_as_the_wire_spells_it() {
        for state in [
            ListedStreamState::Running,
            ListedStreamState::Stopped,
            ListedStreamState::Failed,
            ListedStreamState::Preparing,
            ListedStreamState::Stopping,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.to_string())
            );
        }
    }

    /// Asset results carry the UUID, kind, path and content hash under the names clients read.
    #[test]
    fn an_asset_transaction_result_spells_its_fields_as_the_wire_carries_them() {
        let result = AssetTransactionToolResult {
            asset: ProjectAssetOnTheWire {
                asset: "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11".to_owned(),
                kind: "media".to_owned(),
                path: "media/intro.mp4".to_owned(),
                content_sha256: "ab".repeat(32),
                content_byte_len: 12,
                import_settings: serde_json::Map::new(),
            },
            transaction: "0b6f4e1d-3c55-4b0e-8d1e-6e2f0f9b7c21".to_owned(),
            commit: "c".repeat(40),
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            serde_json::json!({
                "asset": {
                    "asset": "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11",
                    "kind": "media",
                    "path": "media/intro.mp4",
                    "content_sha256": "ab".repeat(32),
                    "content_byte_len": 12,
                    "import_settings": {},
                },
                "transaction": "0b6f4e1d-3c55-4b0e-8d1e-6e2f0f9b7c21",
                "commit": "c".repeat(40),
            })
        );
    }
}
