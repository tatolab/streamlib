// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab graph` and `tap`: what the machine's runtime reports, printed. Neither mutates a
//! graph.

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::OBSERVATION_VERB_TOOL_CALL_TIMEOUT;
use crate::machine_runtime_local_api_socket::call_one_tool_of_the_running_runtime;
use crate::verb_standard_output::write_verb_standard_output;

/// The local API tool `graph` drives.
pub(crate) const GRAPH_TOOL_NAME: &str = "graph";

/// The local API tool `tap` drives.
pub(crate) const TAP_TOOL_NAME: &str = "tap";

/// Drive one tool on the machine's running runtime and print the tool's text.
pub(crate) fn print_local_api_tool_result_of_the_running_runtime(
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<u8, TatolabCommandFailure> {
    let tool_result_text = call_one_tool_of_the_running_runtime(
        tool_name,
        tool_arguments,
        OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    )?;
    write_verb_standard_output(&format!("{tool_result_text}\n"))
}

/// `graph`'s tool arguments: one stream's graph when one is named, every stream's otherwise.
pub(crate) fn graph_tool_arguments(
    requested_stream: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut graph_arguments = serde_json::Map::new();
    if let Some(requested_stream) = requested_stream.filter(|stream| !stream.is_empty()) {
        graph_arguments.insert("stream".to_owned(), requested_stream.into());
    }
    graph_arguments
}

/// `tap`'s tool arguments: the stream, the channel, and each bound only when the caller named it,
/// so the tool's own default applies otherwise.
pub(crate) fn tap_tool_arguments(
    stream: &str,
    channel: &str,
    requested_bag_count: Option<i64>,
    requested_max_bag_bytes: Option<i64>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut tap_arguments = serde_json::Map::new();
    tap_arguments.insert("stream".to_owned(), stream.into());
    tap_arguments.insert("channel".to_owned(), channel.into());
    if let Some(requested_bag_count) = requested_bag_count {
        tap_arguments.insert("count".to_owned(), requested_bag_count.into());
    }
    if let Some(requested_max_bag_bytes) = requested_max_bag_bytes {
        tap_arguments.insert("max_bag_bytes".to_owned(), requested_max_bag_bytes.into());
    }
    tap_arguments
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn graph_names_a_stream_only_when_one_was_asked_for() {
        assert_eq!(
            serde_json::Value::Object(graph_tool_arguments(Some("camera"))),
            json!({"stream": "camera"})
        );
        for no_stream_requested in [None, Some("")] {
            assert_eq!(
                serde_json::Value::Object(graph_tool_arguments(no_stream_requested)),
                json!({}),
                "{no_stream_requested:?}"
            );
        }
    }

    #[test]
    fn tap_sends_the_stream_the_channel_and_only_the_bounds_the_caller_named() {
        assert_eq!(
            serde_json::Value::Object(tap_tool_arguments("camera", "cam/video", Some(3), None)),
            json!({"stream": "camera", "channel": "cam/video", "count": 3})
        );
        assert_eq!(
            serde_json::Value::Object(tap_tool_arguments("camera", "cam/video", None, Some(4096))),
            json!({"stream": "camera", "channel": "cam/video", "max_bag_bytes": 4096})
        );
        assert_eq!(
            serde_json::Value::Object(tap_tool_arguments("camera", "cam/video", None, None)),
            json!({"stream": "camera", "channel": "cam/video"})
        );
    }
}
