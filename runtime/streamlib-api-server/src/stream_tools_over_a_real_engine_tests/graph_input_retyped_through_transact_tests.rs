// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `GraphInput` whose type a `transact` changes: a saved value is cast into the new type by the
//! one conversion that fits, a module's function node by its body run once out of stream, held
//! within the Input's range, or gives way to the new type's
//! default saying why, and one undo puts back both; a cleared value stays cleared, so a change of
//! type that clears it first takes; a type a client does not write is refused for that reason;
//! and a type the port it is linked into does not take is refused naming that port.

use super::*;

use super::input_value_set_tests::{InputValueTestNode, register_the_input_value_test_node_once};

const WARMTH_BASE_NODE_UUID: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";
const WARMTH_NODE_UUID: &str = "8d8d8d8d-8d8d-4d8d-8d8d-8d8d8d8d8d8d";

/// A project whose stream `main` holds a Float `GraphInput` `warmth_base`, saving 0.1 within
/// `[0, 1]`, by [`WARMTH_BASE_NODE_UUID`]; linked into the Float input `gain` of the
/// [`InputValueTestNode`] `warmth` when `linked`.
fn a_project_whose_main_stream_holds_a_float_input(linked: bool) -> ProjectWithGraphDocuments {
    register_the_input_value_test_node_once();
    let links = if linked {
        json!([{ "source": { "node": "warmth_base", "port": "value" },
                 "target": { "node": "warmth", "port": "gain" } }])
    } else {
        json!([])
    };
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": WARMTH_BASE_NODE_UUID, "name": "warmth_base",
              "type": "tatolab.stream:GraphInput",
              "config": { "port": "warmth_base", "pin_type": { "type": "float" },
                          "value": 0.1, "range": [0, 1] } },
            { "id": WARMTH_NODE_UUID, "name": "warmth",
              "type": InputValueTestNode::node_class_import_path().as_str(), "config": {} },
        ],
        "links": links,
    }))
}

/// The `set_node_input_value` operation setting `input` of `warmth_base` in `stream` to `value`.
fn set_on_warmth_base(stream: Uuid, input: &str, value: Value) -> Value {
    json!({ "operation": "set_node_input_value", "stream": stream,
            "node": WARMTH_BASE_NODE_UUID, "input": input, "value": value })
}

/// A Float `GraphInput` renders its saved value under `config.default`, the key a published Tato
/// Studio reads, and once cleared renders no value, under `value` or under `default`, so the clear
/// holds; and one `transact` clearing its value and range and
/// choosing a String takes.
///
/// Mental-revert: keeping the `default` the config itself holds draws the cleared value back from
/// the rendering, and the String is refused for holding 0.1.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_graph_input_value_stays_cleared_and_a_transact_clearing_it_retypes_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_float_input(false);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");
    let transact = |operations: Value| {
        tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
    };

    let set = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(
        set["config"]["default"],
        json!(0.1),
        "a published Studio reads it: {set}"
    );

    tool_answer(&transact(json!([set_on_warmth_base(stream, "value", Value::Null)])).await)
        .expect("the value is cleared");
    let cleared = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert!(cleared["input_values"].get("value").is_none(), "{cleared}");
    assert!(cleared["config"].get("default").is_none(), "{cleared}");
    assert!(cleared["config"].get("value").is_none(), "{cleared}");

    let string = json!({ "type": "string" });
    tool_answer(
        &transact(json!([
            set_on_warmth_base(stream, "value", Value::Null),
            set_on_warmth_base(stream, "range", Value::Null),
            set_on_warmth_base(stream, "pin_type", string.clone()),
        ]))
        .await,
    )
    .expect("a String with no value and no range is taken");
    let retyped = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(
        retyped["components"]["graph_interface"]["pin_type"], string,
        "{retyped}"
    );
    assert!(retyped["input_values"].get("value").is_none(), "{retyped}");
}

/// A `GraphInput` choosing a Texture, a type a client does not write, while it holds a range is
/// refused for that reason, not with the list of keys a fragment's Input takes.
///
/// Mental-revert: refusing the range as a key the Input does not take answers "It takes:
/// `port`, `pin_type`, `family`", which leaves out why.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_input_choosing_a_type_a_client_does_not_write_is_refused_for_that_reason() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_float_input(false);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                set_on_warmth_base(stream, "value", Value::Null),
                set_on_warmth_base(stream, "pin_type", json!({ "type": "texture" })),
            ] }),
        )
        .await,
    )
    .expect_err("a Texture holding a range is refused");
    assert!(
        refusal.contains("which a client does not write, and holds `range`"),
        "{refusal}"
    );
    assert!(
        refusal.contains("a type that holds a value, or array, channels or records"),
        "{refusal}"
    );
    assert!(!refusal.contains("It takes:"), "{refusal}");
}

/// A `GraphInput` linked into a Float input, whose value and range a `transact` clears while it
/// chooses a String, is refused naming the link from `warmth_base.value` into `warmth.gain`.
///
/// Mental-revert: with the cleared value drawn back from the rendering, the change is refused for
/// the 0.1 it held, and the link is never judged.
#[tokio::test(flavor = "multi_thread")]
async fn a_linked_graph_input_retyped_with_its_value_cleared_is_refused_naming_the_link() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_float_input(true);
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                set_on_warmth_base(stream, "value", Value::Null),
                set_on_warmth_base(stream, "range", Value::Null),
                set_on_warmth_base(stream, "pin_type", json!({ "type": "string" })),
            ] }),
        )
        .await,
    )
    .expect_err("a String linked into a Float is refused");
    assert!(refusal.contains("`warmth_base`"), "{refusal}");
    assert!(refusal.contains("`warmth.gain` (Float)"), "{refusal}");
    assert!(!refusal.contains("0.1"), "{refusal}");
    let kept = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(
        kept["components"]["graph_interface"]["pin_type"],
        json!({ "type": "float" }),
        "{kept}"
    );
}

/// A project whose stream `main` holds the `GraphInput` `warmth_base` of `pin_type`, saving
/// `value` with the settings `annotations`, by [`WARMTH_BASE_NODE_UUID`], the built-in
/// conversions registered.
fn a_project_whose_main_stream_holds_an_input_saving(
    pin_type: Value,
    value: Value,
    annotations: Value,
) -> ProjectWithGraphDocuments {
    streamlib_media_builtins::register_the_conversion_node_types();
    let mut config = json!({ "port": "warmth_base", "pin_type": pin_type, "value": value });
    if let (Some(config), Some(annotations)) = (config.as_object_mut(), annotations.as_object()) {
        config.extend(annotations.clone());
    }
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [{ "id": WARMTH_BASE_NODE_UUID, "name": "warmth_base",
                    "type": "tatolab.stream:GraphInput", "config": config }],
        "links": [],
    }))
}

/// The value `warmth_base` saves and the pin type it chooses, as `stream_id`'s live graph and
/// `project`'s graph document of `main` hold them.
fn what_warmth_base_holds(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    project: &ProjectWithGraphDocuments,
    stream_id: &str,
) -> (Value, Value, Value) {
    let live = the_node_named(&engine.the_live_graph_of(stream_id), "warmth_base");
    let saved = super::input_value_set_tests::the_input_values_of(
        &project.the_graph_document_of("main"),
        WARMTH_BASE_NODE_UUID,
    );
    (
        live["input_values"]["value"].clone(),
        saved["value"].clone(),
        live["components"]["graph_interface"]["pin_type"].clone(),
    )
}

/// The answer of a `transact` by alice in `engine` of `operations`, or its refusal.
async fn transact_by_alice(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    operations: Value,
) -> std::result::Result<Value, String> {
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
        .await,
    )
}

/// A Float `GraphInput` saving 0.1 that chooses an Integer saves 0 in the same edit, cast by
/// the registered `FloatToInteger`, which the answer names; `graph`, the graph document and
/// the answer show 0, and one undo puts back the Float and 0.1.
///
/// Mental-revert: without the cast the Integer is refused for the 0.1 it holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_float_input_choosing_an_integer_casts_its_saved_value_and_one_undo_puts_both_back() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({ "range": [0, 1] }),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "integer" })
        )]),
    )
    .await
    .expect("the retype casts the saved value");
    assert_eq!(
        retyped["saved_values_retyped"],
        json!([{ "stream": stream_id, "node": WARMTH_BASE_NODE_UUID, "value_before": 0.1,
                 "value": 0, "cast_by": "tatolab.stream:FloatToInteger" }]),
        "{retyped}"
    );
    assert!(
        retyped["operations"]
            .as_array()
            .is_some_and(|operations| operations
                .iter()
                .any(|operation| { operation["input"] == "value" && operation["to"] == json!(0) })),
        "the answer shows the value cast: {retyped}"
    );
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (json!(0), json!(0), json!({ "type": "integer" }))
    );

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the retype undoes");
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (json!(0.1), json!(0.1), json!({ "type": "float" })),
        "one undo puts back both the type and the value"
    );
}

/// An Integer `GraphInput` saving 2 that chooses a Float saves 2.0, cast by `IntegerToFloat`.
#[tokio::test(flavor = "multi_thread")]
async fn an_integer_input_choosing_a_float_saves_the_same_value_as_a_float() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "integer" }),
        json!(2),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "float" })
        )]),
    )
    .await
    .expect("the retype casts the saved value");
    assert_eq!(
        retyped["saved_values_retyped"][0]["cast_by"], "tatolab.stream:IntegerToFloat",
        "{retyped}"
    );
    let (live, saved, pin_type) = what_warmth_base_holds(&engine, &project, &stream_id);
    assert_eq!(pin_type, json!({ "type": "float" }));
    for value in [live, saved] {
        assert!(value.is_f64() && value == json!(2.0), "{value}");
    }
}

/// A Float `GraphInput` saving 0.9 within `[0.5, 1]` that chooses an Integer saves 1: the cast
/// truncates to 0, then is held within the range's whole numbers.
#[tokio::test(flavor = "multi_thread")]
async fn a_value_cast_by_a_retype_is_held_within_the_inputs_range() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.9),
        json!({ "range": [0.5, 1] }),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "integer" })
        )]),
    )
    .await
    .expect("the retype casts the saved value");
    assert_eq!(retyped["saved_values_retyped"][0]["value"], 1, "{retyped}");
    assert_eq!(what_warmth_base_holds(&engine, &project, &stream_id).1, 1);
}

/// A Float `GraphInput` saving 0.1 that chooses a String, which no conversion reaches, saves no
/// value, and the answer says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_retype_no_conversion_reaches_gives_way_to_the_default_saying_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "string" })
        )]),
    )
    .await
    .expect("a retype no conversion reaches is taken");
    let answered = &retyped["saved_values_retyped"][0];
    assert!(answered.get("value").is_none(), "{retyped}");
    assert!(
        answered["default_because"]
            .as_str()
            .is_some_and(|why| why.contains("no conversion the stream registers")),
        "{retyped}"
    );
    let (live, saved, pin_type) = what_warmth_base_holds(&engine, &project, &stream_id);
    assert_eq!(
        (live, saved, pin_type),
        (Value::Null, Value::Null, json!({ "type": "string" }))
    );
}

/// The Python node type `node_type`, a conversion from a Float into a String, which the stream
/// `stream_id` resolves as its interpreter would describe it.
fn a_projects_float_to_string_conversion(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
    node_type: &str,
) {
    engine
        .engine
        .loaded_stream_of_the_id(stream_id)
        .expect("the stream is loaded")
        .node_types_this_stream_resolves()
        .register_a_described_type_that_runs_no_node_for_a_test(
            a_python_function_node_type(
                node_type,
                vec![a_typed_port("level", PinType::Float { exact_size: None })],
                vec![a_typed_port("text", PinType::String {})],
            )
            .stating_a_conversion(),
            None,
        )
        .expect("the stream resolves the project's node type");
}

/// A retype that only a conversion a project's Python node declares reaches saves no value when
/// the stream has no way to run its body out of stream, and the answer names it.
///
/// Mental-revert: treating the module's conversion as the built-in number cast casts 0.1 into a
/// value no String takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_retype_only_a_modules_conversion_reaches_gives_way_to_the_default_naming_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    a_projects_float_to_string_conversion(&engine, &stream_id, "nodes.format:LevelAsText");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "string" })
        )]),
    )
    .await
    .expect("the retype is taken");
    let answered = &retyped["saved_values_retyped"][0];
    assert_eq!(
        answered["conversions_that_fit"],
        json!(["nodes.format:LevelAsText"]),
        "{retyped}"
    );
    assert!(
        answered["default_because"]
            .as_str()
            .is_some_and(|why| why.contains("only inside a started stream")),
        "{retyped}"
    );
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id).1,
        Value::Null
    );
}

/// The stream `stream_id` casting a saved value through each function node conversion it
/// describes from now on with `cast`, standing in for its body's one run out of stream.
fn a_function_node_conversions_body_standing_in_as(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
    cast: fn(&Value) -> std::result::Result<Value, String>,
) {
    engine
        .engine
        .loaded_stream_of_the_id(stream_id)
        .expect("the stream is loaded")
        .node_types_this_stream_resolves()
        .cast_a_saved_value_through_each_function_node_conversion_for_a_test(move |_conversion| {
            streamlib::sdk::descriptors::PinConversionSavedValueCast::new(move |value, _into| {
                cast(value)
            })
        });
}

/// A Float `GraphInput` saving 0.1 that chooses a String, which only a project's function node
/// conversion reaches, saves the value its body gives, which the answer names; one undo puts
/// back both.
///
/// Mental-revert: leaving a module's conversion with no cast of its own saves no value.
#[tokio::test(flavor = "multi_thread")]
async fn a_retype_only_a_function_node_conversion_reaches_saves_what_its_body_gives() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    a_function_node_conversions_body_standing_in_as(&engine, &stream_id, |value| {
        Ok(json!(format!(
            "{}%",
            (value.as_f64().unwrap_or_default() * 100.0).round()
        )))
    });
    a_projects_float_to_string_conversion(&engine, &stream_id, "nodes.format:LevelAsPercent");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "string" })
        )]),
    )
    .await
    .expect("the retype casts the saved value");
    assert_eq!(
        retyped["saved_values_retyped"],
        json!([{ "stream": stream_id, "node": WARMTH_BASE_NODE_UUID, "value_before": 0.1,
                 "value": "10%", "cast_by": "nodes.format:LevelAsPercent" }]),
        "{retyped}"
    );
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (json!("10%"), json!("10%"), json!({ "type": "string" }))
    );

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the retype undoes");
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id),
        (json!(0.1), json!(0.1), json!({ "type": "float" })),
        "one undo puts back both the type and the value"
    );
}

/// A function node conversion whose body does not run out of stream saves no value, and the
/// answer names the conversion and why.
#[tokio::test(flavor = "multi_thread")]
async fn a_function_node_conversion_that_cannot_run_out_of_stream_gives_way_to_the_default_saying_why()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    a_function_node_conversions_body_standing_in_as(&engine, &stream_id, |_value| {
        Err(
            "`nodes.format:LevelAsPercent` takes the stream's time as `now`, which no stream \
             gives out of stream"
                .to_owned(),
        )
    });
    a_projects_float_to_string_conversion(&engine, &stream_id, "nodes.format:LevelAsPercent");
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "string" })
        )]),
    )
    .await
    .expect("the retype is taken");
    let answered = &retyped["saved_values_retyped"][0];
    assert_eq!(
        answered["conversions_that_fit"],
        json!(["nodes.format:LevelAsPercent"]),
        "{retyped}"
    );
    assert!(
        answered["default_because"].as_str().is_some_and(|why| why
            .starts_with("`nodes.format:LevelAsPercent` did not cast it:")
            && why.contains("takes the stream's time")),
        "{retyped}"
    );
    assert_eq!(
        what_warmth_base_holds(&engine, &project, &stream_id).1,
        Value::Null
    );
}

/// A retype two conversions reach picks neither: the value gives way to the default, and the
/// answer names both.
#[tokio::test(flavor = "multi_thread")]
async fn a_retype_two_conversions_reach_gives_way_to_the_default_naming_both() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({}),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    for node_type in ["nodes.format:LevelAsText", "nodes.format:LevelAsPercent"] {
        a_projects_float_to_string_conversion(&engine, &stream_id, node_type);
    }
    let stream = project.stream_asset_of("main");

    let retyped = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "string" })
        )]),
    )
    .await
    .expect("the retype is taken");
    let answered = &retyped["saved_values_retyped"][0];
    assert_eq!(
        answered["conversions_that_fit"],
        json!(["nodes.format:LevelAsPercent", "nodes.format:LevelAsText"]),
        "{retyped}"
    );
    assert!(answered.get("value").is_none(), "{retyped}");
}

/// A Float `GraphInput` saving 0.1 within `[0, 1]` that chooses a String is refused at the type,
/// after its value is taken off, naming the `range` a String does not take; the graph document,
/// the live graph and the project's history still hold the Float and 0.1, with no commit made.
///
/// Mental-revert: reading a running `GraphInput`'s saved value from its config alone, which
/// renders it under `default`, reverses the value taken off to none, and draws it cleared.
#[tokio::test(flavor = "multi_thread")]
async fn a_retype_refused_partway_leaves_the_type_the_value_and_the_history_as_they_were() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_input_saving(
        json!({ "type": "float" }),
        json!(0.1),
        json!({ "range": [0, 1] }),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream = project.stream_asset_of("main");
    let document_before = project.the_graph_document_of("main");

    let refusal = transact_by_alice(
        &engine,
        json!([set_on_warmth_base(
            stream,
            "pin_type",
            json!({ "type": "string" })
        )]),
    )
    .await
    .expect_err("a String holding a range is refused");
    assert!(refusal.contains("`range`"), "{refusal}");

    let live = the_node_named(&engine.the_live_graph_of(&stream_id), "warmth_base");
    assert_eq!(live["input_values"]["value"], json!(0.1));
    assert_eq!(
        live["components"]["graph_interface"]["pin_type"],
        json!({ "type": "float" })
    );
    let document_after = project.the_graph_document_of("main");
    assert_eq!(document_after, document_before);
    assert_eq!(document_after["nodes"][0]["config"]["value"], json!(0.1));
    let undone = engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await;
    assert!(
        undone.is_err(),
        "the refused retype made no commit: {undone:?}"
    );
}
