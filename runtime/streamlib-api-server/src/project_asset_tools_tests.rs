// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The asset tools driven through the real router over its socket into a real project on disk:
//! each change one commit on the project's history, an asset's UUID kept across a move.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    ReadResourceRequestParams,
};
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
use crate::mcp_stdio_upgrade::tests::upgraded_mcp_stdio_stream;

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

        let renamed_path = match asset_path.rsplit_once('/') {
            Some((folder, _)) => format!("{folder}/renamed-{source_file_name}"),
            None => format!("renamed-{source_file_name}"),
        };
        let renamed = served
            .answered(
                "relocate_asset",
                json!({ "asset": asset, "to_path": renamed_path }),
            )
            .await;
        assert_eq!(renamed["asset"]["asset"], asset.as_str());
        assert_eq!(renamed["asset"]["path"], renamed_path);

        let moved = served
            .answered(
                "relocate_asset",
                json!({ "asset": asset, "to_path": format!("library/renamed-{source_file_name}") }),
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
        assert_eq!(described["file_at_its_path"], true);
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

/// Each asset action a tool is sent is what its commit records, field for field, once the
/// project, the author and the key that carry it are set aside.
#[tokio::test]
async fn an_asset_action_sent_over_mcp_is_the_action_its_commit_records() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let source = served.a_file_outside_the_project("intro.mp4", b"video");
    let import = json!({
        "source": source, "asset_path": "media/intro.mp4", "kind": "media",
        "import_settings": { "colour_space": "rec709" },
    });
    let imported = served.answered("import_asset", import.clone()).await;
    let asset = imported["asset"]["asset"].as_str().unwrap().to_owned();
    let relocate = json!({ "asset": asset, "to_path": "archive/opening.mp4" });
    served
        .answered(
            "relocate_asset",
            json!({ "asset": asset, "to_path": "archive/opening.mp4", "idempotency_key": "drag-1" }),
        )
        .await;
    let set_settings = json!({ "asset": asset, "import_settings": { "colour_space": "srgb" } });
    served
        .answered("set_asset_settings", set_settings.clone())
        .await;
    let delete = json!({ "asset": asset });
    served.answered("delete_asset", delete.clone()).await;

    let recorded_actions_oldest_first: Vec<Value> =
        ProjectRepository::open(&served.project_directory())
            .unwrap()
            .applied_transactions_newest_first(4)
            .unwrap()
            .into_iter()
            .rev()
            .map(|applied| serde_json::to_value(&applied.transaction.actions).unwrap())
            .collect();
    let sent_as = |operation: &str, mut sent: Value| {
        sent["operation"] = json!(operation);
        json!([sent])
    };
    assert_eq!(
        recorded_actions_oldest_first,
        vec![
            sent_as("import_asset", import),
            sent_as("relocate_asset", relocate),
            sent_as("set_asset_settings", set_settings),
            sent_as("delete_asset", delete),
        ]
    );
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
async fn an_import_of_an_upload_moves_it_to_the_assets_path_by_its_hash() {
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
    let upload = streamlib_project_repository::the_upload_file_of(
        &served.project_directory(),
        &content_sha256,
    );
    std::fs::create_dir_all(upload.parent().unwrap()).unwrap();
    std::fs::write(&upload, b"video bytes").unwrap();

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
    assert!(!upload.exists());
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

#[tokio::test]
async fn a_listen_over_mcp_stdio_is_told_of_each_transaction_and_reads_it_from_the_feed() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let listed = served.answered("list_assets", json!({})).await;
    let transactions_resource_uri = listed["transactions_resource_uri"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        transactions_resource_uri,
        format!(
            "streamlib://project/{}/transactions",
            listed["project"].as_str().unwrap()
        )
    );

    let listener =
        crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream(&served.served).await;
    let mut subscription = listener
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription(transactions_resource_uri.clone())
                .resource_subscription("streamlib://graph")
                .build(),
        )
        .await
        .expect("the runtime acknowledges a listen on a project's change feed");
    assert_eq!(
        subscription.acknowledged().resource_subscriptions,
        Some(vec![transactions_resource_uri.clone()]),
        "only a project's change feed is subscribed"
    );

    let source = served.a_file_outside_the_project("intro.mp4", b"video");
    let imported = served
        .answered(
            "import_asset",
            json!({ "source": source, "asset_path": "media/intro.mp4", "author": {"kind": "person", "name": "Jonathan"} }),
        )
        .await;

    let update = tokio::time::timeout(std::time::Duration::from_secs(5), subscription.next())
        .await
        .expect("the import is announced")
        .unwrap()
        .expect("the listen is still open");
    let rmcp::model::ServerNotification::ResourceUpdatedNotification(update) = update else {
        panic!("expected notifications/resources/updated, got {update:?}");
    };
    assert_eq!(update.params.uri, transactions_resource_uri);

    let read = listener
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            transactions_resource_uri.clone(),
        ))
        .await
        .unwrap();
    let read = serde_json::to_value(read).unwrap();
    let feed: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(feed["project"], listed["project"]);
    assert_eq!(feed["transactions"].as_array().unwrap().len(), 1, "{feed}");
    let newest = &feed["transactions"][0];
    assert_eq!(newest["transaction"], imported["transaction"]);
    assert_eq!(newest["commit"], imported["commit"]);
    assert_eq!(newest["author_kind"], "person");
    assert_eq!(newest["author_name"], "Jonathan");
    assert_eq!(newest["operations"][0]["operation"], "import_asset");
    assert_eq!(newest["inverse_operations"][0]["operation"], "delete_asset");
    assert_eq!(newest["status"], "applied");
    assert_eq!(newest["format_version"], 2);
    assert!(newest.get("idempotency_key").is_none(), "{newest}");
    let import_commit = ProjectHistoryRepository::open_for_project(&served.project_directory())
        .unwrap()
        .commits_newest_first()
        .unwrap()
        .remove(0);
    assert_eq!(newest["commit"], import_commit.commit);
    assert_eq!(
        newest["applied_at"],
        crate::mcp_project_change_feed::rfc_3339_utc_of_unix_seconds(
            import_commit.committed_at_unix_seconds
        ),
        "applied_at is when the import's commit was made"
    );
}

#[tokio::test]
async fn the_feed_of_a_project_no_tool_has_opened_is_refused_naming_how_to_open_it() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let refused = served
        .client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(format!(
            "streamlib://project/{}/transactions",
            uuid::Uuid::new_v4()
        )))
        .await
        .expect_err("a project no tool opened has no feed to read");
    assert!(refused.to_string().contains("list_assets"), "{refused}");
}

#[tokio::test]
async fn an_authorless_edit_over_mcp_and_mcp_stdio_is_recorded_under_the_requests_client_info_name()
{
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let transactions_resource_uri =
        served.answered("list_assets", json!({})).await["transactions_resource_uri"]
            .as_str()
            .unwrap()
            .to_owned();
    let claude_code_client_config = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "2.1.0"),
    );
    let discover_the_latest_revision = || ClientLifecycleMode::Discover {
        preferred_versions: vec![ProtocolVersion::LATEST],
    };
    let clients_by_transport = [
        (
            "/mcp",
            claude_code_client_config
                .clone()
                .serve_with_lifecycle(
                    mcp_transport_to(&served.served),
                    discover_the_latest_revision(),
                )
                .await
                .unwrap(),
        ),
        (
            "/mcp/stdio",
            claude_code_client_config
                .clone()
                .serve_with_lifecycle(
                    upgraded_mcp_stdio_stream(&served.served).await,
                    discover_the_latest_revision(),
                )
                .await
                .unwrap(),
        ),
    ];

    for (transport, client) in clients_by_transport {
        let source = served.a_file_outside_the_project("intro.mp4", transport.as_bytes());
        let request = CallToolRequestParams::new("import_asset").with_arguments(
            json!({
                "project_directory": served.project_directory(),
                "source": source,
                "asset_path": format!("media/intro-over-{}.mp4", transport.replace('/', "-")),
            })
            .as_object()
            .cloned()
            .unwrap(),
        );
        let imported = first_text_block_json(
            &serde_json::to_value(client.call_tool(request).await.unwrap()).unwrap(),
        );

        let read = client
            .read_resource(ReadResourceRequestParams::new(
                transactions_resource_uri.clone(),
            ))
            .await
            .unwrap();
        let read = serde_json::to_value(read).unwrap();
        let feed: Value =
            serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
        let recorded = feed["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|transaction| transaction["transaction"] == imported["transaction"])
            .unwrap_or_else(|| panic!("{transport}: the import is on the feed: {feed}"));
        assert_eq!(recorded["author_kind"], "agent", "{transport}: {recorded}");
        assert_eq!(
            recorded["author_name"], "claude-code",
            "{transport}: {recorded}"
        );
    }
}
