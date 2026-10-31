// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What `logs -f` and `dev` say when the runtime they follow a stream on goes away: that it
//! crashed or was stopped, and where its own log is, as `describe_runtime` named it while it
//! answered.

use std::path::{Path, PathBuf};

use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;
use streamlib_runtime_client_contract::tatolab_state_directory::TatolabStateDirectory;

use crate::local_api_mcp_tool_client::{
    LocalApiMcpToolClientFailure, LocalApiMcpToolClientFailureKind,
};
use crate::project_log_records_from_the_runtime::DESCRIBE_RUNTIME_TOOL_NAME;
use crate::stream_actions_on_the_runtime::tool_result_from;

/// The runtime's own log folder, as the runtime described it while it answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeLogDirectoryAsDescribed {
    /// The runtime writes its own log in this folder.
    WrittenIn(PathBuf),
    /// The runtime said it writes no log of its own.
    NoneWritten,
    /// The runtime did not describe itself; its log is looked for in the state directory.
    NotDescribed,
}

impl RuntimeLogDirectoryAsDescribed {
    /// The folder `describe_runtime_answer` names.
    pub(crate) fn from_the_describe_runtime_answer(
        describe_runtime_answer: Result<String, LocalApiMcpToolClientFailure>,
    ) -> Self {
        let Ok(describe_runtime_result_text) = describe_runtime_answer else {
            return Self::NotDescribed;
        };
        match tool_result_from::<DescribeRuntimeToolResult>(
            DESCRIBE_RUNTIME_TOOL_NAME,
            &describe_runtime_result_text,
        ) {
            Ok(DescribeRuntimeToolResult {
                runtime_log_directory: Some(runtime_log_directory),
                ..
            }) => Self::WrittenIn(runtime_log_directory),
            Ok(DescribeRuntimeToolResult {
                runtime_log_directory: None,
                ..
            }) => Self::NoneWritten,
            Err(_not_a_description) => Self::NotDescribed,
        }
    }

    /// `the runtime went away — it crashed or was stopped; …`, naming where its log is.
    pub(crate) fn the_runtime_went_away_message(&self) -> String {
        let runtime_log_directory = match self {
            Self::WrittenIn(runtime_log_directory) => Ok(runtime_log_directory.clone()),
            Self::NoneWritten => Err("it wrote no log of its own".to_owned()),
            Self::NotDescribed => TatolabStateDirectory::resolve_for_a_reader_without_creating()
                .map(|tatolab_state_directory| tatolab_state_directory.runtime_log_directory())
                .map_err(|state_directory_refusal| {
                    format!(
                        "its log is in the runtime's state directory, which this user cannot \
                         place: {state_directory_refusal}"
                    )
                }),
        };
        let where_its_log_is = match runtime_log_directory {
            Ok(runtime_log_directory) => format!(
                "its log is under {}",
                directory_with_its_trailing_slash(&runtime_log_directory)
            ),
            Err(no_log_folder_to_name) => no_log_folder_to_name,
        };
        format!("the runtime went away — it crashed or was stopped; {where_its_log_is}")
    }
}

/// Whether `failure` means the runtime is gone: nothing answers its socket, or the connection to
/// it closed.
pub(crate) fn the_runtime_is_gone(failure: &LocalApiMcpToolClientFailure) -> bool {
    matches!(
        failure.kind,
        LocalApiMcpToolClientFailureKind::LocalApiUnreachable
            | LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed
    )
}

fn directory_with_its_trailing_slash(directory: &Path) -> String {
    format!("{}/", directory.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_description_naming(runtime_log_directory: serde_json::Value) -> String {
        serde_json::json!({
            "version": "0.57.0",
            "build_id": "0.57.0+abc.1",
            "up_for_ms": 5,
            "runtime_log_directory": runtime_log_directory,
        })
        .to_string()
    }

    #[test]
    fn the_message_names_the_folder_the_runtime_described() {
        let described = RuntimeLogDirectoryAsDescribed::from_the_describe_runtime_answer(Ok(
            a_description_naming(serde_json::json!("/home/u/.local/state/tatolab/logs")),
        ));

        assert_eq!(
            described.the_runtime_went_away_message(),
            "the runtime went away — it crashed or was stopped; its log is under \
             /home/u/.local/state/tatolab/logs/"
        );
    }

    #[test]
    fn a_runtime_that_writes_no_log_is_said_to_write_none() {
        let described = RuntimeLogDirectoryAsDescribed::from_the_describe_runtime_answer(Ok(
            a_description_naming(serde_json::Value::Null),
        ));

        assert_eq!(described, RuntimeLogDirectoryAsDescribed::NoneWritten);
        assert_eq!(
            described.the_runtime_went_away_message(),
            "the runtime went away — it crashed or was stopped; it wrote no log of its own"
        );
    }

    #[test]
    fn an_answer_that_is_no_description_is_not_described() {
        for describe_runtime_answer in [
            Ok("{}".to_owned()),
            Err(LocalApiMcpToolClientFailure::local_api_unreachable(
                "nothing answers".to_owned(),
            )),
        ] {
            assert_eq!(
                RuntimeLogDirectoryAsDescribed::from_the_describe_runtime_answer(
                    describe_runtime_answer
                ),
                RuntimeLogDirectoryAsDescribed::NotDescribed
            );
        }
    }

    #[test]
    fn only_an_unreachable_runtime_or_a_closed_connection_is_a_runtime_gone() {
        assert!(the_runtime_is_gone(
            &LocalApiMcpToolClientFailure::local_api_unreachable("gone".to_owned())
        ));
        assert!(the_runtime_is_gone(
            &LocalApiMcpToolClientFailure::local_api_connection_closed("closed".to_owned())
        ));
        assert!(!the_runtime_is_gone(
            &LocalApiMcpToolClientFailure::tool_call_failed("refused".to_owned())
        ));
        assert!(!the_runtime_is_gone(
            &LocalApiMcpToolClientFailure::request_refused_by_the_runtime("refused".to_owned())
        ));
    }
}
