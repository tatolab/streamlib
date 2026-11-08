// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The asset tools driven through the real router over its socket into a real project on disk:
//! each change one commit on the project's history, an asset's UUID kept across a move.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use serde_json::{Value, json};
use streamlib_project_repository::{
    PROJECT_HISTORY_DIRECTORY, ProjectHistoryRepository, ProjectRepository,
    ProjectTransactionAuthor,
};

use crate::control_plane_stub_support::LocalApiServedOnAFreshSocket;
use crate::mcp::tests::{
    ControlPlaneMcpDispatchStubRuntime, first_text_block_json, mcp_transport_to,
};

struct AssetToolsServedOverAProject {
    project_parent_directory: tempfile::TempDir,
    _served: LocalApiServedOnAFreshSocket,
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
            _served: served,
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
        let request = CallToolRequestParams::new(tool_name.to_string())
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
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

#[tokio::test]
async fn media_usd_and_an_fmu_are_imported_renamed_moved_and_deleted_one_commit_each() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let project_directory = served.project_directory();
    let sources = [
        ("intro.mp4", "media/intro.mp4", "media"),
        ("robot.usd", "scenes/robot.usd", "scene"),
        ("arm.fmu", "models/arm.fmu", "data"),
    ];

    for (source_file_name, asset_path, kind) in sources {
        let source =
            served.a_file_outside_the_project(source_file_name, source_file_name.as_bytes());
        let imported = served
            .answered(
                "import_asset",
                json!({ "source": source, "asset_path": asset_path, "author": {"kind": "person", "name": "Jonathan"} }),
            )
            .await;
        let asset = imported["asset"]["asset"].as_str().unwrap().to_owned();
        assert_eq!(imported["asset"]["kind"], kind, "{imported}");
        assert_eq!(imported["asset"]["path"], asset_path);
        assert_eq!(imported["commit"].as_str().unwrap().len(), 40);
        assert!(exists_under(
            &project_directory,
            &format!("{asset_path}.asset.json")
        ));

        let renamed = served
            .answered(
                "rename_asset",
                json!({ "asset": asset, "file_name": format!("renamed-{source_file_name}") }),
            )
            .await;
        assert_eq!(renamed["asset"]["asset"], asset.as_str());

        let moved = served
            .answered(
                "move_asset",
                json!({ "asset": asset, "into_directory": "library" }),
            )
            .await;
        assert_eq!(
            moved["asset"]["asset"],
            asset.as_str(),
            "a move keeps the UUID"
        );
        assert_eq!(
            moved["asset"]["path"],
            format!("library/renamed-{source_file_name}")
        );
        let described = served
            .answered("describe_asset", json!({ "asset": asset }))
            .await;
        assert_eq!(described["content_held"], true);
        assert_eq!(described["asset"]["path"], moved["asset"]["path"]);

        let deleted = served
            .answered("delete_asset", json!({ "asset": asset }))
            .await;
        assert_eq!(deleted["asset"]["path"], moved["asset"]["path"]);
        assert!(!exists_under(
            &project_directory,
            &format!("library/renamed-{source_file_name}.asset.json")
        ));
    }

    let subjects = served.history_subjects_newest_first();
    assert_eq!(subjects.len(), 1 + 4 * sources.len(), "{subjects:?}");
    assert_eq!(subjects.last().unwrap(), "create_project arm-control");
    assert!(project_directory.join(PROJECT_HISTORY_DIRECTORY).is_dir());
    let listed = served.answered("list_assets", json!({})).await;
    assert_eq!(listed["assets"], json!([]));
    assert_eq!(listed["project_name"], "arm-control");
}

#[tokio::test]
async fn a_change_with_no_author_is_by_the_agent_its_client_is_and_kind_narrows_the_list() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let video = served.a_file_outside_the_project("intro.mp4", b"video");
    let lut = served.a_file_outside_the_project("grade.cube", b"lut");
    served
        .answered(
            "import_asset",
            json!({ "source": video, "asset_path": "media/intro.mp4" }),
        )
        .await;
    let imported_lut = served
        .answered(
            "import_asset",
            json!({ "source": lut, "asset_path": "luts/grade.cube", "kind": "data", "import_settings": {"interpolation": "tetrahedral"} }),
        )
        .await;
    let lut_asset = imported_lut["asset"]["asset"].as_str().unwrap().to_owned();
    let set = served
        .answered(
            "set_asset_settings",
            json!({ "asset": lut_asset, "import_settings": {"interpolation": "trilinear"} }),
        )
        .await;
    assert_eq!(
        set["asset"]["import_settings"]["interpolation"],
        "trilinear"
    );

    let media_only = served
        .answered("list_assets", json!({ "kind": "media" }))
        .await;
    assert_eq!(media_only["assets"].as_array().unwrap().len(), 1);
    assert_eq!(media_only["assets"][0]["path"], "media/intro.mp4");

    let history = ProjectHistoryRepository::open_for_project(&served.project_directory())
        .unwrap()
        .commits_newest_first()
        .unwrap();
    assert!(
        history[..3]
            .iter()
            .all(|commit| !commit.author_name.is_empty() && commit.author_name != "tatolab runtime"),
        "{history:?}"
    );
}

#[tokio::test]
async fn an_import_from_stored_content_reuses_the_bytes_by_their_hash() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let source = served.a_file_outside_the_project("intro.mp4", b"video bytes");
    let first = served
        .answered(
            "import_asset",
            json!({ "source": source, "asset_path": "media/intro.mp4" }),
        )
        .await;
    let content_sha256 = first["asset"]["content_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = served
        .answered(
            "import_asset",
            json!({ "source": content_sha256, "asset_path": "media/intro-copy.mp4" }),
        )
        .await;
    assert_eq!(second["asset"]["content_sha256"], content_sha256.as_str());
    assert_ne!(second["asset"]["asset"], first["asset"]["asset"]);
    assert_eq!(
        std::fs::read(served.project_directory().join("media/intro-copy.mp4")).unwrap(),
        b"video bytes"
    );
}

#[tokio::test]
async fn each_refusal_is_the_runtimes_own_words_and_writes_nothing() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let source = served.a_file_outside_the_project("intro.mp4", b"video");
    let imported = served
        .answered(
            "import_asset",
            json!({ "source": source, "asset_path": "media/intro.mp4" }),
        )
        .await;
    let asset = imported["asset"]["asset"].as_str().unwrap().to_owned();
    let commits_before = served.history_subjects_newest_first().len();

    let held = the_error_text_of(
        &served
            .tool_call(
                "import_asset",
                json!({ "source": source, "asset_path": "media/intro.mp4" }),
            )
            .await,
    );
    assert!(held.starts_with("import_asset failed: "), "{held}");
    assert!(held.contains(&asset), "{held}");

    let escaping = the_error_text_of(
        &served
            .tool_call(
                "import_asset",
                json!({ "source": source, "asset_path": "../outside.mp4" }),
            )
            .await,
    );
    assert!(escaping.contains("is not an asset path"), "{escaping}");

    let bad_source = the_error_text_of(
        &served
            .tool_call(
                "import_asset",
                json!({ "source": "intro.mp4", "asset_path": "media/other.mp4" }),
            )
            .await,
    );
    assert!(bad_source.contains("`source` must be"), "{bad_source}");

    std::fs::create_dir_all(served.project_directory().join("streams")).unwrap();
    std::fs::write(
        served.project_directory().join("streams/main.graph.json"),
        serde_json::to_vec(&json!({ "nodes": [{ "config": { "video": { "asset": asset } } }] }))
            .unwrap(),
    )
    .unwrap();
    let referenced = the_error_text_of(
        &served
            .tool_call("delete_asset", json!({ "asset": asset }))
            .await,
    );
    assert!(
        referenced.contains("streams/main.graph.json"),
        "{referenced}"
    );

    let not_an_asset = the_error_text_of(
        &served
            .tool_call("describe_asset", json!({ "asset": "not-a-uuid" }))
            .await,
    );
    assert!(
        not_an_asset.contains("must be an asset's UUID"),
        "{not_an_asset}"
    );

    assert_eq!(served.history_subjects_newest_first().len(), commits_before);
}

#[tokio::test]
async fn a_folder_with_no_project_file_and_a_relative_directory_are_refused() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let request = |project_directory: Value| {
        CallToolRequestParams::new("list_assets").with_arguments(
            json!({ "project_directory": project_directory })
                .as_object()
                .cloned()
                .unwrap(),
        )
    };
    let not_a_project = serde_json::to_value(
        served
            .client
            .call_tool(request(json!(served.project_parent_directory.path())))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        the_error_text_of(&not_a_project).contains("tatolab-project.json"),
        "{not_a_project}"
    );
    let relative = serde_json::to_value(
        served
            .client
            .call_tool(request(json!("arm-control")))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        the_error_text_of(&relative).contains("not an absolute path"),
        "{relative}"
    );
}
