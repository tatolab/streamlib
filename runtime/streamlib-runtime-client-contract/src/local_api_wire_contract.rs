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

/// Where a stream's code lives, as the local API reports it: its project directory, resolved,
/// and its stream function as `module:qualname`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamLocationOnTheWire {
    /// The project directory's absolute path, symlinks resolved.
    pub project_directory: PathBuf,
    /// The stream function as `module:qualname`.
    pub stream_function: String,
}

/// The `run_project` tool's result: the load it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunProjectToolResult {
    /// The queued load's action id, as `describe_stream_action` and `logs` take it.
    pub action: String,
    /// The project, its symlinks resolved.
    pub project_directory: PathBuf,
}

/// The `create_project` tool's result: the load of the new project it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectToolResult {
    /// The queued load's action id; its first step writes the project's files.
    pub action: String,
    /// The id of the stream the new project's `stream.py` defines.
    pub stream: String,
    /// The new project's directory.
    pub project_directory: PathBuf,
}

/// The `stop_stream` tool's result: the stop it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopStreamToolResult {
    /// The queued stop's action id.
    pub action: String,
    /// The stream's id.
    pub stream: String,
}

/// The `start_stream` tool's result: the start it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartStreamToolResult {
    /// The queued start's action id.
    pub action: String,
    /// The stream's id.
    pub stream: String,
}

/// The `remove_project` tool's result: the remove it queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveProjectToolResult {
    /// The queued remove's action id.
    pub action: String,
    /// The project, its symlinks resolved.
    pub project_directory: PathBuf,
}

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionKindOnTheWire {
    /// Loads every `@stream` of a project, after writing a created project's files.
    Load,
    /// Starts a recorded stream.
    Start,
    /// Stops a stream.
    Stop,
    /// Unloads every stream of a project and forgets its records.
    Remove,
    /// Re-loads a project's recorded streams at the runtime's start.
    ReloadAtStart,
}

/// Where a stream action is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionStateOnTheWire {
    /// Waiting for the actions ahead of it in its project's queue.
    Queued,
    /// Running.
    InProgress,
    /// Ended as asked.
    Done,
    /// Ended early, with `failed_because`.
    Failed,
    /// Ended by `cancel_stream_action`.
    Cancelled,
}

impl StreamActionStateOnTheWire {
    /// Whether the action has ended.
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Queued | Self::InProgress)
    }
}

/// The step a running stream action is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamActionStepOnTheWire {
    /// Writing a created project's files.
    WritingTheScaffold,
    /// Making the project's `.venv` with the uv the runtime carries.
    MakingTheVenv,
    /// Compiling the project's `@stream` functions in its own interpreter.
    Compiling,
    /// Loading and starting the compiled streams.
    Starting,
    /// Unloading streams.
    Stopping,
}

impl StreamActionKindOnTheWire {
    /// The kind as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Remove => "remove",
            Self::ReloadAtStart => "reload_at_start",
        }
    }
}

impl std::fmt::Display for StreamActionKindOnTheWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

impl StreamActionStateOnTheWire {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for StreamActionStateOnTheWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

impl StreamActionStepOnTheWire {
    /// The step as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            Self::WritingTheScaffold => "writing_the_scaffold",
            Self::MakingTheVenv => "making_the_venv",
            Self::Compiling => "compiling",
            Self::Starting => "starting",
            Self::Stopping => "stopping",
        }
    }
}

impl std::fmt::Display for StreamActionStepOnTheWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

/// One stream a stream action touched, as `describe_stream_action` reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeStreamActionToolResultStream {
    /// The stream's id.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// The state the stream is listed in now; `null` once the runtime holds it no longer.
    pub state: Option<ListedStreamState>,
}

/// The `describe_stream_action` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescribeStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// The project whose queue holds it.
    pub project_directory: PathBuf,
    /// What it does.
    pub kind: StreamActionKindOnTheWire,
    /// Where it is in its life.
    pub state: StreamActionStateOnTheWire,
    /// The step it is on; `null` while queued and once ended.
    pub step: Option<StreamActionStepOnTheWire>,
    /// The streams it touched so far.
    pub streams: Vec<DescribeStreamActionToolResultStream>,
    /// Why it failed; `null` unless it did.
    pub failed_because: Option<String>,
}

/// The `cancel_stream_action` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelStreamActionToolResult {
    /// The action's id.
    pub action: String,
    /// The action's state once the cancel landed: `cancelled` for a queued one, `in_progress`
    /// for a running one that ends at its current step.
    pub state: StreamActionStateOnTheWire,
    /// Present only when the action was not cancelled: it had ended, or its teardown had begun.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_cancelled_because: Option<String>,
}

/// What the runtime knows of a project's streams, as `list_project_streams` answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListedProjectStreamsState {
    /// The project's last compile listed its streams.
    Compiled,
    /// The project's last compile gave no stream, with `failed_because`.
    GaveNoStream,
    /// A load of the project is queued or running and has not compiled yet.
    Preparing,
    /// No load of the project ran in this runtime.
    NotLoaded,
}

/// One `@stream` of a [`ListProjectStreamsToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListProjectStreamsToolResultStream {
    /// The stream function as `module:qualname`.
    pub stream_function: String,
    /// The function's docstring; `null` when it has none.
    pub description: Option<String>,
    /// The stream id its location computes to.
    pub stream: String,
    /// The state the runtime lists it in; `null` when the runtime holds it not.
    pub state: Option<ListedStreamState>,
}

/// The `list_project_streams` tool's result: a project's `@stream` functions as its last
/// compile in this runtime gave them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListProjectStreamsToolResult {
    /// The project, its symlinks resolved.
    pub project_directory: PathBuf,
    /// What the runtime knows of its streams.
    pub state: ListedProjectStreamsState,
    /// Each `@stream` of `stream.py`, in definition order; empty unless `state` is `compiled`.
    pub streams: Vec<ListProjectStreamsToolResultStream>,
    /// Present only when `state` is `gave_no_stream`: the traceback, or why the `.venv` was not
    /// made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_because: Option<String>,
    /// Present only when `state` is `preparing`: the load's action id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
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
    /// Whether it is preparing, running, stopping, stopped or failed.
    pub state: ListedStreamState,
    /// How many nodes its loaded graph holds; `null` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `null` when it is not.
    pub failed_because: Option<String>,
    /// The stream action working on it; `null` when none is.
    pub action: Option<String>,
    /// The step that action is on; `null` when none is.
    pub step: Option<StreamActionStepOnTheWire>,
}

/// The state of one stream `list_streams` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListedStreamState {
    /// A load or a start of it is under way.
    Preparing,
    /// Loaded; or recorded, neither stopped nor failed, and re-loaded at the runtime's next
    /// start.
    Running,
    /// A stop of it, or a remove of its project, is under way.
    Stopping,
    /// Stopped by its owner, and left unloaded until `start_stream`.
    Stopped,
    /// Skipped at the runtime's start until `start_stream` retries it.
    Failed,
}

impl ListedStreamState {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            ListedStreamState::Preparing => "preparing",
            ListedStreamState::Running => "running",
            ListedStreamState::Stopping => "stopping",
            ListedStreamState::Stopped => "stopped",
            ListedStreamState::Failed => "failed",
        }
    }
}

impl std::fmt::Display for ListedStreamState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

/// An output port's exposure level as the `expose_port` tool takes and answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExposePortLevel {
    /// Read by the stream's own nodes alone.
    Internal,
    /// Read by any other stream, and any code, on this machine as well.
    Private,
    /// Read off this machine as well.
    Public,
}

impl ExposePortLevel {
    /// The level as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            ExposePortLevel::Internal => "internal",
            ExposePortLevel::Private => "private",
            ExposePortLevel::Public => "public",
        }
    }
}

impl std::fmt::Display for ExposePortLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_spelling())
    }
}

/// The `expose_port` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExposePortToolResult {
    /// The stream's id.
    pub stream: String,
    /// The node, as the owner named it.
    pub node: String,
    /// The output port, as the owner named it.
    pub port: String,
    /// The level the port is at now, or will be at when the stream loads.
    pub level: ExposePortLevel,
    /// Whether the level was recorded as the owner's ruling on the stream's record.
    pub recorded: bool,
    /// Present only when a recorded stream's level was raised live and could not be recorded, so a
    /// restart of the runtime puts back the level it had; a restriction that cannot be recorded
    /// is refused instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_recorded_because: Option<String>,
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

/// The `add_relay` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddRelayToolResult {
    /// The id the runtime gave the new relay instance.
    pub relay_id: String,
}

/// The `configure_relay` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigureRelayToolResult {
    /// The relay instance configured.
    pub relay_id: String,
}

/// The `remove_relay` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveRelayToolResult {
    /// The relay instance left and forgotten.
    pub relay_id: String,
}

/// The `list_relays` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListRelaysToolResult {
    /// Every relay instance the runtime holds, the built-in ones first.
    pub relays: Vec<ListedRelayInstance>,
}

/// One relay instance of a [`ListRelaysToolResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedRelayInstance {
    /// The instance's id.
    pub relay_id: String,
    /// The adapter it is joined through, as the relay-adapter catalog names it.
    pub adapter: String,
    /// Whether it is built in, and so never removed.
    pub built_in: bool,
    /// Its settings, every secret left out.
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Each secret setting its adapter takes, by name, and whether it is set.
    pub secrets: std::collections::BTreeMap<String, ListedRelaySecretPresence>,
    /// Where it stands with its relay.
    pub connection_state: ListedRelayConnectionState,
}

/// Whether a relay instance's secret setting holds a value; never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListedRelaySecretPresence {
    /// The secret store holds a value for it.
    Set,
    /// Nothing is held for it.
    Unset,
}

impl std::fmt::Display for ListedRelaySecretPresence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            ListedRelaySecretPresence::Set => "set",
            ListedRelaySecretPresence::Unset => "unset",
        })
    }
}

/// Where a relay instance stands with its relay, as `list_relays` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ListedRelayConnectionState {
    /// Not joined, or left; `why` says why when the runtime knows.
    NotJoined {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<String>,
    },
    /// Joining, with nothing yet to dial.
    Joining,
    /// Joined.
    Joined,
    /// Lost after joining, and joining again after `rejoining_after_ms`.
    Lost {
        why: String,
        rejoining_after_ms: u64,
    },
}

impl std::fmt::Display for ListedRelayConnectionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListedRelayConnectionState::NotJoined { why: None } => {
                formatter.write_str("not joined")
            }
            ListedRelayConnectionState::NotJoined { why: Some(why) } => {
                write!(formatter, "not joined: {why}")
            }
            ListedRelayConnectionState::Joining => formatter.write_str("joining"),
            ListedRelayConnectionState::Joined => formatter.write_str("joined"),
            ListedRelayConnectionState::Lost {
                why,
                rejoining_after_ms,
            } => write!(
                formatter,
                "lost ({why}); joining again in {rejoining_after_ms} ms"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_relay_names_each_secret_set_or_unset_and_its_state_by_tag() {
        let listed = ListRelaysToolResult {
            relays: vec![ListedRelayInstance {
                relay_id: "cloudflare-1".to_owned(),
                adapter: "cloudflare".to_owned(),
                built_in: false,
                settings: serde_json::json!({"region": "us"})
                    .as_object()
                    .unwrap()
                    .clone(),
                secrets: [("token".to_owned(), ListedRelaySecretPresence::Set)].into(),
                connection_state: ListedRelayConnectionState::Lost {
                    why: "refused".to_owned(),
                    rejoining_after_ms: 2000,
                },
            }],
        };
        let wire_text = serde_json::to_string(&listed).unwrap();
        assert_eq!(
            wire_text,
            r#"{"relays":[{"relay_id":"cloudflare-1","adapter":"cloudflare","built_in":false,"settings":{"region":"us"},"secrets":{"token":"set"},"connection_state":{"state":"lost","why":"refused","rejoining_after_ms":2000}}]}"#
        );
        assert_eq!(
            serde_json::from_str::<ListRelaysToolResult>(&wire_text).unwrap(),
            listed
        );
        assert_eq!(
            serde_json::to_string(&ListedRelayConnectionState::NotJoined { why: None }).unwrap(),
            r#"{"state":"not_joined"}"#
        );
    }

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
            wire_text(&RunProjectToolResult {
                action: "rt-1".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
            }),
            r#"{"action":"rt-1","project_directory":"/srv/project"}"#
        );
        assert_eq!(
            wire_text(&CreateProjectToolResult {
                action: "rt-2".to_owned(),
                stream: "brave-otter-main-0a1b2c".to_owned(),
                project_directory: PathBuf::from("/home/me/streams/brave-otter"),
            }),
            r#"{"action":"rt-2","stream":"brave-otter-main-0a1b2c","project_directory":"/home/me/streams/brave-otter"}"#
        );
        assert_eq!(
            wire_text(&StopStreamToolResult {
                action: "rt-3".to_owned(),
                stream: "camera".to_owned(),
            }),
            r#"{"action":"rt-3","stream":"camera"}"#
        );
        assert_eq!(
            wire_text(&StartStreamToolResult {
                action: "rt-4".to_owned(),
                stream: "camera".to_owned(),
            }),
            r#"{"action":"rt-4","stream":"camera"}"#
        );
        assert_eq!(
            wire_text(&RemoveProjectToolResult {
                action: "rt-5".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
            }),
            r#"{"action":"rt-5","project_directory":"/srv/project"}"#
        );
        assert_eq!(
            wire_text(&DescribeStreamActionToolResult {
                action: "rt-1".to_owned(),
                project_directory: PathBuf::from("/srv/project"),
                kind: StreamActionKindOnTheWire::Load,
                state: StreamActionStateOnTheWire::InProgress,
                step: Some(StreamActionStepOnTheWire::MakingTheVenv),
                streams: vec![DescribeStreamActionToolResultStream {
                    stream: "project-main-3f2a9c".to_owned(),
                    display_name: "main".to_owned(),
                    state: Some(ListedStreamState::Preparing),
                }],
                failed_because: None,
            }),
            r#"{"action":"rt-1","project_directory":"/srv/project","kind":"load","state":"in_progress","step":"making_the_venv","streams":[{"stream":"project-main-3f2a9c","display_name":"main","state":"preparing"}],"failed_because":null}"#
        );
        assert_eq!(
            wire_text(&CancelStreamActionToolResult {
                action: "rt-1".to_owned(),
                state: StreamActionStateOnTheWire::Cancelled,
                not_cancelled_because: None,
            }),
            r#"{"action":"rt-1","state":"cancelled"}"#
        );
        assert_eq!(
            wire_text(&ListProjectStreamsToolResult {
                project_directory: PathBuf::from("/srv/project"),
                state: ListedProjectStreamsState::Compiled,
                streams: vec![ListProjectStreamsToolResultStream {
                    stream_function: "stream:main".to_owned(),
                    description: Some("The desk camera.".to_owned()),
                    stream: "project-main-3f2a9c".to_owned(),
                    state: None,
                }],
                failed_because: None,
                action: None,
            }),
            r#"{"project_directory":"/srv/project","state":"compiled","streams":[{"stream_function":"stream:main","description":"The desk camera.","stream":"project-main-3f2a9c","state":null}]}"#
        );
        assert_eq!(
            wire_text(&ListStreamsToolResult {
                streams: vec![
                    ListStreamsToolResultStream {
                        stream: "cam-main-0a1b2c".to_owned(),
                        display_name: "Desk camera".to_owned(),
                        location: Some(StreamLocationOnTheWire {
                            project_directory: PathBuf::from("/srv/cam"),
                            stream_function: "stream:main".to_owned(),
                        }),
                        state: ListedStreamState::Stopping,
                        node_count: Some(4),
                        failed_because: None,
                        action: Some("rt-6".to_owned()),
                        step: Some(StreamActionStepOnTheWire::Stopping),
                    },
                    ListStreamsToolResultStream {
                        stream: "main".to_owned(),
                        display_name: "main".to_owned(),
                        location: None,
                        state: ListedStreamState::Failed,
                        node_count: None,
                        failed_because: Some("an earlier runtime recorded it".to_owned()),
                        action: None,
                        step: None,
                    },
                ],
            }),
            r#"{"streams":[{"stream":"cam-main-0a1b2c","display_name":"Desk camera","location":{"project_directory":"/srv/cam","stream_function":"stream:main"},"state":"stopping","node_count":4,"failed_because":null,"action":"rt-6","step":"stopping"},{"stream":"main","display_name":"main","location":null,"state":"failed","node_count":null,"failed_because":"an earlier runtime recorded it","action":null,"step":null}]}"#
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

    /// `not_recorded_because` is on the wire only when the runtime could not record.
    #[test]
    fn expose_carries_not_recorded_because_only_when_it_is_set() {
        let exposed = |not_recorded_because: Option<&str>| ExposePortToolResult {
            stream: "camera".to_owned(),
            node: "source".to_owned(),
            port: "video".to_owned(),
            level: ExposePortLevel::Public,
            recorded: not_recorded_because.is_none(),
            not_recorded_because: not_recorded_because.map(str::to_owned),
        };

        assert_eq!(
            serde_json::to_string(&exposed(None)).unwrap(),
            r#"{"stream":"camera","node":"source","port":"video","level":"public","recorded":true}"#
        );
        assert_eq!(
            serde_json::to_string(&exposed(Some("cannot write"))).unwrap(),
            r#"{"stream":"camera","node":"source","port":"video","level":"public","recorded":false,"not_recorded_because":"cannot write"}"#
        );
    }

    /// The `Display` spelling a client prints is the spelling the wire carries.
    #[test]
    fn each_state_and_level_displays_as_the_wire_spells_it() {
        for state in [
            ListedStreamState::Preparing,
            ListedStreamState::Running,
            ListedStreamState::Stopping,
            ListedStreamState::Stopped,
            ListedStreamState::Failed,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.to_string())
            );
        }
        for kind in [
            StreamActionKindOnTheWire::Load,
            StreamActionKindOnTheWire::Start,
            StreamActionKindOnTheWire::Stop,
            StreamActionKindOnTheWire::Remove,
            StreamActionKindOnTheWire::ReloadAtStart,
        ] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::String(kind.to_string())
            );
        }
        for state in [
            StreamActionStateOnTheWire::Queued,
            StreamActionStateOnTheWire::InProgress,
            StreamActionStateOnTheWire::Done,
            StreamActionStateOnTheWire::Failed,
            StreamActionStateOnTheWire::Cancelled,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.to_string())
            );
        }
        for step in [
            StreamActionStepOnTheWire::WritingTheScaffold,
            StreamActionStepOnTheWire::MakingTheVenv,
            StreamActionStepOnTheWire::Compiling,
            StreamActionStepOnTheWire::Starting,
            StreamActionStepOnTheWire::Stopping,
        ] {
            assert_eq!(
                serde_json::to_value(step).unwrap(),
                serde_json::Value::String(step.to_string())
            );
        }
        for level in [
            ExposePortLevel::Internal,
            ExposePortLevel::Private,
            ExposePortLevel::Public,
        ] {
            assert_eq!(
                serde_json::to_value(level).unwrap(),
                serde_json::Value::String(level.to_string())
            );
        }
        assert!(serde_json::from_str::<ExposePortLevel>(r#""Public""#).is_err());
    }
}
