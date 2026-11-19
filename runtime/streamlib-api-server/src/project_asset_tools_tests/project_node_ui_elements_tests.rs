// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The controls and node bodies a project registers, listed over the socket from its manifest.

use super::*;

#[tokio::test]
async fn each_element_is_listed_with_its_module_and_file_and_the_manifest_as_written() {
    let served = AssetToolsServedOverAProject::over_a_new_project().await;
    let node_ui_folder = served.project_directory().join("node_ui");
    std::fs::create_dir_all(&node_ui_folder).unwrap();
    let manifest = json!({
        "contract": "1.1",
        "controls": [{ "type": "float", "control": "knob", "script": "knob.ts" }],
        "node_bodies": [{ "node_type": "crossfade:Crossfade", "script": "crossfade.ts" }],
    });
    std::fs::write(
        node_ui_folder.join("tatolab_node_ui.json"),
        manifest.to_string(),
    )
    .unwrap();

    let listed = served
        .answered("list_project_node_ui_elements", json!({}))
        .await;

    let module = json!({
        "project": listed["project"],
        "project_name": listed["project_name"],
        "import_folder": null,
    });
    assert_eq!(
        listed["elements"],
        json!([
            {
                "module": module,
                "script_path": "node_ui/knob.ts",
                "element": { "kind": "control", "pin_type": "float", "control": "knob" },
            },
            {
                "module": module,
                "script_path": "node_ui/crossfade.ts",
                "element": { "kind": "node_body", "node_type": "crossfade:Crossfade" },
            },
        ]),
        "{listed}"
    );
    assert_eq!(
        listed["manifests"],
        json!([{
            "module": module,
            "manifest_path": "node_ui/tatolab_node_ui.json",
            "manifest": manifest,
        }])
    );
    assert_eq!(listed["unreadable_manifests"], json!([]));
}
