// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `logs`: a project's log subscriber is sent each new record in order, and `logs` pages a
//! project's records by stream and cursor.

use super::*;

/// A subscriber to a project's log, through a symlink to it, is sent each record written after it
/// subscribed in `project_log_records_written`, naming the project as it spelled it, each
/// notification following the previous one's cursor, past 500 records as several; `logs` from the
/// first's `after` `until` the last's `next_after` reads the same records, and a project it did
/// not subscribe to is never named.
///
/// Mental-revert: without the drain worker's announcement the wait below times out, without the
/// connection's filter the other project's write is sent, naming the project with symlinks
/// resolved sends a directory the subscriber never named, and a push of every record to one
/// notification sends one past 500.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_log_subscriber_is_sent_each_new_record_in_order_and_logs_reads_the_same() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let another_project_directory = tempfile::tempdir().expect("another project directory");
    let engine = Runner::new().expect("the engine builds");
    let stream = engine
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "logged",
        ))
        .expect("the stream loads");
    let stream_of_another_project = engine
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            another_project_directory.path(),
            "elsewhere",
        ))
        .expect("the other stream loads");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    )
    .await;
    let symlinks_directory = tempfile::tempdir().expect("a directory for a symlink");
    let project_directory_through_a_symlink = symlinks_directory.path().join("logged-project");
    std::os::unix::fs::symlink(
        project_directory.path(),
        &project_directory_through_a_symlink,
    )
    .expect("the symlink to the project is made");
    let project_directory_as_subscribed = project_directory_through_a_symlink.display().to_string();
    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_log_records",
                      "params": { "project_directories": [project_directory_as_subscribed] } }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut subscriber).await["result"],
        json!({ "project_directories": [project_directory_as_subscribed] })
    );

    stream_of_another_project.log_route().run_entered(|| {
        tracing::info!("project-log-push-token-of-another-project");
    });
    stream_of_another_project
        .log_route()
        .write_every_record_it_emitted_and_sync();
    let records_written_in_one_burst = PROJECT_LOG_RECORDS_PER_NOTIFICATION + 20;
    stream.log_route().run_entered(|| {
        for token_index in 0..records_written_in_one_burst {
            tracing::info!("project-log-push-token-{token_index}");
        }
    });
    stream.log_route().write_every_record_it_emitted_and_sync();

    let mut project_log_records_pushed = Vec::new();
    while let Ok(written) = tokio::time::timeout(
        Duration::from_secs(1),
        the_next_notification_under(&mut subscriber, "project_log_records_written"),
    )
    .await
    {
        project_log_records_pushed.push(written);
    }
    assert!(
        project_log_records_pushed.len() >= 2,
        "a burst past 500 records arrives as several notifications: {}",
        project_log_records_pushed.len()
    );
    for pushed in &project_log_records_pushed {
        assert_eq!(
            pushed["project_directory"],
            project_directory_as_subscribed.as_str()
        );
        assert!(
            pushed["records"].as_array().expect("a records array").len()
                <= PROJECT_LOG_RECORDS_PER_NOTIFICATION
        );
    }
    for (earlier, later) in project_log_records_pushed
        .iter()
        .zip(project_log_records_pushed.iter().skip(1))
    {
        assert_eq!(later["after"], earlier["next_after"]);
    }
    let messages_of = |records: &Value| -> Vec<String> {
        records
            .as_array()
            .expect("a records array")
            .iter()
            .filter_map(|record| record["message"].as_str().map(str::to_owned))
            .collect()
    };
    let messages_pushed: Vec<String> = project_log_records_pushed
        .iter()
        .flat_map(|pushed| messages_of(&pushed["records"]))
        .collect();
    let tokens_pushed: Vec<&String> = messages_pushed
        .iter()
        .filter(|message| message.starts_with("project-log-push-token-"))
        .collect();
    assert_eq!(
        tokens_pushed.len(),
        records_written_in_one_burst,
        "{tokens_pushed:?}"
    );
    assert!(
        tokens_pushed
            .iter()
            .all(|message| !message.contains("of-another-project"))
    );

    let page = served
        .control_call(
            "logs",
            json!({
                "project_directory": project_directory_as_subscribed,
                "after": project_log_records_pushed[0]["after"],
                "until": project_log_records_pushed.last().expect("a notification")["next_after"],
                "count": 4096,
            }),
        )
        .await;
    assert_eq!(messages_of(&page["result"]["records"]), messages_pushed);
    assert_eq!(
        page["result"]["next_after"],
        project_log_records_pushed.last().expect("a notification")["next_after"]
    );
}

/// `logs` reads a project's log from disk: `stream` narrows it to one of the
/// project's streams, `count` bounds a page, `next_after` reads on, and the
/// records stay readable once the stream is unloaded.
#[tokio::test(flavor = "multi_thread")]
async fn logs_reads_a_projects_records_by_stream_paged_by_its_cursor() {
    let project_directory = tempfile::tempdir().expect("a project directory");
    let engine = Runner::new().expect("the engine builds");
    for stream_id in ["paged", "other"] {
        let stream = engine
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                stream_id,
            ))
            .expect("the stream loads");
        stream.log_route().run_entered(|| {
            for token_index in 0..3 {
                tracing::info!("logs-paging-token-{stream_id}-{token_index}");
            }
        });
        stream.log_route().write_every_record_it_emitted_and_sync();
    }
    let operations_on_the_loaded_streams =
        Arc::clone(&engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let project = project_directory.path().to_str().unwrap().to_owned();
    let tokens = |page: &Value| -> Vec<String> {
        page["records"]
            .as_array()
            .expect("a records array")
            .iter()
            .filter_map(|record| record["message"].as_str())
            .filter(|message| message.starts_with("logs-paging-token-"))
            .map(str::to_owned)
            .collect()
    };
    let read = |arguments: Value| {
        let operations = Arc::clone(&operations_on_the_loaded_streams);
        async move {
            tool_answer(&tool_call_result(operations, "logs", arguments).await)
                .expect("logs answers")
        }
    };

    let of_paged = read(json!({ "project_directory": project, "stream": "paged" })).await;
    assert_eq!(
        tokens(&of_paged),
        [
            "logs-paging-token-paged-0",
            "logs-paging-token-paged-1",
            "logs-paging-token-paged-2"
        ]
    );
    assert_eq!(of_paged["project_directory"], project.as_str());
    assert_eq!(of_paged["records_no_longer_held"], false);
    let every_record = read(json!({ "project_directory": project })).await;
    assert_eq!(tokens(&every_record).len(), 6, "{every_record}");

    let mut cursor = Value::Null;
    let mut read_one_by_one = Vec::new();
    loop {
        let page = read(json!({
            "project_directory": project,
            "stream": "paged",
            "after": cursor,
            "count": 1,
        }))
        .await;
        let records = page["records"].as_array().unwrap();
        if records.is_empty() {
            break;
        }
        assert_eq!(records.len(), 1, "{page}");
        read_one_by_one.extend(tokens(&page));
        cursor = page["next_after"].clone();
    }
    assert_eq!(read_one_by_one, tokens(&of_paged));

    engine.unload_stream("paged").expect("the stream unloads");
    engine.unload_stream("other").expect("the stream unloads");
    let after_the_unload = read(json!({ "project_directory": project, "stream": "paged" })).await;
    assert_eq!(tokens(&after_the_unload), tokens(&of_paged));

    let refusal = tool_answer(
        &tool_call_result(
            operations_on_the_loaded_streams,
            "logs",
            json!({ "project_directory": "relative/project" }),
        )
        .await,
    )
    .expect_err("a relative project directory is refused");
    assert!(refusal.contains("absolute path"), "{refusal}");
}
