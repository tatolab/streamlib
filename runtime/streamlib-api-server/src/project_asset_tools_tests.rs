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
async fn an_import_with_no_path_lands_in_its_kinds_folder_numbered_past_a_taken_name() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let project_directory = served.project_directory();
    let imports = [
        ("intro.mp4", "media/intro.mp4"),
        ("cover.png", "media/cover.png"),
        ("robot.usd", "scenes/robot.usd"),
        ("arm.fmu", "data/arm.fmu"),
        ("grade.cube", "data/grade.cube"),
    ];
    for (source_file_name, standard_path) in imports {
        let source =
            served.a_file_outside_the_project(source_file_name, source_file_name.as_bytes());
        let imported = served
            .answered("import_asset", json!({ "source": source }))
            .await;
        assert_eq!(imported["asset"]["path"], standard_path, "{imported}");
        assert!(exists_under(&project_directory, standard_path));
    }

    let another_intro = served.a_file_outside_the_project("intro.mp4", b"a second take");
    let second = served
        .answered("import_asset", json!({ "source": another_intro }))
        .await;
    assert_eq!(second["asset"]["path"], "media/intro-2.mp4", "{second}");
    let third = served
        .answered("import_asset", json!({ "source": another_intro }))
        .await;
    assert_eq!(third["asset"]["path"], "media/intro-3.mp4", "{third}");

    let named = served
        .answered(
            "import_asset",
            json!({ "source": another_intro, "file_name": "opening.mp4" }),
        )
        .await;
    assert_eq!(named["asset"]["path"], "media/opening.mp4", "{named}");
}

#[tokio::test]
async fn an_upload_imported_with_no_path_needs_a_file_name() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let content_sha256 = "0".repeat(64);
    let refused = served
        .tool_call("import_asset", json!({ "source": content_sha256 }))
        .await;
    assert!(
        the_error_text_of(&refused).contains("needs a `file_name`"),
        "{refused}"
    );
    let refused = served
        .tool_call(
            "import_asset",
            json!({ "source": content_sha256, "file_name": "media/intro.mp4" }),
        )
        .await;
    assert!(
        the_error_text_of(&refused).contains("names a folder"),
        "{refused}"
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
        crate::mcp::tests::the_tool_call_that_reaches(
            "list_assets",
            &json!({ "project_directory": project_directory }),
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
    let applied: Vec<&Value> = feed["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "applied")
        .collect();
    assert_eq!(applied.len(), 1, "{feed}");
    let newest = applied[0];
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
        let request = crate::mcp::tests::the_tool_call_that_reaches(
            "import_asset",
            &json!({
                "project_directory": served.project_directory(),
                "source": source,
                "asset_path": format!("media/intro-over-{}.mp4", transport.replace('/', "-")),
            }),
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

/// `replace_asset` puts new bytes at an asset's path under the same UUID, and its undo puts
/// the earlier bytes and sidecar back.
#[tokio::test]
async fn a_replaced_asset_keeps_its_uuid_and_its_undo_restores_the_earlier_take() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let take_one = served.a_file_outside_the_project("take-one.mp4", b"take one");
    let imported = served
        .answered(
            "import_asset",
            json!({ "source": take_one, "asset_path": "media/intro.mp4" }),
        )
        .await;
    let asset = imported["asset"]["asset"].as_str().unwrap().to_owned();
    let take_two = served.a_file_outside_the_project("take-two.mp4", b"take two, longer");

    let replaced = served
        .answered(
            "replace_asset",
            json!({ "asset": asset, "source": take_two }),
        )
        .await;

    assert_eq!(replaced["asset"]["asset"], asset.as_str());
    assert_eq!(replaced["asset"]["path"], "media/intro.mp4");
    assert_eq!(replaced["asset"]["content_byte_len"], 16);
    assert_ne!(
        replaced["asset"]["content_sha256"],
        imported["asset"]["content_sha256"]
    );
    let file = served.project_directory().join("media/intro.mp4");
    assert_eq!(std::fs::read(&file).unwrap(), b"take two, longer");
    assert_eq!(
        served.history_subjects_newest_first()[0],
        "replace_asset media/intro.mp4"
    );

    served.answered("undo", json!({})).await;

    let described = served
        .answered("describe_asset", json!({ "asset": asset }))
        .await;
    for field in imported["asset"].as_object().unwrap().keys() {
        assert_eq!(
            described["asset"][field], imported["asset"][field],
            "{field}: {described}"
        );
    }
    assert_eq!(std::fs::read(&file).unwrap(), b"take one");
}

/// `write_project_file` writes a file of the project as one commit that `undo` reverses,
/// `read_project_file` answers its bytes whole, and a path outside the project is refused.
#[tokio::test]
async fn a_project_file_written_over_the_socket_is_read_back_listed_and_undone() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;

    let written = served
        .answered(
            "write_project_file",
            json!({ "path": "views/panel/logo.png", "content_base64": "iVBORw0KGgoA//6A" }),
        )
        .await;
    assert_eq!(written["path"], "views/panel/logo.png");
    assert_eq!(
        served.history_subjects_newest_first()[0],
        "write_project_file views/panel/logo.png"
    );
    let read = served
        .answered(
            "read_project_file",
            json!({ "path": "views/panel/logo.png" }),
        )
        .await;
    assert_eq!(read["content_base64"], "iVBORw0KGgoA//6A");
    assert_eq!(read["byte_len"], 12);
    let listed = served
        .answered("list_project_files", json!({ "under": "views" }))
        .await;
    assert_eq!(
        listed["files"],
        json!([{ "path": "views/panel/logo.png", "byte_len": 12 }])
    );

    served.answered("undo", json!({})).await;

    assert!(
        !served
            .project_directory()
            .join("views/panel/logo.png")
            .exists()
    );
    let refused = the_error_text_of(
        &served
            .tool_call(
                "write_project_file",
                json!({ "path": "../outside.py", "content": "x" }),
            )
            .await,
    );
    assert!(
        refused.starts_with("write_project_file failed: "),
        "{refused}"
    );
    assert!(
        !served
            .project_parent_directory
            .path()
            .join("outside.py")
            .exists()
    );
}

/// A `write_project_file` carrying the `content_sha256` its `read_project_file` answered is
/// refused in the runtime's words once another write changed the file, leaving the file as the
/// other write left it; the hash the read and each write answer chains the next save.
#[tokio::test]
async fn a_project_file_save_carrying_a_stale_hash_is_refused_over_the_socket() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let first_written = served
        .answered(
            "write_project_file",
            json!({ "path": "nodes/camera.py", "content": "as read\n" }),
        )
        .await;
    let read = served
        .answered("read_project_file", json!({ "path": "nodes/camera.py" }))
        .await;
    assert_eq!(read["content_sha256"], first_written["content_sha256"]);
    assert_eq!(
        read["content_sha256"],
        content_sha256_of(b"as read\n").as_str()
    );
    let saved = served
        .answered(
            "write_project_file",
            json!({
                "path": "nodes/camera.py",
                "content": "saved\n",
                "expected_sha256": read["content_sha256"],
            }),
        )
        .await;
    served
        .answered(
            "write_project_file",
            json!({ "path": "nodes/camera.py", "content": "by the agent\n" }),
        )
        .await;

    let refused = the_error_text_of(
        &served
            .tool_call(
                "write_project_file",
                json!({
                    "path": "nodes/camera.py",
                    "content": "the studio save\n",
                    "expected_sha256": saved["content_sha256"],
                }),
            )
            .await,
    );
    assert_eq!(
        refused,
        "write_project_file failed: `nodes/camera.py` changed since it was read, so this write \
         would overwrite that change; nothing was written. Read the file again to see what it \
         holds now"
    );
    assert_eq!(
        std::fs::read(served.project_directory().join("nodes/camera.py")).unwrap(),
        b"by the agent\n"
    );

    let already_held = the_error_text_of(
        &served
            .tool_call(
                "write_project_file",
                json!({ "path": "nodes/camera.py", "content": "by the agent\n" }),
            )
            .await,
    );
    assert_eq!(
        already_held,
        "write_project_file failed: `nodes/camera.py` already holds this content, so the write \
         has nothing to change"
    );
}

/// `create_node_file` writes the runtime's node template, its class named after the file, as
/// one commit that `undo` takes out, and refuses a path that already holds a file or names no
/// importable module.
#[tokio::test]
async fn a_node_file_created_from_the_template_names_its_node_type_and_undoes() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;

    let created = served
        .answered("create_node_file", json!({ "path": "nodes/edge_glow.py" }))
        .await;

    assert_eq!(created["path"], "nodes/edge_glow.py", "{created}");
    assert_eq!(
        created["node_type"], "nodes.edge_glow:EdgeGlow",
        "{created}"
    );
    assert_eq!(
        served.history_subjects_newest_first()[0],
        "write_project_file nodes/edge_glow.py"
    );
    let node_file = served.project_directory().join("nodes/edge_glow.py");
    let content = std::fs::read_to_string(&node_file).unwrap();
    assert!(content.contains("@node.meta(description="), "{content}");
    assert!(
        content.contains("@node.process\nclass EdgeGlow:\n"),
        "{content}"
    );

    let refused_over_a_file = the_error_text_of(
        &served
            .tool_call("create_node_file", json!({ "path": "nodes/edge_glow.py" }))
            .await,
    );
    assert!(
        refused_over_a_file.starts_with("create_node_file failed: ")
            && refused_over_a_file.contains("already holds `nodes/edge_glow.py`"),
        "{refused_over_a_file}"
    );
    assert_eq!(std::fs::read_to_string(&node_file).unwrap(), content);
    let refused_as_no_module = the_error_text_of(
        &served
            .tool_call("create_node_file", json!({ "path": "nodes/edge-glow.py" }))
            .await,
    );
    assert!(
        refused_as_no_module.starts_with("create_node_file failed: "),
        "{refused_as_no_module}"
    );
    assert!(!exists_under(
        &served.project_directory(),
        "nodes/edge-glow.py"
    ));

    served.answered("undo", json!({})).await;

    assert!(!node_file.exists());
}

#[tokio::test]
async fn a_project_is_renamed_described_and_covered_and_list_assets_serves_each() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let listed_before = served.answered("list_assets", json!({})).await;
    assert_eq!(listed_before["project_description"], Value::Null);
    assert_eq!(listed_before["project_cover"], Value::Null);
    let created_at = listed_before["project_last_edited_at"].as_i64().unwrap();

    let renamed = served
        .answered("rename_project", json!({ "name": "Arm control" }))
        .await;
    assert_eq!(renamed["project_name"], "Arm control");
    assert_eq!(renamed["project"], listed_before["project"]);
    let described = served
        .answered(
            "set_project_description",
            json!({ "description": "Two arms and a camera" }),
        )
        .await;
    assert_eq!(described["project_description"], "Two arms and a camera");
    let image = served.a_file_outside_the_project("cover.png", b"png");
    let imported = served
        .answered(
            "import_asset",
            json!({ "source": image, "asset_path": "assets/cover.png" }),
        )
        .await;
    let cover = imported["asset"]["asset"].as_str().unwrap().to_owned();
    let covered = served
        .answered("set_project_cover", json!({ "cover": cover }))
        .await;
    assert_eq!(covered["project_cover"], cover.as_str());

    let listed = served.answered("list_assets", json!({})).await;
    assert_eq!(listed["project_name"], "Arm control");
    assert_eq!(listed["project_description"], "Two arms and a camera");
    assert_eq!(listed["project_cover"], cover.as_str());
    assert!(listed["project_last_edited_at"].as_i64().unwrap() >= created_at);
    let subjects = served.history_subjects_newest_first();
    assert_eq!(
        &subjects[..4],
        [
            "set_project_cover",
            "import_asset assets/cover.png",
            "set_project_description",
            "rename_project Arm control"
        ],
        "{subjects:?}"
    );

    let blank = served
        .tool_call("rename_project", json!({ "name": " " }))
        .await;
    assert!(the_error_text_of(&blank).contains("cannot be blank"));
    let deleted_cover = served
        .tool_call("delete_asset", json!({ "asset": cover }))
        .await;
    assert!(the_error_text_of(&deleted_cover).contains("the project's cover"));
}

#[tokio::test]
async fn a_project_is_duplicated_with_its_history_under_a_new_uuid() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let original = served.answered("list_assets", json!({})).await;
    let into = served
        .project_parent_directory
        .path()
        .join("arm-control-copy");

    let duplicated = served
        .answered("duplicate_project", json!({ "into": into }))
        .await;

    assert_ne!(duplicated["project"], original["project"]);
    assert_eq!(duplicated["project_name"], "arm-control copy");
    assert_eq!(duplicated["commit"].as_str().unwrap().len(), 40);
    assert!(into.join(PROJECT_HISTORY_DIRECTORY).is_dir());
    let again = served
        .tool_call("duplicate_project", json!({ "into": into }))
        .await;
    assert!(the_error_text_of(&again).contains("already exists"));
}

#[tokio::test]
async fn a_delete_project_is_queued_on_the_runtime_that_runs_the_stream_actions() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let refused = served.tool_call("delete_project", json!({})).await;
    assert!(the_error_text_of(&refused).contains("delete_project"));
    assert!(served.project_directory().exists());
}

/// Every `media_facts_read` entry on the project's change feed, by asset UUID.
async fn the_assets_whose_media_facts_the_feed_says_were_read(
    listener: &RunningService<RoleClient, ()>,
    transactions_resource_uri: &str,
) -> Vec<String> {
    let read = listener
        .read_resource(ReadResourceRequestParams::new(
            transactions_resource_uri.to_owned(),
        ))
        .await
        .unwrap();
    let read = serde_json::to_value(read).unwrap();
    let feed: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    feed["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"] == "media_facts_read")
        .map(|entry| entry["asset"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_imported_image_lut_and_fmu_are_read_once_announced_and_described_with_their_facts() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let transactions_resource_uri =
        served.answered("list_assets", json!({})).await["transactions_resource_uri"]
            .as_str()
            .unwrap()
            .to_owned();
    let listener =
        crate::mcp_stdio_upgrade::tests::rmcp_client_over_the_upgraded_stream(&served.served).await;
    let mut subscription = listener
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription(transactions_resource_uri.clone())
                .build(),
        )
        .await
        .unwrap();

    let cover = served.project_parent_directory.path().join("cover.png");
    image::RgbImage::from_pixel(1024, 512, image::Rgb([10, 120, 200]))
        .save(&cover)
        .unwrap();
    let grade = served
        .a_file_outside_the_project("grade.cube", b"TITLE \"grade\"\nLUT_3D_SIZE 33\n0 0 0\n");
    let arm = served.a_file_outside_the_project("arm.fmu", b"an FMU");
    let mut imported_assets = Vec::new();
    for source in [&cover, &grade, &arm] {
        let imported = served
            .answered("import_asset", json!({ "source": source }))
            .await;
        imported_assets.push(imported["asset"]["asset"].as_str().unwrap().to_owned());
    }
    let [cover_asset, grade_asset, arm_asset] = imported_assets.clone().try_into().unwrap();

    loop {
        let read_assets = the_assets_whose_media_facts_the_feed_says_were_read(
            &listener,
            &transactions_resource_uri,
        )
        .await;
        if imported_assets
            .iter()
            .all(|asset| read_assets.contains(asset))
        {
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), subscription.next())
            .await
            .expect("each read is announced on the change feed")
            .unwrap()
            .expect("the listen is still open");
    }

    let cover_described = served
        .answered("describe_asset", json!({ "asset": cover_asset }))
        .await;
    let cover_described = &cover_described["asset"];
    assert_eq!(
        cover_described["media_facts_state"], "read",
        "{cover_described}"
    );
    assert_eq!(
        cover_described["media_facts"]["image"],
        json!({ "pixel_width": 1024, "pixel_height": 512 })
    );
    assert_eq!(
        cover_described["preview"],
        json!({ "pixel_width": 512, "pixel_height": 256, "media_type": "image/png" })
    );
    let preview = served
        .answered("read_asset_preview", json!({ "asset": cover_asset }))
        .await;
    assert_eq!(preview["media_type"], "image/png");
    let png_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        preview["content_base64"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(preview["byte_len"], png_bytes.len());
    let decoded = image::load_from_memory(&png_bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (512, 256));
    let content_sha256 = cover_described["content_sha256"].as_str().unwrap();
    assert!(exists_under(
        &served.project_directory(),
        &format!(".tatolab/asset-media/{content_sha256}/png/preview.png")
    ));

    let grade_described = served
        .answered("describe_asset", json!({ "asset": grade_asset }))
        .await;
    assert_eq!(
        grade_described["asset"]["media_facts"]["lut"]["lut_3d_size"], 33,
        "{grade_described}"
    );
    assert_eq!(grade_described["asset"]["preview"], Value::Null);
    let refused = served
        .tool_call("read_asset_preview", json!({ "asset": grade_asset }))
        .await;
    assert!(
        the_error_text_of(&refused).contains("has no preview picture"),
        "{refused}"
    );

    let arm_described = served
        .answered("describe_asset", json!({ "asset": arm_asset }))
        .await;
    assert_eq!(arm_described["asset"]["media_facts_state"], "unreadable");
    assert_eq!(
        arm_described["asset"]["media_facts_unreadable_because"],
        "the runtime has no reader for `.fmu` files"
    );
    assert_eq!(arm_described["asset"]["media_facts"], Value::Null);

    let listed = served
        .answered("list_assets", json!({ "kind": "media" }))
        .await;
    assert_eq!(listed["assets"][0]["asset"], cover_asset.as_str());
    assert_eq!(listed["assets"][0]["media_facts_state"], "read", "{listed}");
    assert_eq!(
        listed["assets"][0]["media_facts"]["image"]["pixel_width"],
        1024
    );
}

#[tokio::test]
async fn a_stream_asset_carries_no_media_facts_and_has_no_preview() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    served
        .answered(
            "create_stream",
            json!({ "path": "streams/main.graph.json", "display_name": "Main" }),
        )
        .await;
    let listed = served
        .answered("list_assets", json!({ "kind": "stream" }))
        .await;
    let main = &listed["assets"][0];
    assert_eq!(main["kind"], "stream", "{listed}");
    assert_eq!(main["media_facts_state"], Value::Null);
    let refused = served
        .tool_call("read_asset_preview", json!({ "asset": main["asset"] }))
        .await;
    assert!(
        the_error_text_of(&refused).contains("is a stream, which has no preview"),
        "{refused}"
    );
}
