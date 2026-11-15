// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Project files written, read, listed, saved and undone over the socket, a
//! node file made from the template, and a project renamed, described, covered,
//! duplicated and deleted.

use super::*;

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
