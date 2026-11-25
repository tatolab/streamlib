// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `list_pin_types`, `list_node_types` and `describe_node_types` over a real engine: every
//! built-in pin type with the conversions from it, a project's node types by source with the
//! runtime's own last, and each node type named described from the best description the runtime
//! has, or why it cannot be.

use super::input_value_set_tests::{
    InputValueTestNode, MIXER_NODE_UUID, a_project_whose_main_stream_holds_a_mixer,
    register_the_input_value_test_node_once,
};
use super::project_node_type_listing_tests::STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING;
use super::*;

use streamlib::sdk::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration, PinType, PortDescriptor};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

/// A native conversion from Audio to a Tensor, a pair no other test links.
#[streamlib::sdk::node(
    execution = manual,
    input("channels", delivery_profile = "ordered", pin_type = Audio),
    output("array", pin_type = Tensor),
)]
#[meta(conversion = true)]
struct AudioToTensorTestConversion;

impl ManualNode for AudioToTensorTestConversion::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// Register [`AudioToTensorTestConversion`] with the node registry, once per test binary.
fn register_the_audio_to_tensor_test_conversion_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<AudioToTensorTestConversion::Node>());
}

/// A project whose venv's interpreter lists `sources` as its node types.
pub(super) fn a_project_listing(sources: Value) -> ProjectWithGraphDocuments {
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    write_an_executable_script_from_a_child_process(
        &project.interpreter(),
        STUB_VENV_INTERPRETER_PRINTING_THE_PROJECTS_NODE_TYPE_LISTING,
    );
    std::fs::write(
        project.path().join("listing.json"),
        json!({ "sources": sources }).to_string(),
    )
    .expect("the listing document");
    project
}

/// The project's own node code, as the listing reads it: `nodes.mix:Mixer`, whose ports its
/// source states, and `nodes.camera:Camera`, which registers its ports in code; and a graph
/// generator promoting one setting.
pub(super) fn the_project_source() -> Value {
    json!({
        "source": "project",
        "node_types": [
            {
                "type": "nodes.mix:Mixer", "kind": "function",
                "description": "Mixes two levels.\n\nThe second is scaled first.",
                "category": "Audio", "path": "nodes/mix.py", "line": 7,
                "inputs": [{ "name": "level", "description": "the level",
                             "annotation": "float", "pin_default": 0.5 }],
                "outputs": [{ "name": "mixed", "description": "",
                              "annotation": null, "pin_default": null }],
            },
            {
                "type": "nodes.camera:Camera", "kind": "process", "description": "A camera.",
                "category": null, "inputs": null, "outputs": null,
                "path": "nodes/camera.py", "line": 3,
            },
        ],
        "graph_generators": [{
            "generator": "graphs:two_cameras", "description": "Two cameras side by side.",
            "promoted_config": [{ "name": "spacing", "annotation": "float", "default": 2.0 }],
            "path": "graphs.py", "line": 12,
        }],
        "unreadable_modules": [],
    })
}

/// Every built-in pin type is listed in the contract's order, each storing a value with its
/// value's schema, List and bundle with how a pin of one is declared, and Audio with, among
/// the conversions the test binary registers, the one a registered node type states from it.
///
/// Mental-revert: reading conversions from no node type leaves Audio's `converts_to` empty.
#[tokio::test(flavor = "multi_thread")]
async fn every_built_in_pin_type_is_listed_with_its_schema_and_the_conversions_from_it() {
    register_the_audio_to_tensor_test_conversion_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_pin_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the pin types are listed");

    let rows = listed["pin_types"].as_array().expect("the pin types");
    let row = |name: &str| {
        rows.iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("no row `{name}`: {listed}"))
    };
    assert_eq!(
        rows.iter()
            .map(|row| row["name"].clone())
            .collect::<Vec<_>>(),
        streamlib_node_schema::pin_type::BuiltInPinTypeName::EVERY_NAME
            .iter()
            .map(|name| serde_json::to_value(name).expect("a name serializes"))
            .collect::<Vec<_>>()
    );
    let audio_test_conversion = json!({
        "from": { "type": "audio" },
        "to": { "type": "tensor" },
        "node_type": AudioToTensorTestConversion::node_class_import_path().as_str(),
    });
    assert!(
        row("audio")["converts_to"]
            .as_array()
            .is_some_and(|conversions| conversions.contains(&audio_test_conversion)),
        "{}",
        row("audio")
    );
    assert_eq!(
        row("boolean")["value_json_schema"]["type"],
        "boolean",
        "{listed}"
    );
    assert!(
        row("texture").get("value_json_schema").is_none(),
        "{listed}"
    );
    assert!(
        row("list_of")["how_to_declare"]
            .as_str()
            .is_some_and(|how| how.contains("list[float]")),
        "{listed}"
    );
    assert!(row("bundle")["how_to_declare"].is_string(), "{listed}");
    assert!(row("float").get("how_to_declare").is_none(), "{listed}");
}

/// A conversion a node type registered by its descriptor alone states is not listed: the engine
/// builds no node of that type, so it offers no such conversion.
///
/// Mental-revert: reading conversions from every registered descriptor lists Rectangle's.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversion_no_node_can_be_built_for_is_not_listed() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        let port = |name: &str, pin_type: PinType| {
            PortDescriptor::new(name, "", true, PinDeclaration::exactly(pin_type))
            .with_delivery_profile("newest")
        };
        let descriptor = NodeDescriptor {
            conversion: true,
            ..NodeDescriptor::new(
                NodeClassShortName::new("RectangleToTensor").expect("a short name"),
                NodeClassImportPath::new("tests.described:RectangleToTensor").expect("a node type"),
                "a conversion registered by its descriptor alone",
            )
        }
        .with_input(port("rectangle", PinType::Rectangle {}))
        .with_output(port("array", PinType::Tensor {}));
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("the descriptor registers");
    });
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_pin_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the pin types are listed");

    let rectangle = listed["pin_types"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["name"] == "rectangle")
        .expect("Rectangle is listed");
    assert_eq!(rectangle["converts_to"], json!([]), "{rectangle}");
}

/// Each named type a source of the project declares is listed by its pin type, its
/// description and the source it comes from, an import's beside the project's own.
///
/// Mental-revert: listing the built-in rows alone answers no `named_pin_types`.
#[tokio::test(flavor = "multi_thread")]
async fn each_named_type_the_projects_code_declares_is_listed_with_its_source() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let named_pin_type = |import_path: &str, description: &str, path: &str| json!({ "named_pin_type": import_path, "description": description, "path": path, "line": 4 });
    let project = a_project_listing(json!([
        {
            "source": "project", "node_types": [], "graph_generators": [],
            "named_pin_types": [named_pin_type("faces:FaceBox", "A face found.", "faces.py")],
            "unreadable_modules": [],
        },
        {
            "source": "dependency", "project": "6f1c2a8e-0000-4000-8000-000000000001",
            "folder": "poses", "version": "v1", "node_types": [], "graph_generators": [],
            "named_pin_types": [named_pin_type("poses.kinds:Pose", "A body's pose.", "poses/kinds.py")],
            "unreadable_modules": [],
        },
    ]));

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_pin_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the pin types are listed");

    assert_eq!(
        listed["named_pin_types"],
        json!([
            {
                "source": "project",
                "pin_type": { "type": "named", "named_pin_type": "faces:FaceBox" },
                "documentation": "A face found.", "path": "faces.py", "line": 4,
            },
            {
                "source": "dependency", "project": "6f1c2a8e-0000-4000-8000-000000000001",
                "folder": "poses", "version": "v1", "project_name": null,
                "pin_type": { "type": "named", "named_pin_type": "poses.kinds:Pose" },
                "documentation": "A body's pose.", "path": "poses/kinds.py", "line": 4,
            },
        ]),
        "{listed}"
    );
    assert!(
        listed.get("named_pin_types_not_listed_because").is_none(),
        "{listed}"
    );
}

/// A project whose node code does not list still has every built-in pin type listed, and says
/// why no named type of it is.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_whose_code_does_not_list_answers_the_built_in_types_and_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_pin_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the built-in pin types are listed");

    assert_eq!(
        listed["pin_types"].as_array().map(Vec::len),
        Some(streamlib_node_schema::pin_type::BuiltInPinTypeName::EVERY_NAME.len()),
        "{listed}"
    );
    assert_eq!(listed["named_pin_types"], json!([]), "{listed}");
    assert!(
        listed["named_pin_types_not_listed_because"]
            .as_str()
            .is_some_and(|why| why.contains("were not listed")),
        "{listed}"
    );
}

/// `describe_node_types` naming no node type is refused before it reads the project.
///
/// Mental-revert: decoding the list as any list describes nothing and answers success.
#[tokio::test(flavor = "multi_thread")]
async fn describing_no_node_type_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([]));

    let answered = tool_call_result(
        engine.operations_on_the_loaded_streams(),
        "describe_node_types",
        json!({ "project_directory": project.path(), "node_types": [] }),
    )
    .await;

    assert_eq!(answered["isError"], true, "{answered}");
    assert!(
        answered.to_string().contains("names no node type"),
        "{answered}"
    );
}

/// The project's sources are listed as its interpreter read them, each node type by the first
/// line of its description and described from source while no loaded stream described it, a
/// graph generator with the settings it promotes, and the runtime's own node types last.
///
/// Mental-revert: leaving out the built-in source lists no `InputValueTestNode`.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_node_types_are_listed_by_source_with_the_runtimes_own_last() {
    register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([the_project_source()]));

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_node_types",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the node types are listed");

    assert_eq!(listed["project"], json!(project.project), "{listed}");
    let sources = listed["sources"].as_array().expect("the sources");
    assert_eq!(sources.len(), 2, "{listed}");
    assert_eq!(sources[0]["source"], "project");
    assert_eq!(
        sources[0]["node_types"],
        json!([
            { "node_type": "nodes.mix:Mixer", "documentation": "Mixes two levels.",
              "category": "Audio", "category_from": "declaration", "kind": "function",
              "described_from": "source" },
            { "node_type": "nodes.camera:Camera", "documentation": "A camera.",
              "category": null, "category_from": null, "kind": "process",
              "described_from": "source" },
        ])
    );
    assert_eq!(
        sources[0]["graph_generators"],
        json!([{
            "generator": "graphs:two_cameras", "description": "Two cameras side by side.",
            "promoted_settings": [{ "name": "spacing", "annotation": "float", "default": 2.0 }],
            "path": "graphs.py", "line": 12,
        }])
    );
    assert_eq!(sources[1]["source"], "built_in");
    let built_in = sources[1]["node_types"].as_array().expect("the built-ins");
    assert!(
        built_in.iter().any(|listed| listed
            == &json!({
                "node_type": InputValueTestNode::node_class_import_path().as_str(),
                "documentation": "", "category": null, "category_from": null,
                "kind": "process", "described_from": "built_in",
            })),
        "{listed}"
    );
    let names: Vec<&str> = built_in
        .iter()
        .filter_map(|listed| listed["node_type"].as_str())
        .collect();
    let (run_by_node_code, of_the_graph_itself) = names.split_at(names.len() - 4);
    assert!(run_by_node_code.is_sorted(), "{names:?}");
    assert_eq!(
        of_the_graph_itself,
        [
            "tatolab.stream:GraphInput",
            "tatolab.stream:GraphOutput",
            "tatolab.stream:SetVariable",
            "tatolab.stream:GetVariable",
        ],
        "the graph's own node types come last"
    );
}

/// A built-in node type is described from the runtime's own description: a typed input with its
/// mode, lifetime and value schema, a Texture input taking a link only, its config field as a setting read at setup with its field's schema, and its
/// typed outputs.
///
/// Mental-revert: describing a built-in from source answers `not_described_because`.
#[tokio::test(flavor = "multi_thread")]
async fn a_built_in_node_type_is_described_with_each_input_mode_value_schema_and_setting() {
    register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([]));
    let node_type = InputValueTestNode::node_class_import_path();

    let described = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "describe_node_types",
            json!({ "project_directory": project.path(), "node_types": [node_type.as_str()] }),
        )
        .await,
    )
    .expect("the node type is described");

    let [description] = described["node_types"]
        .as_array()
        .expect("the node types")
        .as_slice()
    else {
        panic!("one node type was named: {described}");
    };
    assert_eq!(description["source"], "built_in");
    assert_eq!(description["described_from"], "built_in");
    assert!(description.get("declared_at").is_none(), "{description}");
    assert_eq!(
        description["inputs"],
        json!([
            { "name": "gain", "documentation": "",
              "pin_type": { "kind": "is", "pin_type": { "type": "float" } }, "flow": "value",
              "mode": "wire_or_value", "takes_a_wire": true, "lifetime": "kept",
              "value_json_schema": { "type": "number", "minimum": -f64::MAX, "maximum": f64::MAX } },
            { "name": "video", "documentation": "",
              "pin_type": { "kind": "is", "pin_type": { "type": "texture" } }, "flow": "stream",
              "mode": "wire_only", "takes_a_wire": true, "lifetime": "kept" },
            { "name": "frame_width", "documentation": "", "mode": "setting_only",
              "takes_a_wire": false, "read": "at_setup", "lifetime": "kept",
              "value_json_schema": {
                  "type": ["integer", "null"], "format": "uint32", "minimum": 0.0,
                  "default": null,
              } },
        ])
    );
    assert_eq!(
        description["outputs"],
        json!([
            { "name": "level", "documentation": "",
              "pin_type": { "kind": "is", "pin_type": { "type": "float" } }, "flow": "value" },
            { "name": "count", "documentation": "",
              "pin_type": { "kind": "is", "pin_type": { "type": "integer" } }, "flow": "value" },
        ])
    );
}

/// Project node types no loaded stream described are described from source in the order named:
/// one whose source states its ports by annotation and literal default, and one that
/// registers its ports in code carrying why it is not described, its file named; neither refuses
/// the other.
///
/// Mental-revert: refusing the call for the one not described answers no description at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_node_type_is_described_from_source_or_says_why_it_is_not() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([the_project_source()]));

    let described = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "describe_node_types",
            json!({ "project_directory": project.path(),
                    "node_types": ["nodes.camera:Camera", "nodes.mix:Mixer"] }),
        )
        .await,
    )
    .expect("the node types are described");

    let [camera, mixer] = described["node_types"]
        .as_array()
        .expect("the node types")
        .as_slice()
    else {
        panic!("two node types were named: {described}");
    };
    assert_eq!(camera["node_type"], "nodes.camera:Camera");
    assert_eq!(camera["source"], "project");
    let not_described_because = &camera["not_described_because"];
    assert_eq!(not_described_because["refusal_code"], "refused", "{camera}");
    assert_eq!(
        not_described_because["refused_file"],
        json!({ "path": "nodes/camera.py", "line": 3 })
    );
    assert!(
        not_described_because["runtime_words"]
            .as_str()
            .is_some_and(|words| words.contains("`nodes.camera:Camera`")),
        "{camera}"
    );
    assert_eq!(
        mixer,
        &json!({
            "node_type": "nodes.mix:Mixer", "source": "project", "described_from": "source",
            "documentation": "Mixes two levels.\n\nThe second is scaled first.",
            "category": "Audio", "category_from": "declaration", "kind": "function",
            "declared_at": { "path": "nodes/mix.py", "line": 7 },
            "inputs": [{ "name": "level", "documentation": "the level",
                         "takes_a_wire": true, "default_value": 0.5 }],
            "outputs": [{ "name": "mixed", "documentation": "" }],
        })
    );
}

/// A node type neither the project nor the runtime offers refuses the whole call with
/// `node_type_not_found`, suggesting the call that lists the ones they do.
///
/// Mental-revert: describing the rest and leaving it out answers no refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_type_not_offered_refuses_the_call_suggesting_list_node_types() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_listing(json!([the_project_source()]));

    let answered = tool_call_result(
        engine.operations_on_the_loaded_streams(),
        "describe_node_types",
        json!({ "project_directory": project.path(),
                "node_types": ["nodes.mix:Mixer", "nodes.gone:Gone"] }),
    )
    .await;

    let refusal = crate::mcp::tests::the_refusal_of_a_tool_result(&answered);
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::NodeTypeNotFound,
        "{answered}"
    );
    assert!(
        refusal.runtime_words.contains("`nodes.gone:Gone`"),
        "{answered}"
    );
    let suggested = refusal.suggested_call.expect("the listing is suggested");
    assert_eq!(suggested.operation, "list_node_types");
    assert_eq!(
        suggested.params["project_directory"],
        json!(project.resolved_project_directory),
        "{answered}"
    );
}

/// A project node type a loaded stream described in its interpreter is described from that
/// description, even where its source states no ports, keeping its source and where it is
/// declared.
///
/// Mental-revert: preferring the source's ports answers `not_described_because`.
#[test]
fn a_project_node_type_an_interpreter_described_is_described_from_that_description() {
    use streamlib_runtime_client_contract::local_api_wire_contract::{
        NodeTypeDescribedOrWhyNot, ProjectNodeTypesReadFromSource,
    };

    register_the_input_value_test_node_once();
    let node_type = InputValueTestNode::node_class_import_path();
    let listing: ProjectNodeTypesReadFromSource = serde_json::from_value(json!({
        "project_directory": "/projects/cell", "project": Uuid::new_v4(), "project_name": "cell",
        "sources": [{
            "source": "project",
            "node_types": [{
                "type": node_type.as_str(), "kind": "process", "description": "",
                "category": null, "inputs": null, "outputs": null,
                "path": "nodes/mixer.py", "line": 9,
            }],
            "graph_generators": [], "unreadable_modules": [],
        }],
    }))
    .expect("a listing");
    let source = &listing.sources[0];
    let descriptor = NODE_REGISTRY
        .list_registered()
        .into_iter()
        .find(|descriptor| descriptor.node_class_import_path == node_type)
        .expect("the node type is registered");

    let described = crate::control_operations_on_node_types::the_project_node_type_described(
        &listing,
        &source.origin,
        &source.node_types[0],
        Some(&descriptor),
    );

    let NodeTypeDescribedOrWhyNot::Described(description) = described else {
        panic!("an interpreter's description describes it: {described:?}");
    };
    let description = serde_json::to_value(&description).expect("a description serializes");
    assert_eq!(description["source"], "project");
    assert_eq!(description["described_from"], "interpreter");
    assert_eq!(
        description["declared_at"],
        json!({ "path": "nodes/mixer.py", "line": 9 })
    );
    assert_eq!(description["inputs"][0]["name"], "gain", "{description}");
    assert_eq!(
        description["inputs"][0]["mode"], "wire_or_value",
        "{description}"
    );
}

/// A value set on, and a link made into, a port the node does not have are refused suggesting
/// `describe_node_types` on the node's type in its project.
///
/// Mental-revert: carrying no suggestion on a graph refusal answers none.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_the_node_does_not_have_is_refused_suggesting_describe_node_types() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_a_mixer();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    for (tool_name, arguments) in [
        (
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": MIXER_NODE_UUID, "input": "loudness",
                    "value": 1, "author": an_agent_named("alice") }),
        ),
        (
            "connect",
            json!({ "stream": stream_asset, "author": an_agent_named("alice"),
                    "source": { "node": MIXER_NODE_UUID, "port": "level" },
                    "target": { "node": MIXER_NODE_UUID, "port": "loudness" } }),
        ),
    ] {
        let refusal = engine
            .the_answer_or_the_refusal_of(tool_name, arguments)
            .await
            .expect_err("the mixer has no input `loudness`");

        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::PortNotFound,
            "{tool_name}: {refusal:?}"
        );
        let suggested = refusal
            .suggested_call
            .unwrap_or_else(|| panic!("{tool_name} suggests no call"));
        assert_eq!(suggested.operation, "describe_node_types");
        assert_eq!(
            Value::Object(suggested.params),
            json!({ "project_directory": project.resolved_project_directory,
                    "node_types": [InputValueTestNode::node_class_import_path().as_str()] }),
            "{tool_name}"
        );
    }
}
