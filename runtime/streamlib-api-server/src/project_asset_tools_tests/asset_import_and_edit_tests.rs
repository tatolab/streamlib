// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Assets imported, replaced, renamed, moved and deleted over the socket: one
//! commit each, the action each commit records, where an import with no path
//! lands, and the refusals that write nothing.

use super::*;

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
        serde_json::to_vec(
            &json!({ "nodes": [{ "input_values": { "video": { "asset": asset } } }] }),
        )
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
