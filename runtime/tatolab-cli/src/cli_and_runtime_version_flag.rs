// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab --version`: this CLI's version, then how the machine's runtime describes itself, or
//! that none is running. It never fails on a missing runtime.

use std::time::Duration;

use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;
use streamlib_runtime_client_contract::machine_runtime_lock::{
    MachineRuntimeLockHolder, holder_of_the_machine_runtime_lock,
};
use streamlib_runtime_client_contract::streamlib_runtime_directory::current_process_uid;

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::{
    OBSERVATION_VERB_TOOL_CALL_TIMEOUT, call_one_local_api_tool,
};
use crate::machine_runtime_local_api_socket::{
    holder_serving_another_user, local_api_socket_of_the_running_runtime,
};
use crate::runtime_gone_while_following_a_stream::DESCRIBE_RUNTIME_TOOL_NAME;
use crate::stream_actions_on_the_runtime::tool_result_from;
use crate::verb_standard_output::write_verb_standard_output;

/// The runtime line when nothing answers at the machine's local API socket.
const NO_RUNTIME_ANSWERS_RUNTIME_LINE: &str = "runtime: not running";

/// `tatolab --version`.
pub(crate) fn print_the_cli_and_runtime_versions() -> Result<u8, TatolabCommandFailure> {
    write_verb_standard_output(&format!(
        "tatolab {}\n{}\n",
        env!("CARGO_PKG_VERSION"),
        runtime_line_of_the_machines_runtime()
    ))
}

/// The runtime's description line; `runtime: not running` when nothing answers this user's
/// socket, naming the holder of the machine runtime lock when it serves another user; and
/// `runtime: ` followed by the failure when the runtime answers but cannot describe itself.
fn runtime_line_of_the_machines_runtime() -> String {
    let Ok(local_api_socket_path) = local_api_socket_of_the_running_runtime() else {
        return runtime_line_when_nothing_answers(
            holder_serving_another_user(
                holder_of_the_machine_runtime_lock().ok().flatten(),
                current_process_uid(),
            )
            .as_ref(),
        );
    };
    call_one_local_api_tool(
        &local_api_socket_path,
        DESCRIBE_RUNTIME_TOOL_NAME,
        serde_json::Map::new(),
        OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
    )
    .map_err(|tool_call_failure| tool_call_failure.to_string())
    .and_then(|tool_result_text| {
        tool_result_from::<DescribeRuntimeToolResult>(DESCRIBE_RUNTIME_TOOL_NAME, &tool_result_text)
    })
    .map_or_else(
        |describe_failure| format!("runtime: {describe_failure}"),
        |runtime_description| rendered_runtime_description_line(&runtime_description),
    )
}

/// `runtime: not running`, followed by who holds the machine's runtime when it serves another
/// user's socket.
fn runtime_line_when_nothing_answers(
    holder_serving_another_user: Option<&MachineRuntimeLockHolder>,
) -> String {
    match holder_serving_another_user {
        None => NO_RUNTIME_ANSWERS_RUNTIME_LINE.to_owned(),
        Some(holder_serving_another_user) => format!(
            "{NO_RUNTIME_ANSWERS_RUNTIME_LINE}; the machine's runtime is held by \
             {holder_serving_another_user}, serving that user's socket, not yours"
        ),
    }
}

/// `runtime <version> (<build_id>), up <d>d <h>h <m>m, log <folder>`, the log segment reading
/// `keeps no own log` when the runtime names no folder.
fn rendered_runtime_description_line(runtime_description: &DescribeRuntimeToolResult) -> String {
    let runtime_log_segment = runtime_description
        .runtime_log_directory
        .as_ref()
        .map_or_else(
            || "keeps no own log".to_owned(),
            |runtime_log_directory| format!("log {}", runtime_log_directory.display()),
        );
    format!(
        "runtime {} ({}), up {}, {runtime_log_segment}",
        runtime_description.version,
        runtime_description.build_id,
        rendered_days_hours_and_minutes(Duration::from_millis(runtime_description.up_for_ms))
    )
}

/// `<d>d <h>h <m>m`, every part written even when it is zero, the seconds dropped.
fn rendered_days_hours_and_minutes(elapsed: Duration) -> String {
    const SECONDS_PER_MINUTE: u64 = 60;
    const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
    const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
    let elapsed_seconds = elapsed.as_secs();
    format!(
        "{}d {}h {}m",
        elapsed_seconds / SECONDS_PER_DAY,
        elapsed_seconds % SECONDS_PER_DAY / SECONDS_PER_HOUR,
        elapsed_seconds % SECONDS_PER_HOUR / SECONDS_PER_MINUTE
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn a_runtime_description(runtime_log_directory: Option<&str>) -> DescribeRuntimeToolResult {
        DescribeRuntimeToolResult {
            version: "0.55.1".to_owned(),
            build_id: "0.55.1+0123abc.9f8e7d".to_owned(),
            up_for_ms: ((3 * 24 + 2) * 60 + 7) * 60 * 1000 + 59_999,
            runtime_log_directory: runtime_log_directory.map(PathBuf::from),
        }
    }

    #[test]
    fn the_runtime_line_names_its_version_build_uptime_and_log_folder() {
        assert_eq!(
            rendered_runtime_description_line(&a_runtime_description(Some(
                "/home/u/.local/state/tatolab/logs"
            ))),
            "runtime 0.55.1 (0.55.1+0123abc.9f8e7d), up 3d 2h 7m, log \
             /home/u/.local/state/tatolab/logs"
        );
    }

    #[test]
    fn a_runtime_keeping_no_own_log_says_so_in_place_of_a_folder() {
        assert_eq!(
            rendered_runtime_description_line(&a_runtime_description(None)),
            "runtime 0.55.1 (0.55.1+0123abc.9f8e7d), up 3d 2h 7m, keeps no own log"
        );
    }

    #[test]
    fn nothing_answering_names_a_runtime_held_for_another_user() {
        assert_eq!(
            runtime_line_when_nothing_answers(None),
            "runtime: not running"
        );
        assert_eq!(
            runtime_line_when_nothing_answers(Some(&MachineRuntimeLockHolder {
                user_name: Some("other".to_owned()),
                uid: 1001,
                pid: 4242,
                executable: Some(PathBuf::from("/opt/tatolab/bin/tatolabd")),
            })),
            "runtime: not running; the machine's runtime is held by user other (uid 1001), pid \
             4242, /opt/tatolab/bin/tatolabd, serving that user's socket, not yours"
        );
    }

    #[test]
    fn an_uptime_writes_every_part_even_when_it_is_zero() {
        assert_eq!(
            rendered_days_hours_and_minutes(Duration::from_secs(59)),
            "0d 0h 0m"
        );
        assert_eq!(
            rendered_days_hours_and_minutes(Duration::from_secs(24 * 60 * 60 + 60)),
            "1d 0h 1m"
        );
    }
}
