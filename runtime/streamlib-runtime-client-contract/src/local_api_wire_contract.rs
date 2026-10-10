// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The names a runtime's local API speaks on its socket, and the shape of each tool result a
//! client parses, written once for the runtime that serves them and every client that calls them.

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

/// The surface-image exchange as an OpenAPI path template: a published surface id in, that
/// frame's exact pixels as a PNG out.
pub const SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE: &str = "/api/surfaces/{surface_id}/image";

/// The placeholder in [`SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE`] the percent-encoded id fills.
const SURFACE_IMAGE_EXCHANGE_ROUTE_SURFACE_ID_PLACEHOLDER: &str = "{surface_id}";

/// RFC 3986's unreserved set: everything outside it is percent-encoded into the route's
/// `{surface_id}` segment. A pooled frame id is `<slot>#<generation>`, and a bare `#` would make
/// the generation a URL fragment the runtime never sees.
const SURFACE_ID_PATH_SEGMENT_PERCENT_ENCODE_ASCII_SET: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');

/// The header carrying the width the exchanged surface's own backing holds. A downscaled image's
/// PNG header states only what was encoded, so this is how a caller learns the true extent.
pub const SURFACE_PIXEL_WIDTH_HEADER_NAME: &str = "x-streamlib-surface-pixel-width";

/// Height counterpart of [`SURFACE_PIXEL_WIDTH_HEADER_NAME`].
pub const SURFACE_PIXEL_HEIGHT_HEADER_NAME: &str = "x-streamlib-surface-pixel-height";

/// The status the exchange answers for an id whose frame's pool slot has since been recycled:
/// the id was real and the frame is gone, so a newer bag answers it where a `404` never will.
pub const RECYCLED_FRAME_HTTP_STATUS_CODE: u16 = 410;

/// The exchange route's path for `published_surface_id`, ready to put on the wire.
pub fn surface_image_exchange_route_path_for_surface_id(published_surface_id: &str) -> String {
    let percent_encoded_surface_id = percent_encoding::utf8_percent_encode(
        published_surface_id,
        SURFACE_ID_PATH_SEGMENT_PERCENT_ENCODE_ASCII_SET,
    )
    .to_string();
    SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE.replace(
        SURFACE_IMAGE_EXCHANGE_ROUTE_SURFACE_ID_PLACEHOLDER,
        &percent_encoded_surface_id,
    )
}

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

/// The `tap` tool's result: a bounded sample of the bags one channel carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapToolResult {
    /// The tapped output port's address, `<runtime_name>/<node>/<port>`.
    pub channel: String,
    /// Bags the sample asked for, after the runtime bounded the caller's count.
    pub requested: usize,
    /// Bags the sample carries.
    pub received: usize,
    /// The longest the runtime waited to fill the sample.
    pub window_ms: u64,
    /// Bags the tap dropped because the sample fell behind the channel; a tap drops rather than
    /// back-pressure the source.
    pub dropped_bags: u64,
    /// The per-bag ceiling on the bytes hex-encoded into [`TapToolResultBag::hex_preview`].
    pub max_bag_bytes: usize,
    /// Bags received and discarded because they would have overrun the whole result's byte
    /// budget: 0 or 1, since the sample stops at the first.
    pub bags_withheld_at_byte_budget: usize,
    /// The sampled bags, in the order the channel carried them.
    pub bags: Vec<TapToolResultBag>,
}

/// One bag of a [`TapToolResult`]: the frame header and payload exactly as the channel carried
/// them, hex-encoded up to [`TapToolResult::max_bag_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapToolResultBag {
    /// The whole bag's length, frame header included, whether or not the preview holds all of it.
    pub byte_len: u64,
    /// Lowercase hex, two digits per byte with nothing between them.
    pub hex_preview: String,
    /// Whether the preview holds less than the whole bag; a capped bag cannot be decoded.
    pub hex_truncated: bool,
}

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
    /// The stream's cast name.
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

/// The `run_stream` tool's result: the stream a project's compile loaded and started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStreamToolResult {
    /// The name the stream was loaded under.
    pub stream: String,
    /// This load of it, as its `logs` pages name it.
    pub stream_instance: LoadedStreamInstance,
    /// Whether the runtime keeps it; else it is attached to the connection that ran it.
    pub kept: bool,
    /// The project the compile ran in.
    pub project_directory: PathBuf,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
    /// Whether the load replaced the kept stream of the same project and function.
    pub replaced_the_kept_record: bool,
    /// Each line the compile wrote to its standard error, the cross-floor check's warnings
    /// among them; empty when it wrote nothing.
    pub compile_warnings: Vec<String>,
}

/// The `stop_stream` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopStreamToolResult {
    /// The stream's cast name.
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
    /// The stream's cast name.
    pub stream: String,
    /// How many nodes the loaded graph holds.
    pub node_count: usize,
}

/// The `remove_stream` tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveStreamToolResult {
    /// The stream's cast name.
    pub stream: String,
    /// Whether the stream was loaded and is now unloaded.
    pub unloaded: bool,
    /// Whether the stream was kept and its record is now gone.
    pub forgotten: bool,
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
    /// The stream's cast name.
    pub name: String,
    /// Whether it is attached, kept, stopped or failed.
    pub state: ListedStreamState,
    /// The stream's project directory.
    pub project_directory: PathBuf,
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
    /// The stream's cast name.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare `#` would make the generation a URL fragment the runtime never sees.
    #[test]
    fn a_pooled_frame_id_is_percent_encoded_into_the_exchange_route() {
        assert_eq!(
            surface_image_exchange_route_path_for_surface_id("cam/frame#7"),
            "/api/surfaces/cam%2Fframe%237/image"
        );
    }

    #[test]
    fn a_surface_id_is_encoded_down_to_the_unreserved_set() {
        assert_eq!(
            surface_image_exchange_route_path_for_surface_id("Az09-._~ é#/?%"),
            "/api/surfaces/Az09-._~%20%C3%A9%23%2F%3F%25/image"
        );
    }

    #[test]
    fn an_empty_surface_id_fills_an_empty_segment() {
        assert_eq!(
            surface_image_exchange_route_path_for_surface_id(""),
            "/api/surfaces//image"
        );
    }

    fn tap_tool_result_carrying(bags: Vec<TapToolResultBag>) -> TapToolResult {
        TapToolResult {
            channel: "studio/camera/video".to_owned(),
            requested: 8,
            received: bags.len(),
            window_ms: 500,
            dropped_bags: 2,
            max_bag_bytes: 1024,
            bags_withheld_at_byte_budget: 0,
            bags,
        }
    }

    /// The keys and JSON types a caller parses, spelled as the wire spells them.
    #[test]
    fn a_tap_tool_result_serializes_every_key_with_its_wire_type() {
        let serialized = serde_json::to_value(tap_tool_result_carrying(vec![TapToolResultBag {
            byte_len: 9000,
            hex_preview: "0aff".to_owned(),
            hex_truncated: true,
        }]))
        .unwrap();

        assert_eq!(
            serialized,
            serde_json::json!({
                "channel": "studio/camera/video",
                "requested": 8,
                "received": 1,
                "window_ms": 500,
                "dropped_bags": 2,
                "max_bag_bytes": 1024,
                "bags_withheld_at_byte_budget": 0,
                "bags": [{"byte_len": 9000, "hex_preview": "0aff", "hex_truncated": true}],
            })
        );
    }

    #[test]
    fn a_tap_tool_result_round_trips_through_its_json() {
        let tap_tool_result = tap_tool_result_carrying(vec![
            TapToolResultBag {
                byte_len: 2,
                hex_preview: "0aff".to_owned(),
                hex_truncated: false,
            },
            TapToolResultBag {
                byte_len: u64::MAX,
                hex_preview: String::new(),
                hex_truncated: true,
            },
        ]);

        let serialized = serde_json::to_string(&tap_tool_result).unwrap();

        assert_eq!(
            serde_json::from_str::<TapToolResult>(&serialized).unwrap(),
            tap_tool_result
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
                stream: "camera".to_owned(),
                stream_instance: LoadedStreamInstance("4".to_owned()),
                kept: true,
                project_directory: PathBuf::from("/srv/project"),
                node_count: 3,
                replaced_the_kept_record: false,
                compile_warnings: vec!["tatolab: one warning".to_owned()],
            }),
            r#"{"stream":"camera","stream_instance":"4","kept":true,"project_directory":"/srv/project","node_count":3,"replaced_the_kept_record":false,"compile_warnings":["tatolab: one warning"]}"#
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
                        name: "camera".to_owned(),
                        state: ListedStreamState::Attached,
                        project_directory: PathBuf::from("/srv/cam"),
                        node_count: Some(4),
                        failed_because: None,
                    },
                    ListStreamsToolResultStream {
                        name: "parked".to_owned(),
                        state: ListedStreamState::Stopped,
                        project_directory: PathBuf::from("/srv/parked"),
                        node_count: None,
                        failed_because: None,
                    },
                    ListStreamsToolResultStream {
                        name: "crasher".to_owned(),
                        state: ListedStreamState::Failed,
                        project_directory: PathBuf::from("/srv/crasher"),
                        node_count: None,
                        failed_because: Some("it crashed the runtime".to_owned()),
                    },
                ],
            }),
            r#"{"streams":[{"name":"camera","state":"attached","project_directory":"/srv/cam","node_count":4,"failed_because":null},{"name":"parked","state":"stopped","project_directory":"/srv/parked","node_count":null,"failed_because":null},{"name":"crasher","state":"failed","project_directory":"/srv/crasher","node_count":null,"failed_because":"it crashed the runtime"}]}"#
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

    /// A flag is a bool and a length a whole number: a result spelling either another way is not
    /// the tap tool's.
    #[test]
    fn a_bag_whose_flag_or_length_has_another_json_type_is_refused() {
        for misspelled_bag in [
            serde_json::json!({"byte_len": 5, "hex_preview": "", "hex_truncated": 1}),
            serde_json::json!({"byte_len": 5, "hex_preview": "", "hex_truncated": "yes"}),
            serde_json::json!({"byte_len": 5.0, "hex_preview": "", "hex_truncated": false}),
            serde_json::json!({"byte_len": "5", "hex_preview": "", "hex_truncated": false}),
            serde_json::json!({"byte_len": -5, "hex_preview": "", "hex_truncated": false}),
            serde_json::json!({"hex_preview": "", "hex_truncated": false}),
        ] {
            assert!(
                serde_json::from_value::<TapToolResultBag>(misspelled_bag.clone()).is_err(),
                "{misspelled_bag}"
            );
        }
    }
}
