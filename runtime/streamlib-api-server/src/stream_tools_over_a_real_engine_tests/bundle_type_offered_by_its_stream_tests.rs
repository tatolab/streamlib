// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `bundle_type` of a placed `MakeBundle` or `BreakBundle`, over a real engine: `graph`
//! serves it as a choice listing exactly the bundles its stream's node types declare, each by
//! its name, and none while they declare none; a bundle it does not list is refused, and one it
//! lists is set.

use streamlib::sdk::descriptors::BundleMember;
use streamlib_media_builtins::{BreakBundle, MakeBundle};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::input_value_set_tests::the_input_values_of;
use super::*;

const THERMOMETER_TYPE: &str = "nodes.weather:Thermometer";
const LOGBOOK_TYPE: &str = "nodes.weather:Logbook";
const SYNTH_TYPE: &str = "nodes.music:Synth";

/// A bundle the dataclass `name` declares, of a Float member for each of `members`.
fn a_bundle_named(name: &str, members: &[&str]) -> PinType {
    PinType::Bundle {
        name: Some(name.to_owned()),
        members: members
            .iter()
            .map(|member| BundleMember {
                name: (*member).to_owned(),
                member_type: PinType::Float { exact_size: None },
                meaning: None,
                default_value: None,
            })
            .collect(),
    }
}

fn a_reading() -> PinType {
    a_bundle_named("Reading", &["level", "humidity"])
}

fn a_tone() -> PinType {
    a_bundle_named("Tone", &["pitch"])
}

/// `stream_id` resolving, as its interpreter described them, a `Thermometer` giving a reading,
/// a `Logbook` taking a List of readings and a `Synth` taking a tone.
fn the_stream_describing_three_node_types_of_two_bundles(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
) {
    let loaded = engine
        .engine
        .loaded_stream_of_the_id(stream_id)
        .expect("the stream is loaded");
    let node_types = loaded.node_types_this_stream_resolves();
    for descriptor in [
        a_python_function_node_type(
            THERMOMETER_TYPE,
            Vec::new(),
            vec![a_typed_port("reading", a_reading())],
        ),
        a_python_function_node_type(
            LOGBOOK_TYPE,
            vec![a_typed_port(
                "readings",
                PinType::ListOf {
                    element_type: Box::new(a_reading()),
                },
            )],
            Vec::new(),
        ),
        a_python_function_node_type(SYNTH_TYPE, vec![a_typed_port("tone", a_tone())], Vec::new()),
    ] {
        node_types
            .register_a_described_type_that_runs_no_node_for_a_test(descriptor, None)
            .expect("the stream resolves the node type");
    }
}

/// A loaded stream `main` holding a `make` of `MakeBundle` and a `break` of `BreakBundle`; its
/// id, the project and the two nodes' UUIDs.
async fn a_stream_holding_a_make_and_a_break_bundle(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (String, ProjectWithGraphDocuments, String, String) {
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": [], "links": [],
    }));
    let (stream_id, make, break_) = a_make_and_a_break_bundle_placed_in(engine, &project).await;
    (stream_id, project, make, break_)
}

/// `project`'s stream `main`, loaded holding a `make` of `MakeBundle` and a `break` of
/// `BreakBundle`; its id and the two nodes' UUIDs.
async fn a_make_and_a_break_bundle_placed_in(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    project: &ProjectWithGraphDocuments,
) -> (String, String, String) {
    streamlib_media_builtins::register_media_builtin_node_types();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(project, "main");
    let added = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": project.stream_asset_of("main"),
                      "type": MakeBundle::node_class_import_path().as_str(), "name": "make" },
                    { "operation": "add_node", "stream": project.stream_asset_of("main"),
                      "type": BreakBundle::node_class_import_path().as_str(), "name": "break" },
                ],
            }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the two nodes are placed: {refusal:?}"));
    let the_uuid_of = |index: usize| {
        added["added_nodes"][index]["node"]
            .as_str()
            .expect("an added node's UUID")
            .to_owned()
    };
    (stream_id, the_uuid_of(0), the_uuid_of(1))
}

/// The pin type `graph` serves on the `bundle_type` of the node `node_name` of `stream_id`.
async fn the_bundle_type_served_on(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
    node_name: &str,
) -> Value {
    let graph = engine
        .the_answer_or_the_refusal_of("graph", json!({ "stream": stream_id }))
        .await
        .unwrap_or_else(|refusal| panic!("graph answers: {refusal:?}"));
    let node = the_node_named(&graph, node_name);
    node["ports"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == "bundle_type")
        .map(|port| port["pin_type"].clone())
        .unwrap_or_else(|| panic!("`{node_name}` has a `bundle_type`: {node}"))
}

/// Both nodes, placed before the stream's node types are described and after, offer exactly the
/// two bundles they declare, each once and by its name, a List's element among them.
///
/// Mental-revert: serving the setting as its node type declares it lists no bundle, and reading
/// the list when a node is placed leaves out the bundles described after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_placed_make_or_break_bundle_offers_exactly_the_bundles_its_stream_declares() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, _, _, _) = a_stream_holding_a_make_and_a_break_bundle(&engine).await;

    let none_on_offer = json!({
        "type": "pin_type_choice",
        "among_the_bundles_its_stream_declares": true,
    });
    for node in ["make", "break"] {
        assert_eq!(
            the_bundle_type_served_on(&engine, &stream_id, node).await,
            none_on_offer,
            "a stream whose node types declare no bundle offers none"
        );
    }

    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);

    let both_on_offer = json!({
        "type": "pin_type_choice",
        "among": [a_reading(), a_tone()],
        "among_the_bundles_its_stream_declares": true,
    });
    for node in ["make", "break"] {
        let served = the_bundle_type_served_on(&engine, &stream_id, node).await;
        assert_eq!(served, both_on_offer, "{node}");
        assert_eq!(served["among"][0]["name"], "Reading", "{served}");
    }
}

/// A bundle only a described class declares whose name a view of the project also has is not
/// offered, since the stream resolves that name as the view; the view's own bundle, and those of
/// classes no view shadows, are.
///
/// Mental-revert: listing every described class's bundles offers `Tone`, which no node type the
/// stream resolves declares.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_only_a_class_a_view_shadows_declares_is_not_offered() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": [], "links": [],
    }));
    let a_gauge = a_bundle_named("Gauge", &["needle"]);
    let folder = project.path().join("views/synth");
    std::fs::create_dir_all(&folder).expect("the view's folder");
    std::fs::write(
        folder.join("tatolab_view.json"),
        json!({ "contract": "1.1", "title": "Synth", "entry": "index.html", "placement": "node",
                "outputs": [{ "name": "gauge", "pin_type": a_gauge }] })
        .to_string(),
    )
    .expect("its manifest");
    let (stream_id, _, _) = a_make_and_a_break_bundle_placed_in(&engine, &project).await;
    let loaded = engine
        .engine
        .loaded_stream_of_the_id(&stream_id)
        .expect("the stream is loaded");
    let node_types = loaded.node_types_this_stream_resolves();
    let the_view_type = format!("{TEST_PROJECT_NAME}:synth");
    for descriptor in [
        a_python_function_node_type(
            THERMOMETER_TYPE,
            Vec::new(),
            vec![a_typed_port("reading", a_reading())],
        ),
        a_python_function_node_type(
            &the_view_type,
            vec![a_typed_port("tone", a_tone())],
            Vec::new(),
        ),
    ] {
        node_types
            .register_a_described_type_that_runs_no_node_for_a_test(descriptor, None)
            .expect("the stream resolves the node type");
    }

    let offered = the_bundle_type_served_on(&engine, &stream_id, "make").await;
    assert_eq!(
        offered,
        json!({
            "type": "pin_type_choice",
            "among": [a_gauge, a_reading()],
            "among_the_bundles_its_stream_declares": true,
        })
    );
}

/// A bundle the stream's node types do not declare is refused naming the bundles on offer, and
/// nothing is saved; one on offer is set, and types the node's ports by it.
///
/// Mental-revert: a choice among any type takes the bundle no node type declares.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_its_stream_does_not_declare_is_refused_and_one_it_declares_is_set() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project, make, _) = a_stream_holding_a_make_and_a_break_bundle(&engine).await;
    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": make, "input": "bundle_type",
                    "value": a_bundle_named("Elsewhere", &["level"]),
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a bundle no node type of the stream declares is refused");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputValueNotValid,
        "{refusal:?}"
    );
    assert!(
        refusal.runtime_words.contains("`Reading`") && refusal.runtime_words.contains("`Tone`"),
        "the refusal names the bundles on offer: {refusal:?}"
    );
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), &make),
        json!({})
    );

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": make, "input": "bundle_type",
                    "value": a_tone(), "author": an_agent_named("alice") }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("a bundle on offer is set: {refusal:?}"));
    let made = the_node_named(&engine.the_live_graph_of(&stream_id), "make");
    let bundle_output = made["ports"]["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|port| port["name"] == "bundle")
        .unwrap_or_else(|| panic!("`make` has a `bundle` output: {made}"));
    assert_eq!(bundle_output["pin_type"], json!(a_tone()), "{made}");
}

const MAKE_NODE_UUID: &str = "5a5a5a5a-5a5a-4a5a-8a5a-5a5a5a5a5a5a";

/// A Make Bundle saved before bundles were named holds its reading unnamed; setting a named
/// bundle over it, then undoing and redoing the set, each applies, and the undo puts the unnamed
/// reading back.
///
/// Mental-revert: matching a chosen bundle to an offered one whole refuses the undo, since the
/// unnamed reading the undo puts back is no bundle on offer.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_saved_unnamed_is_put_back_by_an_undo_of_a_set_over_it() {
    streamlib_media_builtins::register_media_builtin_node_types();
    let PinType::Bundle { members, .. } = a_reading() else {
        unreachable!("a reading is a bundle")
    };
    let an_unnamed_reading = json!(PinType::Bundle {
        name: None,
        members
    });
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": MAKE_NODE_UUID, "name": "make",
              "type": MakeBundle::node_class_import_path().as_str(),
              "input_values": { "bundle_type": an_unnamed_reading } },
        ],
        "links": [],
    }));
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    the_stream_describing_three_node_types_of_two_bundles(&engine, &stream_id);
    let alice =
        || json!({ "project_directory": project.path(), "author": an_agent_named("alice") });

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": MAKE_NODE_UUID,
                    "input": "bundle_type", "value": a_tone(),
                    "author": an_agent_named("alice") }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("a bundle on offer is set: {refusal:?}"));
    engine
        .the_answer_or_the_refusal_of("undo", alice())
        .await
        .unwrap_or_else(|refusal| panic!("the undo puts the unnamed reading back: {refusal:?}"));
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MAKE_NODE_UUID),
        json!({ "bundle_type": an_unnamed_reading })
    );
    engine
        .the_answer_or_the_refusal_of("redo", alice())
        .await
        .unwrap_or_else(|refusal| panic!("the redo sets the tone again: {refusal:?}"));
    assert_eq!(
        the_input_values_of(&project.the_graph_document_of("main"), MAKE_NODE_UUID),
        json!({ "bundle_type": a_tone() })
    );
}

/// The port `name` of `pin_type` as a node interpreter describes it.
fn a_described_port(name: &str, pin_type: PinType) -> Value {
    json!({
        "name": name,
        "description": "",
        "family": PortFamilyDeclaration::of_family(
            PortFamily::carrying(&pin_type).unwrap_or(PortFamily::Records),
        ),
        "delivery_profile": "newest",
        "pin_declaration": PinDeclaration::exactly(pin_type),
    })
}

/// The class `node_type` of the project's own code, as its interpreter describes it.
fn a_described_class_of_the_projects_own_code(
    node_type: &str,
    input_ports: Vec<Value>,
    output_ports: Vec<Value>,
) -> Value {
    json!({
        "import_path": node_type,
        "short_name": node_type.rsplit(':').next().expect("a short name"),
        "description": "",
        "kind": "process",
        "execution": { "mode": "reactive" },
        "scheduling_priority": null,
        "paced_by_real_time": false,
        "config_schema": { "type": "object" },
        "input_ports": input_ports,
        "output_ports": output_ports,
    })
}

/// A stream whose project's own code declares a `Thermometer` giving a reading and a `Synth`
/// taking a tone, holding only a placed Make Bundle, offers both bundles: the stream's
/// interpreter describes the project's classes when the stream loads, before a node names one.
///
/// Mental-revert: describing only the classes installed distributions declare at load leaves the
/// project's own unnamed until a node of one is placed, and the Make Bundle offers no bundle.
#[tokio::test(flavor = "multi_thread")]
async fn a_make_bundle_placed_alone_offers_the_bundles_its_projects_own_code_declares() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": [], "links": [],
    }));
    let described = json!({
        "described_node_types": [
            a_described_class_of_the_projects_own_code(
                THERMOMETER_TYPE,
                Vec::new(),
                vec![a_described_port("reading", a_reading())],
            ),
            a_described_class_of_the_projects_own_code(
                SYNTH_TYPE,
                vec![a_described_port("tone", a_tone())],
                Vec::new(),
            ),
        ],
        "refused_node_types": [],
        "installed_modules": [],
        "project_node_types": [THERMOMETER_TYPE, SYNTH_TYPE],
    });
    write_an_executable_script_from_a_child_process(
        &project.interpreter(),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n*--describe-installed-modules*)\n\
             cat <<'DESCRIBED'\n{described}\nDESCRIBED\n;;\nesac\nexit 0\n"
        ),
    );

    streamlib_media_builtins::register_media_builtin_node_types();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": project.stream_asset_of("main"),
                      "type": MakeBundle::node_class_import_path().as_str(), "name": "make" },
                ],
            }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the Make Bundle is placed: {refusal:?}"));

    assert_eq!(
        the_bundle_type_served_on(&engine, &stream_id, "make").await,
        json!({
            "type": "pin_type_choice",
            "among": [a_reading(), a_tone()],
            "among_the_bundles_its_stream_declares": true,
        })
    );
}
