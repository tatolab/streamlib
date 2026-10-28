// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The names a runtime's local API speaks on its socket and its loopback HTTP listener on its
//! port, and the shape of each result a client parses, written once for the runtime that serves
//! them and every client that calls them.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The route MCP's Streamable HTTP transport is served on.
pub const MCP_STREAMABLE_HTTP_ROUTE_PATH: &str = "/mcp";

/// The request target a client upgrades to MCP's stdio framing on.
pub const MCP_STDIO_UPGRADE_REQUEST_TARGET: &str = "/mcp/stdio";

/// The `Upgrade` protocol token [`MCP_STDIO_UPGRADE_REQUEST_TARGET`] switches to, matched
/// case-insensitively.
pub const MCP_STDIO_UPGRADE_PROTOCOL_TOKEN: &str = "mcp-stdio";

/// How long a client waits for `run_stream` or `start_stream`. The engine asserts at build time
/// that its `STREAM_FUNCTION_COMPILE_BOUND`, `PROCESSOR_INTERPRETER_DESCRIBE_BOUND` and
/// `ENGINE_TEARDOWN_WATCHDOG_BUDGET` fit within it for this load and one stream action queued
/// ahead of it; more actions queued ahead can outlast it, and the runtime still takes the action.
pub const STREAM_LOAD_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(360);

/// How long a client waits for `stop_stream`, `remove_stream` or `expose_port`. The engine
/// asserts at build time that its `PROCESSOR_INTERPRETER_DESCRIBE_BOUND` and
/// `ENGINE_TEARDOWN_WATCHDOG_BUDGET` fit within it for this action and one stream action queued
/// ahead of it; more actions queued ahead can outlast it, and the runtime still takes the action.
pub const STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(240);

/// Which load of a stream a `run_stream` result or a `logs` page is about: opaque, compared only
/// for equality. A stream re-loaded under the same name is another instance, and numbers its log
/// records from 1 again.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoadedStreamInstance(pub String);

impl std::fmt::Display for LoadedStreamInstance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The `logs` tool's result: one loaded stream's records after the sequence number asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogsToolResult {
    /// The stream's id.
    pub stream: String,
    /// The load whose records these are; a page naming another instance than the last numbers
    /// its records afresh.
    pub stream_instance: LoadedStreamInstance,
    /// The records after `after`, oldest first.
    pub records: Vec<LogsToolResultRecord>,
    /// The `after` the next call passes to read on from this page.
    pub next_after: u64,
    /// Records after `after` the runtime dropped from its in-memory history before this read.
    pub records_no_longer_held: u64,
}

/// One record of a [`LogsToolResult`], numbered by the runtime from 1 per loaded stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogsToolResultRecord {
    /// The record's place in its stream's log.
    pub sequence: u64,
    /// The record as its stream's JSONL log file holds it.
    pub record: serde_json::Value,
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

/// The `run_stream` tool's result: the stream a project's compile loaded and started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStreamToolResult {
    /// The stream's id, computed from its location; every tool naming the stream takes it.
    pub stream: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// This load of it, as its `logs` pages name it.
    pub stream_instance: LoadedStreamInstance,
    /// Whether the runtime keeps it; else it is attached to the connection that ran it.
    pub kept: bool,
    /// Where the stream's code lives.
    pub location: StreamLocationOnTheWire,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
    /// Whether the load replaced the kept stream of the same location.
    pub replaced_the_kept_record: bool,
    /// Each line the compile wrote to its standard error, the cross-floor check's warnings
    /// among them; empty when it wrote nothing.
    pub compile_warnings: Vec<String>,
}

/// The `stop_stream` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopStreamToolResult {
    /// The stream's id.
    pub stream: String,
    /// Always true: a stream the runtime could not unload is refused instead.
    pub stopped: bool,
    /// Whether the stream is kept, and so recorded stopped.
    pub kept: bool,
    /// Present only when a kept stream unloaded and could not be recorded stopped, so a restart
    /// of the runtime loads it again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_recorded_because: Option<String>,
}

/// The `start_stream` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartStreamToolResult {
    /// The stream's id.
    pub stream: String,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
}

/// The `remove_stream` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveStreamToolResult {
    /// The stream's id.
    pub stream: String,
    /// Whether the stream was loaded and is now unloaded.
    pub unloaded: bool,
    /// Whether the stream was kept and its record is now gone.
    pub forgotten: bool,
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
    /// Whether it is attached, kept, stopped or failed.
    pub state: ListedStreamState,
    /// How many nodes its loaded graph holds; `null` when it is not loaded.
    pub node_count: Option<usize>,
    /// Why it is failed; `null` when it is not.
    pub failed_because: Option<String>,
}

/// The state of one stream `list_streams` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListedStreamState {
    /// Loaded, and lives as long as the connection that ran it.
    Attached,
    /// Kept by the runtime: loaded, or recorded and not loaded.
    Kept,
    /// Kept, and stopped by its owner.
    Stopped,
    /// Kept, and failed: skipped at the runtime's start until `start_stream`
    /// retries it.
    Failed,
}

impl ListedStreamState {
    /// The state as the wire spells it.
    pub fn wire_spelling(self) -> &'static str {
        match self {
            ListedStreamState::Attached => "attached",
            ListedStreamState::Kept => "kept",
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
    /// Whether the level was recorded as the owner's ruling on a kept stream.
    pub recorded: bool,
    /// Present only when a kept stream's level was raised live and could not be recorded, so a
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
            wire_text(&RunStreamToolResult {
                stream: "project-main-3f2a9c".to_owned(),
                display_name: "main".to_owned(),
                stream_instance: LoadedStreamInstance("4".to_owned()),
                kept: true,
                location: StreamLocationOnTheWire {
                    project_directory: PathBuf::from("/srv/project"),
                    stream_function: "stream:main".to_owned(),
                },
                node_count: 3,
                replaced_the_kept_record: false,
                compile_warnings: vec!["tatolab: one warning".to_owned()],
            }),
            r#"{"stream":"project-main-3f2a9c","display_name":"main","stream_instance":"4","kept":true,"location":{"project_directory":"/srv/project","stream_function":"stream:main"},"node_count":3,"replaced_the_kept_record":false,"compile_warnings":["tatolab: one warning"]}"#
        );
        assert_eq!(
            wire_text(&StartStreamToolResult {
                stream: "camera".to_owned(),
                node_count: 3,
            }),
            r#"{"stream":"camera","node_count":3}"#
        );
        assert_eq!(
            wire_text(&RemoveStreamToolResult {
                stream: "camera".to_owned(),
                unloaded: true,
                forgotten: false,
            }),
            r#"{"stream":"camera","unloaded":true,"forgotten":false}"#
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
                        state: ListedStreamState::Attached,
                        node_count: Some(4),
                        failed_because: None,
                    },
                    ListStreamsToolResultStream {
                        stream: "main".to_owned(),
                        display_name: "main".to_owned(),
                        location: None,
                        state: ListedStreamState::Failed,
                        node_count: None,
                        failed_because: Some("an earlier runtime recorded it".to_owned()),
                    },
                ],
            }),
            r#"{"streams":[{"stream":"cam-main-0a1b2c","display_name":"Desk camera","location":{"project_directory":"/srv/cam","stream_function":"stream:main"},"state":"attached","node_count":4,"failed_because":null},{"stream":"main","display_name":"main","location":null,"state":"failed","node_count":null,"failed_because":"an earlier runtime recorded it"}]}"#
        );
        assert_eq!(
            wire_text(&LogsToolResult {
                stream: "camera".to_owned(),
                stream_instance: LoadedStreamInstance("4".to_owned()),
                records: vec![LogsToolResultRecord {
                    sequence: 7,
                    record: serde_json::json!({"message": "hello"}),
                }],
                next_after: 7,
                records_no_longer_held: 2,
            }),
            r#"{"stream":"camera","stream_instance":"4","records":[{"sequence":7,"record":{"message":"hello"}}],"next_after":7,"records_no_longer_held":2}"#
        );
    }

    /// `not_recorded_because` is on the wire only when the runtime could not record.
    #[test]
    fn stop_and_expose_carry_not_recorded_because_only_when_it_is_set() {
        let stopped = |not_recorded_because: Option<&str>| StopStreamToolResult {
            stream: "camera".to_owned(),
            stopped: true,
            kept: true,
            not_recorded_because: not_recorded_because.map(str::to_owned),
        };
        let exposed = |not_recorded_because: Option<&str>| ExposePortToolResult {
            stream: "camera".to_owned(),
            node: "source".to_owned(),
            port: "video".to_owned(),
            level: ExposePortLevel::Public,
            recorded: not_recorded_because.is_none(),
            not_recorded_because: not_recorded_because.map(str::to_owned),
        };

        assert_eq!(
            serde_json::to_string(&stopped(None)).unwrap(),
            r#"{"stream":"camera","stopped":true,"kept":true}"#
        );
        assert_eq!(
            serde_json::to_string(&stopped(Some("cannot write"))).unwrap(),
            r#"{"stream":"camera","stopped":true,"kept":true,"not_recorded_because":"cannot write"}"#
        );
        assert_eq!(
            serde_json::to_string(&exposed(None)).unwrap(),
            r#"{"stream":"camera","node":"source","port":"video","level":"public","recorded":true}"#
        );
        assert_eq!(
            serde_json::to_string(&exposed(Some("cannot write"))).unwrap(),
            r#"{"stream":"camera","node":"source","port":"video","level":"public","recorded":false,"not_recorded_because":"cannot write"}"#
        );
        assert_eq!(
            serde_json::from_str::<StopStreamToolResult>(
                r#"{"stream":"camera","stopped":true,"kept":true}"#
            )
            .unwrap(),
            stopped(None)
        );
    }

    /// The `Display` spelling a client prints is the spelling the wire carries.
    #[test]
    fn each_state_and_level_displays_as_the_wire_spells_it() {
        for state in [
            ListedStreamState::Attached,
            ListedStreamState::Kept,
            ListedStreamState::Stopped,
            ListedStreamState::Failed,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.to_string())
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
