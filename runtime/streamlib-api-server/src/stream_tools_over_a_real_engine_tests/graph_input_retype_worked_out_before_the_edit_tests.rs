// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A module's conversion casts the value a `GraphInput`'s retype carries before the transaction
//! takes the project's lock or opens a stream's edit batch, and never under either: a retype
//! that an earlier change in the same transaction touched runs no module's conversion and says
//! why.

use super::*;

use super::graph_input_retyped_through_transact_tests::{
    WARMTH_BASE_NODE_UUID, a_function_node_conversions_body_standing_in_as,
    a_project_whose_main_stream_holds_an_input_saving, a_projects_float_to_string_conversion,
    as_a_percentage, set_on_warmth_base, transact_by_alice, what_warmth_base_holds,
};
use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlRequestOrigin};

/// One run of a function node conversion's stand-in: the value it was handed, whether the
/// project's lock was free, and whether no stream's edit batch was open on its thread.
#[derive(Debug, PartialEq)]
struct ARunOfTheCast {
    value: Value,
    the_project_lock_was_free: bool,
    no_edit_batch_was_open: bool,
}

/// Each run of the stand-in that casts as a percentage through each function node conversion
/// the stream `stream_id` describes from now on, recording what it ran under: the lock of
/// `project` that `dispatcher`'s transactions take, and an edit batch of the stream, which a
/// batch its thread holds open refuses to open beside.
fn the_runs_of_a_cast_checking_what_it_runs_under(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    dispatcher: &ControlOperationDispatcher,
    project: &ProjectWithGraphDocuments,
    stream_id: &str,
) -> Arc<parking_lot::Mutex<Vec<ARunOfTheCast>>> {
    let project_repository = dispatcher
        .project_repositories
        .the_project_repository_at(project.path())
        .expect("the project opens");
    let stream_operations = engine
        .operations_on_the_loaded_streams()
        .runtime_operations_of_the_stream_a_call_names(stream_id)
        .expect("the stream is loaded");
    let runs = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let runs_recorded = Arc::clone(&runs);
    a_function_node_conversions_body_standing_in_as(engine, stream_id, move |value| {
        let the_project_lock_was_free = project_repository.try_lock().is_some();
        let no_edit_batch_was_open = match Arc::clone(&stream_operations).open_a_graph_edit_batch()
        {
            Ok(batch) => batch.close().is_ok(),
            Err(_) => false,
        };
        runs_recorded.lock().push(ARunOfTheCast {
            value: value.clone(),
            the_project_lock_was_free,
            no_edit_batch_was_open,
        });
        as_a_percentage(value)
    });
    runs
}

/// The answer of a `transact` by alice of `operations` through `dispatcher`, or its refusal.
async fn transact_through(
    dispatcher: &ControlOperationDispatcher,
    operations: Value,
) -> std::result::Result<Value, String> {
    let call = streamlib_control_contract::ControlOperationCall::parse(
        "transact",
        Some(json!({ "author": an_agent_named("alice"), "operations": operations })),
    )
    .unwrap_or_else(|_| panic!("a transact call parses"));
    dispatcher
        .dispatch(
            call,
            &ControlRequestOrigin {
                client_name: Some("alice".to_owned()),
                subscriptions: None,
                viewers_held_open: None,
            },
        )
        .await
        .map(|answer| serde_json::to_value(answer).expect("the answer serializes"))
        .map_err(|refusal| format!("{refusal:?}"))
}

/// A Float `GraphInput` saving 0.1 that chooses a String through a project's function node
/// conversion, on a stream `started` or not, saves the value the conversion's stand-in gives,
/// which ran once, with the project's lock free and no edit batch open.
async fn a_retype_casting_through_a_function_node_conversion_with_the_project_unlocked(
    started: bool,
) {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = if started {
        engine.a_stream_started_from_its_graph_document(&project, "main")
    } else {
        engine.a_stream_loaded_from_its_graph_document(&project, "main")
    };
    let handler = crate::mcp::LocalApiMcpServerHandler::new(
        engine.operations_on_the_loaded_streams(),
        tokio_util::sync::CancellationToken::new(),
    );
    let dispatcher = handler.control_operation_dispatcher();
    let runs =
        the_runs_of_a_cast_checking_what_it_runs_under(&engine, &dispatcher, &project, &stream_id);
    a_projects_float_to_string_conversion(&engine, &stream_id, "nodes.format:LevelAsPercent");
    let stream = project.stream_asset_of("main");

    let retyped = tokio::time::timeout(
        Duration::from_secs(60),
        transact_through(
            &dispatcher,
            json!([set_on_warmth_base(
                stream,
                "pin_type",
                json!({ "type": "string" })
            )]),
        ),
    )
    .await
    .expect("the transaction answers")
    .expect("the retype casts the saved value");

    assert_eq!(
        retyped["saved_values_retyped"][0]["value"],
        json!("10%"),
        "{retyped}"
    );
    assert_eq!(
        *runs.lock(),
        vec![ARunOfTheCast {
            value: json!(0.1),
            the_project_lock_was_free: true,
            no_edit_batch_was_open: true,
        }]
    );
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (json!("10%"), json!("10%"), json!({ "type": "string" }))
    );
}

/// On a stream not started, a module's conversion casts a retyped value with the project's lock
/// free.
///
/// Mental-revert: working out the casts after the transaction takes the project's lock runs the
/// conversion under it.
#[tokio::test(flavor = "multi_thread")]
async fn a_function_node_conversion_casts_a_retyped_value_with_the_project_unlocked() {
    a_retype_casting_through_a_function_node_conversion_with_the_project_unlocked(false).await;
}

/// On a running stream, a module's conversion casts a retyped value with the project's lock free
/// and no edit batch of the stream open, and the transaction answers: the Input, still built
/// for its Float, is not handed `"10%"` before its rebuild.
///
/// Mental-revert: as on a stream not started, and casting once the stream's batch is open runs
/// the conversion beside it. Handing the running Input `"10%"` before its rebuild refuses it
/// for the Channels its output still carries; and when the retype's own reversal also rebuilds
/// it while the batch is open, that waits for ever on the stream's writer, holding the
/// project's lock.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn on_a_running_stream_a_function_node_conversion_casts_with_no_edit_batch_open() {
    a_retype_casting_through_a_function_node_conversion_with_the_project_unlocked(true).await;
}

/// A transaction that sets a Float `GraphInput`'s saved value to 0.3 and then chooses a String
/// for it runs no module's conversion under the project's lock: the retype it finds carries
/// 0.3, not the 0.1 worked out before the edit, so the value gives way to the default, naming
/// the conversion and why, and the conversion ran once, before the edit, on 0.1.
///
/// Mental-revert: matching a retype worked out before the edit by its node alone saves `"10%"`
/// for 0.3; running the module's conversion for a retype with none worked out runs it under the
/// lock, on 0.3.
#[tokio::test(flavor = "multi_thread")]
async fn a_retype_after_a_set_on_the_same_input_runs_no_modules_conversion_and_says_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let values_cast = Arc::new(parking_lot::Mutex::new(Vec::<Value>::new()));
    let values_recorded = Arc::clone(&values_cast);
    a_function_node_conversions_body_standing_in_as(&engine, &stream_id, move |value| {
        values_recorded.lock().push(value.clone());
        as_a_percentage(value)
    });
    a_projects_float_to_string_conversion(&engine, &stream_id, "nodes.format:LevelAsPercent");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([
            set_on_warmth_base(stream, "value", json!(0.3)),
            set_on_warmth_base(stream, "pin_type", json!({ "type": "string" })),
        ]),
    )
    .await
    .expect("the retype is taken");

    let answered = &retyped["saved_values_retyped"][0];
    assert_eq!(answered["node"], WARMTH_BASE_NODE_UUID, "{retyped}");
    assert_eq!(answered["value_before"], json!(0.3), "{retyped}");
    assert_eq!(
        answered["conversions_that_fit"],
        json!(["nodes.format:LevelAsPercent"]),
        "{retyped}"
    );
    assert!(
        answered["default_because"].as_str().is_some_and(|why| why
            .starts_with("`nodes.format:LevelAsPercent` cannot cast it")
            && why.contains("an earlier change in the same transaction touched this Input")),
        "{retyped}"
    );
    assert_eq!(*values_cast.lock(), vec![json!(0.1)]);
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (Value::Null, Value::Null, json!({ "type": "string" }))
    );
}

/// A transaction choosing a String for a Float `GraphInput` saving 0.1 twice runs its module's
/// conversion once: the second retype finds the Input already a String and carries nothing.
///
/// Mental-revert: working out every retype a transaction names runs the conversion twice and
/// drops the second cast.
#[tokio::test(flavor = "multi_thread")]
async fn two_identical_retypes_in_one_transaction_run_a_modules_conversion_once() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let values_cast = Arc::new(parking_lot::Mutex::new(Vec::<Value>::new()));
    let values_recorded = Arc::clone(&values_cast);
    a_function_node_conversions_body_standing_in_as(&engine, &stream_id, move |value| {
        values_recorded.lock().push(value.clone());
        as_a_percentage(value)
    });
    a_projects_float_to_string_conversion(&engine, &stream_id, "nodes.format:LevelAsPercent");
    let stream = project.stream_asset_of("main");
    let retype = set_on_warmth_base(stream, "pin_type", json!({ "type": "string" }));

    let retyped = transact_by_alice(&engine, json!([retype.clone(), retype]))
        .await
        .expect("the retypes are taken");

    assert_eq!(
        retyped["saved_values_retyped"],
        json!([{ "stream": stream_id, "node": WARMTH_BASE_NODE_UUID, "value_before": 0.1,
                 "value": "10%", "cast_by": "nodes.format:LevelAsPercent" }]),
        "{retyped}"
    );
    assert_eq!(*values_cast.lock(), vec![json!(0.1)]);
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (json!("10%"), json!("10%"), json!({ "type": "string" }))
    );
}
