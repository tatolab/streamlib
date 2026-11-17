// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab call`: any operation the machine's runtime serves to agents, run by name from a
//! shell, its answer or its refusal printed as the runtime's JSON.

use std::path::Path;

use crate::TatolabCommandFailure;
use crate::local_api_connection::LocalApiConnection;
use crate::local_api_mcp_tool_client::{ONE_SHOT_VERB_TOOL_CALL_TIMEOUT, OperationAnswered};
use crate::machine_runtime_local_api_socket::local_api_socket_of_the_running_runtime;
use crate::verb_standard_output::write_verb_standard_output;

/// The exit code of a call the runtime refused, whose refusal is printed on stdout.
const REFUSED_OPERATION_EXIT_CODE: u8 = 1;

/// `--params`' value read as the JSON object an operation's params are.
pub(crate) fn operation_params_argument(
    params_argument: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match serde_json::from_str(params_argument) {
        Ok(serde_json::Value::Object(params)) => Ok(params),
        Ok(_) => Err("the params are one JSON object".to_owned()),
        Err(not_json) => Err(format!("not JSON: {not_json}")),
    }
}

/// Run `operation` with `params` on the machine's running runtime and print what it answered:
/// its result, exiting 0, or its refusal, exiting 1.
pub(crate) fn call_an_operation_of_the_running_runtime(
    operation: &str,
    params: serde_json::Map<String, serde_json::Value>,
) -> Result<u8, TatolabCommandFailure> {
    let local_api_socket_path = local_api_socket_of_the_running_runtime()?;
    call_an_operation(&local_api_socket_path, operation, params)
}

fn call_an_operation(
    local_api_socket_path: &Path,
    operation: &str,
    params: serde_json::Map<String, serde_json::Value>,
) -> Result<u8, TatolabCommandFailure> {
    let mut local_api_connection =
        LocalApiConnection::open(local_api_socket_path, ONE_SHOT_VERB_TOOL_CALL_TIMEOUT)?;
    match local_api_connection.call_operation_by_name(operation, params)? {
        OperationAnswered::Answered(answer) => write_verb_standard_output(&format!("{answer}\n")),
        OperationAnswered::Refused(refusal) => {
            write_verb_standard_output(&format!("{refusal}\n"))?;
            Ok(REFUSED_OPERATION_EXIT_CODE)
        }
    }
}

#[cfg(test)]
mod tests;
