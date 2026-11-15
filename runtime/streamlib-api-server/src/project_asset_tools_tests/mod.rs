// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The asset tools driven through the real router over its socket into a real project on disk:
//! each change one commit on the project's history, an asset's UUID kept across a move.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::model::{
    ClientCapabilities, ClientConfig, Implementation, ProtocolVersion, ReadResourceRequestParams,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use serde_json::{Value, json};
use streamlib_project_repository::{
    PROJECT_HISTORY_DIRECTORY, ProjectHistoryRepository, ProjectRepository,
    ProjectTransactionAuthor, content_sha256_of,
};

use crate::control_plane_stub_support::LocalApiServedOnAFreshSocket;
use crate::mcp::tests::{
    ControlPlaneMcpDispatchStubRuntime, first_text_block_json, mcp_transport_to,
};
use crate::mcp_stdio_upgrade::tests::upgraded_mcp_stdio_stream;

mod asset_import_and_edit_tests;
mod asset_media_facts_tests;
mod project_file_and_whole_project_operation_tests;
mod transaction_feed_and_client_identity_tests;

struct AssetToolsServedOverAProject {
    project_parent_directory: tempfile::TempDir,
    served: LocalApiServedOnAFreshSocket,
    client: RunningService<RoleClient, ()>,
}

impl AssetToolsServedOverAProject {
    async fn over_a_new_project() -> Self {
        let project_parent_directory = tempfile::tempdir().unwrap();
        ProjectRepository::create(
            &project_parent_directory.path().join("arm-control"),
            "arm-control",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let served =
            LocalApiServedOnAFreshSocket::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
        let client = ()
            .serve_with_lifecycle(
                mcp_transport_to(&served),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the runtime answers `server/discover`");
        Self {
            project_parent_directory,
            served,
            client,
        }
    }

    fn project_directory(&self) -> PathBuf {
        self.project_parent_directory.path().join("arm-control")
    }

    fn a_file_outside_the_project(&self, file_name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.project_parent_directory.path().join(file_name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    async fn tool_call(&self, tool_name: &str, mut arguments: Value) -> Value {
        arguments["project_directory"] = json!(self.project_directory());
        let request = crate::mcp::tests::the_tool_call_that_reaches(tool_name, &arguments);
        serde_json::to_value(self.client.call_tool(request).await.unwrap()).unwrap()
    }

    async fn answered(&self, tool_name: &str, arguments: Value) -> Value {
        first_text_block_json(&self.tool_call(tool_name, arguments).await)
    }

    fn history_subjects_newest_first(&self) -> Vec<String> {
        ProjectHistoryRepository::open_for_project(&self.project_directory())
            .unwrap()
            .commits_newest_first()
            .unwrap()
            .into_iter()
            .map(|commit| commit.message.lines().next().unwrap_or_default().to_owned())
            .collect()
    }
}

fn the_error_text_of(tool_result: &Value) -> String {
    assert_eq!(tool_result["isError"], true, "{tool_result}");
    tool_result["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

fn exists_under(project_directory: &Path, path: &str) -> bool {
    project_directory.join(path).exists()
}
