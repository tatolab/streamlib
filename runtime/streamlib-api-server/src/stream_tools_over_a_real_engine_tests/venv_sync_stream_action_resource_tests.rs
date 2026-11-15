// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A venv sync followed package by package through the stream action resources.

use super::*;

/// A run whose venv the runtime's uv makes is followed live through the resource of the actions
/// under way: a listen on it is told as uv starts each download, the resource then holds each
/// package uv is fetching with its size, and once the sync ends the action's own resource holds
/// what uv installed, each package at its version.
#[tokio::test(flavor = "multi_thread")]
async fn a_venv_sync_is_followed_package_by_package_through_the_stream_action_resources() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let downloads_released = uv_directory.path().join("released");
    let uv_writing_what_uv_writes = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_writing_what_uv_writes,
        &format!(
            "#!/bin/sh
printf 'Using CPython 3.13.18 interpreter at: /usr/local/bin/python3\\nCreating virtual environment at: .venv\\nResolved 8 packages in 776ms\\nDownloading numpy (15.9MiB)\\nDownloading pillow (6.8MiB)\\n' >&2
while [ ! -e '{released}' ]; do sleep 0.05; done
printf ' Downloaded pillow\\n Downloaded numpy\\nPrepared 4 packages in 12.50s\\nInstalled 4 packages in 8ms\\n + numpy==2.5.4\\n + packaging==26.3\\n + pillow==13.0.0\\n + six==1.17.0\\n' >&2
mkdir -p .venv/bin && printf '#!/bin/sh\\nexit 0\\n' > .venv/bin/python && chmod 755 .venv/bin/python
",
            released = downloads_released.display()
        ),
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_writing_what_uv_writes)
        .expect("the runtime is handed its uv");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let following_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let read_the_resource = |uri: String| {
        let following_connection = &following_connection;
        async move {
            let read = following_connection
                .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
                .await
                .expect("the resource reads");
            let read = serde_json::to_value(read).unwrap();
            serde_json::from_str::<Value>(read["contents"][0]["text"].as_str().unwrap()).unwrap()
        }
    };
    let mut subscription = following_connection
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription("streamlib://stream-actions".to_owned())
                .build(),
        )
        .await
        .expect("the runtime acknowledges a listen on the actions under way");

    let queued = tool_answer(
        &tool_call_over_the_connection(
            &following_connection,
            "open_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let downloading = loop {
        let update = tokio::time::timeout_at(deadline.into(), subscription.next())
            .await
            .expect("each change of the action is announced")
            .expect("the listen is still open")
            .expect("the update is well formed");
        let rmcp::model::ServerNotification::ResourceUpdatedNotification(update) = update else {
            panic!("expected notifications/resources/updated, got {update:?}");
        };
        assert_eq!(update.params.uri, "streamlib://stream-actions");
        let under_way = read_the_resource("streamlib://stream-actions".to_owned()).await;
        let the_action = &under_way["actions"][0];
        if the_action["venv_sync"]["downloads_started"] == 2 {
            break the_action.clone();
        }
    };
    assert_eq!(downloading["action"], action.as_str());
    assert_eq!(downloading["step"], "making_the_venv");
    assert_eq!(
        downloading["venv_sync"],
        json!({
            "phase": "downloading",
            "packages_resolved": 8,
            "packages_prepared": null,
            "packages_installed": null,
            "downloads_started": 2,
            "downloads_done": 0,
            "packages": [
                {
                    "name": "numpy", "version": null, "kind": "distribution",
                    "state": "downloading", "download_size_bytes": 16_672_358,
                },
                {
                    "name": "pillow", "version": null, "kind": "distribution",
                    "state": "downloading", "download_size_bytes": 7_130_317,
                },
            ],
        })
    );

    std::fs::write(&downloads_released, "").expect("the downloads are released");
    the_stream_action_once_finished(&engine.operations_on_the_loaded_streams(), &action).await;
    let finished = read_the_resource(format!("streamlib://stream-action/{action}")).await;
    let venv_sync = &finished["venv_sync"];
    assert_eq!(
        (
            &venv_sync["phase"],
            &venv_sync["packages_prepared"],
            &venv_sync["packages_installed"],
            &venv_sync["downloads_done"],
        ),
        (&json!("installed"), &json!(4), &json!(4), &json!(2)),
        "{finished}"
    );
    let installed: Vec<(&str, &str, &str)> = venv_sync["packages"]
        .as_array()
        .expect("the packages uv named")
        .iter()
        .map(|package| {
            (
                package["name"].as_str().unwrap(),
                package["version"].as_str().unwrap(),
                package["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        installed,
        [
            ("numpy", "2.5.4", "installed"),
            ("pillow", "13.0.0", "installed"),
            ("packaging", "26.3", "installed"),
            ("six", "1.17.0", "installed"),
        ]
    );
    assert_eq!(
        read_the_resource("streamlib://stream-actions".to_owned()).await,
        json!({ "actions": [] })
    );
}
