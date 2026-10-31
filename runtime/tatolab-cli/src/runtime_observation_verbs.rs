// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab graph`: what the machine's runtime reports, printed. It mutates no graph.

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::ONE_SHOT_VERB_TOOL_CALL_TIMEOUT;
use crate::machine_runtime_local_api_socket::call_one_tool_of_the_running_runtime;
use crate::verb_standard_output::write_verb_standard_output;

/// The local API tool `graph` drives.
pub(crate) const GRAPH_TOOL_NAME: &str = "graph";

/// Drive one tool on the machine's running runtime and print the tool's text.
pub(crate) fn print_local_api_tool_result_of_the_running_runtime(
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<u8, TatolabCommandFailure> {
    let tool_result_text = call_one_tool_of_the_running_runtime(
        tool_name,
        tool_arguments,
        ONE_SHOT_VERB_TOOL_CALL_TIMEOUT,
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
}
