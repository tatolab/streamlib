// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The transaction feed a listen over MCP stdio reads, the feed refused for a
//! project no tool has opened, and the client name an authorless edit is
//! recorded under.

use super::*;

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
    assert_eq!(
        newest["format_version"],
        streamlib_project_repository::PROJECT_TRANSACTION_FORMAT_VERSION
    );
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
