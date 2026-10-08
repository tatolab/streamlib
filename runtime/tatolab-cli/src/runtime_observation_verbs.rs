// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab nodes`, `graph` and `tap`: what the runtimes on this machine report, printed. None of
//! them mutates a graph.

use std::path::Path;

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::call_one_local_api_tool;
use crate::local_api_runtime_selection::{
    LivenessCheckedNodeRegistryEntry, scan_liveness_check_and_prune_node_registry,
    select_live_runtime_on_this_machine, this_users_node_registry_directory,
};

/// The local API tool `graph` drives.
pub(crate) const GRAPH_TOOL_NAME: &str = "graph";

/// The local API tool `tap` drives.
pub(crate) const TAP_TOOL_NAME: &str = "tap";

const NODES_TABLE_RUNTIME_NAME_COLUMN_HEADER: &str = "RUNTIME_NAME";
const NODES_TABLE_RUNTIME_ID_COLUMN_HEADER: &str = "RUNTIME_ID";
const NODES_TABLE_LOCAL_API_SOCKET_COLUMN_HEADER: &str = "LOCAL_API_SOCKET";
const NODES_TABLE_PID_COLUMN_HEADER: &str = "PID";
const NODES_TABLE_ALIVE_COLUMN_HEADER: &str = "ALIVE?";
const NODES_TABLE_HINT_COLUMN_HEADER: &str = "HINT";
const NODES_TABLE_PID_COLUMN_WIDTH: usize = 7;
const NODES_TABLE_ALIVE_COLUMN_WIDTH: usize = 6;

/// One line of the `nodes` table: runtime name, runtime_id, local API socket, pid, alive? and
/// hint.
type NodesTableLineCells<'cell> = [&'cell str; 6];

/// What `nodes` prints for `liveness_checked_entries` read from `node_registry_directory`: a
/// header and one aligned row per entry, or a line naming the registry when it holds none.
pub(crate) fn render_node_registry_listing(
    node_registry_directory: &Path,
    liveness_checked_entries: &[LivenessCheckedNodeRegistryEntry],
) -> String {
    if liveness_checked_entries.is_empty() {
        return format!(
            "No running nodes found in {}.\n",
            node_registry_directory.display()
        );
    }
    let rows: Vec<[String; 6]> = liveness_checked_entries
        .iter()
        .map(|liveness_checked_entry| {
            let node_registry_entry = &liveness_checked_entry.node_registry_entry;
            [
                node_registry_entry.runtime_name.clone(),
                node_registry_entry.runtime_id.clone(),
                node_registry_entry
                    .local_api_socket_path
                    .display()
                    .to_string(),
                node_registry_entry.pid.to_string(),
                if liveness_checked_entry.local_api_answers {
                    "yes"
                } else {
                    "no"
                }
                .to_owned(),
                node_registry_entry.hint.clone(),
            ]
        })
        .collect();
    let column_width = |column_index: usize, column_header: &str| {
        rows.iter()
            .map(|row| row[column_index].chars().count())
            .chain([column_header.chars().count()])
            .max()
            .unwrap_or_default()
    };
    let runtime_name_width = column_width(0, NODES_TABLE_RUNTIME_NAME_COLUMN_HEADER);
    let runtime_id_width = column_width(1, NODES_TABLE_RUNTIME_ID_COLUMN_HEADER);
    let local_api_socket_width = column_width(2, NODES_TABLE_LOCAL_API_SOCKET_COLUMN_HEADER);
    let render_line = |line_cells: NodesTableLineCells<'_>| {
        let [runtime_name, runtime_id, local_api_socket, pid, alive, hint] = line_cells;
        format!(
            "{runtime_name:<runtime_name_width$}  {runtime_id:<runtime_id_width$}  \
             {local_api_socket:<local_api_socket_width$}  \
             {pid:>NODES_TABLE_PID_COLUMN_WIDTH$}  {alive:<NODES_TABLE_ALIVE_COLUMN_WIDTH$}  \
             {hint}\n"
        )
    };
    let mut listing = render_line([
        NODES_TABLE_RUNTIME_NAME_COLUMN_HEADER,
        NODES_TABLE_RUNTIME_ID_COLUMN_HEADER,
        NODES_TABLE_LOCAL_API_SOCKET_COLUMN_HEADER,
        NODES_TABLE_PID_COLUMN_HEADER,
        NODES_TABLE_ALIVE_COLUMN_HEADER,
        NODES_TABLE_HINT_COLUMN_HEADER,
    ]);
    for row in &rows {
        listing.push_str(&render_line(row.each_ref().map(String::as_str)));
    }
    listing
}

/// `tatolab nodes`: this machine's registered runtimes, liveness-checked, the dead ones pruned.
pub(crate) fn print_node_registry_listing() -> Result<u8, TatolabCommandFailure> {
    let node_registry_directory = this_users_node_registry_directory()?;
    let liveness_checked_entries =
        scan_liveness_check_and_prune_node_registry(&node_registry_directory)?;
    print!(
        "{}",
        render_node_registry_listing(&node_registry_directory, &liveness_checked_entries)
    );
    Ok(0)
}

/// Pick the live runtime on this machine `requested_runtime_name_or_id` names, drive one tool on
/// it, and print the tool's text.
pub(crate) fn print_local_api_tool_result_of_selected_runtime(
    requested_runtime_name_or_id: Option<&str>,
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<u8, TatolabCommandFailure> {
    let selected_runtime = select_live_runtime_on_this_machine(requested_runtime_name_or_id)?;
    let tool_result_text = call_one_local_api_tool(
        &selected_runtime.local_api_socket_path,
        tool_name,
        tool_arguments,
    )?;
    println!("{tool_result_text}");
    Ok(0)
}

/// `tap`'s tool arguments: the channel, and each bound only when the caller named it, so the
/// tool's own default applies otherwise.
pub(crate) fn tap_tool_arguments(
    channel: &str,
    requested_bag_count: Option<i64>,
    requested_max_bag_bytes: Option<i64>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut tap_arguments = serde_json::Map::new();
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
    use std::path::PathBuf;

    use serde_json::json;
    use streamlib_runtime_client_contract::node_registry::{
        NODE_REGISTRY_SCHEMA_VERSION, NodeRegistryEntry,
    };

    use super::*;
    use crate::isolated_node_registry::{IsolatedNodeRegistry, a_registry_entry_named};
    use crate::stub_local_api_server::StubLocalApiServer;

    fn liveness_checked_entry(
        runtime_name: &str,
        runtime_id: &str,
        local_api_socket_path: &str,
        pid: u32,
        local_api_answers: bool,
        hint: &str,
    ) -> LivenessCheckedNodeRegistryEntry {
        LivenessCheckedNodeRegistryEntry {
            node_registry_entry: NodeRegistryEntry {
                schema_version: NODE_REGISTRY_SCHEMA_VERSION,
                runtime_id: runtime_id.to_owned(),
                runtime_name: runtime_name.to_owned(),
                local_api_socket_path: PathBuf::from(local_api_socket_path),
                pid,
                hint: hint.to_owned(),
            },
            local_api_answers,
        }
    }

    #[test]
    fn an_empty_registry_is_one_line_naming_it() {
        assert_eq!(
            render_node_registry_listing(Path::new("/run/user/1000/streamlib/nodes"), &[]),
            "No running nodes found in /run/user/1000/streamlib/nodes.\n"
        );
    }

    /// Text columns left-aligned to their widest cell, the pid right-aligned in seven, alive? in
    /// six, two spaces between, the hint last and unpadded.
    #[test]
    fn the_table_lays_out_every_column_as_the_python_cli_printed_it() {
        let listing = render_node_registry_listing(
            Path::new("/unused"),
            &[
                liveness_checked_entry(
                    "rig-desk-a1b2",
                    "Rlisted",
                    "/tmp/tl-stub-abc/local-api.sock",
                    4242,
                    true,
                    "tatolabd (/tmp/app)",
                ),
                liveness_checked_entry("r", "Rlonger-runtime-id", "/s", 1234567, false, ""),
            ],
        );

        assert_eq!(
            listing,
            [
                "RUNTIME_NAME   RUNTIME_ID          LOCAL_API_SOCKET                     PID  \
                 ALIVE?  HINT\n",
                "rig-desk-a1b2  Rlisted             /tmp/tl-stub-abc/local-api.sock     4242  \
                 yes     tatolabd (/tmp/app)\n",
                "r              Rlonger-runtime-id  /s                               1234567  \
                 no      \n",
            ]
            .concat()
        );
    }

    #[test]
    fn a_column_is_as_wide_as_its_widest_cell_in_characters_not_bytes() {
        let listing = render_node_registry_listing(
            Path::new("/unused"),
            &[liveness_checked_entry(
                "café-rig-desk",
                "Rcafé",
                "/s",
                1,
                true,
                "",
            )],
        );
        let lines: Vec<&str> = listing.lines().collect();

        assert_eq!(
            lines[0]
                .find("RUNTIME_ID")
                .map(|byte_offset| lines[0][..byte_offset].chars().count()),
            lines[1]
                .find("Rcafé")
                .map(|byte_offset| lines[1][..byte_offset].chars().count())
        );
    }

    #[test]
    fn a_live_runtime_in_a_scanned_registry_is_a_row_marked_alive() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rlisted",
            "rig-desk-a1b2",
            &stub_local_api_server.local_api_socket_path,
        ));
        let node_registry_directory = isolated_node_registry.node_registry_directory();

        let listing = render_node_registry_listing(
            &node_registry_directory,
            &scan_liveness_check_and_prune_node_registry(&node_registry_directory).unwrap(),
        );

        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), 2, "{listing}");
        assert!(lines[0].starts_with("RUNTIME_NAME  "), "{listing}");
        assert!(lines[0].contains("LOCAL_API_SOCKET"), "{listing}");
        let row_cells: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(
            row_cells[..5],
            [
                "rig-desk-a1b2",
                "Rlisted",
                &stub_local_api_server
                    .local_api_socket_path
                    .display()
                    .to_string(),
                &std::process::id().to_string(),
                "yes",
            ]
        );
    }

    #[test]
    fn tap_sends_the_channel_and_only_the_bounds_the_caller_named() {
        assert_eq!(
            serde_json::Value::Object(tap_tool_arguments("cam/video", Some(3), None)),
            json!({"channel": "cam/video", "count": 3})
        );
        assert_eq!(
            serde_json::Value::Object(tap_tool_arguments("cam/video", None, Some(4096))),
            json!({"channel": "cam/video", "max_bag_bytes": 4096})
        );
        assert_eq!(
            serde_json::Value::Object(tap_tool_arguments("cam/video", None, None)),
            json!({"channel": "cam/video"})
        );
    }
}
