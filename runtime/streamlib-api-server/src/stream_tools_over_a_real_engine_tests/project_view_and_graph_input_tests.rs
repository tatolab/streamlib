// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's views listed by view type, a view node loaded with the view's ports, and a top-level
//! graph input loaded linked to its readers.

use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::*;

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Why `project`'s stream `stream_name` is refused loading from its graph document.
    fn a_stream_refused_loading_from_its_graph_document(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> streamlib::sdk::error::Error {
        let stream_location = project.location_of(stream_name);
        let stream_id = stream_location.stream_id_named(TEST_PROJECT_NAME, stream_name);
        match self.engine.load_stream_from_graph_snapshot(
            &GraphSnapshot::from_graph_document(project.the_graph_document_of(stream_name))
                .expect("the graph document parses"),
            OptionsForLoadingOneStream::at_stream_location(stream_id, stream_location)
                .in_stream_environment(project.stream_environment())
                .with_display_name(stream_name),
        ) {
            Ok(_) => panic!("the stream `{stream_name}` loads, and a refusal was expected"),
            Err(refusal) => refusal,
        }
    }
}

/// Write `manifest` as the `tatolab_view.json` of `views/<view_folder_name>/` in `project_directory`.
fn a_view_folder_in(project_directory: &Path, view_folder_name: &str, manifest: &Value) {
    let folder = project_directory.join("views").join(view_folder_name);
    std::fs::create_dir_all(&folder).expect("the view's folder");
    std::fs::write(folder.join("tatolab_view.json"), manifest.to_string()).expect("its manifest");
}

/// Project `cell` brings the view `deck` and imports project `levels`, which brings `meter` and a
/// folder whose manifest is not JSON: each is listed under the project bringing it, and the one
/// that does not open says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_views_and_its_imports_views_are_listed_by_view_type() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let levels = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    std::fs::write(
        levels.path().join("tatolab-project.json"),
        json!({ "project": levels.project, "name": "levels", "dependencies": [] }).to_string(),
    )
    .expect("the import's project file names it");
    a_view_folder_in(
        levels.path(),
        "meter",
        &json!({ "contract": "1.0", "title": "Meter", "entry": "index.html", "placement": "area" }),
    );
    let broken_folder = levels.path().join("views/broken");
    std::fs::create_dir_all(&broken_folder).expect("the broken view's folder");
    std::fs::write(broken_folder.join("tatolab_view.json"), "{").expect("a manifest cut short");
    let cell = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let deck_manifest = json!({
        "contract": "1.1", "title": "DJ Deck", "description": "Two decks and a crossfader.",
        "icon": "icon.svg", "preview": "preview.png", "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "waveform", "pin_type": { "type": "audio" } }],
        "outputs": [{ "name": "crossfader", "pin_type": { "type": "tensor" }, "range": [-1, 1] }],
    });
    a_view_folder_in(cell.path(), "deck", &deck_manifest);
    a_view_folder_in(
        cell.path(),
        "older",
        &json!({ "contract": "1.0", "title": "Older", "entry": "index.html", "placement": "node",
                 "inputs": [{ "name": "picture", "family": "image" }] }),
    );
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };
    let added = call(
        "add_dependency",
        json!({ "project_directory": cell.path(), "source": levels.path().display().to_string(),
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("levels is imported");
    let folder = added["dependency"]["folder"].clone();

    let listed = call(
        "list_project_views",
        json!({ "project_directory": cell.path() }),
    )
    .await
    .expect("the cell's views are listed");

    let deck_manifest_once_listed: Value = serde_json::from_slice(
        &std::fs::read(cell.path().join("views/deck/tatolab_view.json"))
            .expect("the deck's manifest reads"),
    )
    .expect("the deck's manifest is JSON");
    let deck_view = deck_manifest_once_listed["view"]
        .as_str()
        .expect("the project gives its own view naming no UUID one as it opens")
        .to_owned();
    let mut deck_manifest_given_its_uuid = deck_manifest.clone();
    deck_manifest_given_its_uuid["view"] = json!(deck_view);
    assert_eq!(deck_manifest_once_listed, deck_manifest_given_its_uuid);

    assert_eq!(listed["project"], json!(cell.project), "{listed}");
    assert_eq!(listed["project_name"], TEST_PROJECT_NAME, "{listed}");
    let views = listed["views"].as_array().expect("a list of views");
    assert_eq!(views.len(), 4, "{listed}");
    assert_eq!(
        views[0],
        json!({
            "view_type": deck_view,
            "project": cell.project, "project_name": TEST_PROJECT_NAME, "import_folder": null,
            "view_folder_in_the_project": "views/deck",
            "opens": true, "not_opened_because": null,
            "title": "DJ Deck", "description": "Two decks and a crossfader.",
            "icon": "icon.svg", "preview": "preview.png", "placement": "node",
            "inputs": [{ "name": "waveform", "pin_type": { "type": "audio" },
                         "description": null, "range": null }],
            "outputs": [{ "name": "crossfader", "pin_type": { "type": "tensor" },
                          "description": null, "range": [-1.0, 1.0] }],
            "manifest": deck_manifest_given_its_uuid,
        })
    );
    assert!(
        views[1]["view_type"]
            .as_str()
            .is_some_and(|view_type| Uuid::parse_str(view_type).is_ok()),
        "a view whose manifest does not read is given a UUID too: {listed}"
    );
    assert_eq!(
        views[1]["view_type"], views[1]["manifest"]["view"],
        "{listed}"
    );
    assert_eq!(views[1]["opens"], false, "{listed}");
    assert_eq!(
        views[1]["not_opened_because"],
        "`inputs[0]`: port `picture` is declared by the older `family` field, which a view's \
         port no longer names; write `\"pin_type\": {\"type\": \"texture\"}` in place of \
         `\"family\": \"image\"`, with `\"contract\": \"1.1\"`.",
        "a view whose port is declared by `family` is listed with the reader's words: {listed}"
    );
    let views = &views[1..];
    assert_eq!(
        views[1]["view_type"],
        format!("{}:broken", levels.project),
        "{listed}"
    );
    assert_eq!(views[1]["opens"], false, "{listed}");
    assert_eq!(views[1]["import_folder"], folder, "{listed}");
    assert!(
        views[1]["not_opened_because"]
            .as_str()
            .is_some_and(|why| why.starts_with("`tatolab_view.json` is not JSON: ")),
        "{listed}"
    );
    assert_eq!(
        views[2]["view_type"],
        format!("{}:meter", levels.project),
        "{listed}"
    );
    assert_eq!(views[2]["project"], json!(levels.project), "{listed}");
    assert_eq!(views[2]["project_name"], "levels", "{listed}");
    assert_eq!(views[2]["placement"], "area", "{listed}");
    assert_eq!(
        views[2]["view_folder_in_the_project"],
        json!(format!(
            "imports/{}/views/meter",
            folder.as_str().expect("a folder name")
        )),
        "{listed}"
    );
}

/// A listing whose interpreter prints no listing is refused naming the project, in the
/// runtime's own words.
/// The UUID the graph document of the view-node test gives its `deck`.
const DECK_NODE_UUID: &str = "decdecde-cdec-4dec-8dec-decdecdecdec";

/// The UUID the manifest of the view-node test's `deck` names.
const DECK_VIEW_UUID: &str = "5d1e4a7c-9b3f-4e26-a8d0-c7f2b1e94a63";

/// A graph document holding a node of the project's view `deck` loads it with the view's ports,
/// each of the pin type its manifest names, and the graph document on disk is unchanged by the
/// load.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_document_holding_a_view_node_loads_it_with_the_views_ports() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    register_the_stream_tools_test_source_once();
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"].as_array_mut().unwrap().push(json!({
        "id": DECK_NODE_UUID, "name": "deck", "type": format!("{TEST_PROJECT_NAME}:deck"),
        "input_values": { "bpm": 128 },
    }));
    let project = ProjectWithGraphDocuments::holding(graph);
    a_view_folder_in(
        project.path(),
        "deck",
        &json!({
            "contract": "1.1", "title": "Deck", "entry": "index.html", "placement": "node",
            "inputs": [{ "name": "waveform", "pin_type": { "type": "audio" } }],
            "outputs": [{ "name": "crossfader", "pin_type": { "type": "tensor" }, "range": [-1, 1] }],
        }),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let document_before = project.the_graph_document_of("main");

    let live_graph = engine.the_live_graph_of(&stream_id);
    let deck = the_node_named(&live_graph, "deck");
    assert_eq!(deck["id"], DECK_NODE_UUID, "{deck}");
    assert_eq!(deck["type"], format!("{}:deck", project.project), "{deck}");
    assert!(
        deck.to_string().contains("waveform") && deck.to_string().contains("crossfader"),
        "the view node takes the view's ports: {deck}"
    );
    let waveform = super::input_value_set_tests::the_input_named(&deck, "waveform");
    assert_eq!(
        waveform["pin_type"],
        json!({ "type": "audio" }),
        "{waveform}"
    );
    assert_eq!(waveform["flow"], "stream", "{waveform}");

    assert_eq!(project.the_graph_document_of("main"), document_before);
}

/// A view node renders each setting its manifest declares as an input before its ports, taking
/// no wire and reading its config: `bpm` and `label` typed, as their schemas state a type
/// exactly, and `curve`, free-form, of no pin type. The input values `graph` serves keep their
/// keys, an untyped setting is still checked against its schema, and the graph document on disk is
/// unchanged by the load.
///
/// Mental-revert: rendering the view node's declared ports alone leaves `bpm` out of its inputs.
#[tokio::test(flavor = "multi_thread")]
async fn a_view_node_renders_its_manifest_settings_as_inputs_reading_its_config() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    register_the_stream_tools_test_source_once();
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"].as_array_mut().unwrap().push(json!({
        "id": DECK_NODE_UUID, "name": "deck", "type": format!("{TEST_PROJECT_NAME}:deck"),
        "input_values": { "bpm": 128, "curve": { "ease": "in" } },
    }));
    let project = ProjectWithGraphDocuments::holding(graph);
    a_view_folder_in(
        project.path(),
        "deck",
        &json!({
            "contract": "1.1", "view": DECK_VIEW_UUID, "title": "Deck", "entry": "index.html",
            "placement": "node",
            "inputs": [{ "name": "waveform", "pin_type": { "type": "audio" } }],
            "settings": { "type": "object", "properties": {
                "bpm": { "type": "number", "default": 120, "description": "Beats a minute." },
                "label": { "type": "string", "default": "Deck A" },
                "curve": { "type": "object", "default": { "ease": "out" } },
            } },
        }),
    );
    let document_before = project.the_graph_document_of("main");
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    assert_eq!(project.the_graph_document_of("main"), document_before);

    let answered = engine
        .the_answer_or_the_refusal_of("graph", json!({ "stream": stream_id }))
        .await
        .expect("the graph is answered");
    let deck = the_node_named(&answered, "deck");
    let input_names: Vec<&str> = deck["ports"]["inputs"]
        .as_array()
        .expect("inputs")
        .iter()
        .map(|input| input["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(input_names, ["bpm", "label", "curve", "waveform"], "{deck}");
    let bpm = super::input_value_set_tests::the_input_named(&deck, "bpm");
    assert_eq!(bpm["pin_type"], json!({ "type": "float" }), "{bpm}");
    assert_eq!(bpm["takes_a_wire"], false, "{bpm}");
    assert_eq!(bpm["description"], "Beats a minute.", "{bpm}");
    assert_eq!(bpm["value"], 128, "{bpm}");
    assert_eq!(bpm["value_comes_from"], "saved", "{bpm}");
    let label = super::input_value_set_tests::the_input_named(&deck, "label");
    assert_eq!(label["pin_type"], json!({ "type": "string" }), "{label}");
    assert_eq!(label["value"], "Deck A", "{label}");
    assert_eq!(label["value_comes_from"], "default", "{label}");
    let curve = super::input_value_set_tests::the_input_named(&deck, "curve");
    assert!(curve.get("pin_type").is_none(), "{curve}");
    assert_eq!(curve["takes_a_wire"], false, "{curve}");
    assert_eq!(curve["value"], json!({ "ease": "in" }), "{curve}");
    assert_eq!(
        deck["input_values"],
        json!({ "bpm": 128, "curve": { "ease": "in" } }),
        "{deck}"
    );

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": DECK_NODE_UUID,
                    "input": "curve", "value": 5 }),
        )
        .await
        .expect_err("`curve` takes only an object, as its schema states");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputValueNotValid
    );
    assert!(
        refusal.runtime_words.contains("config schema")
            && refusal.runtime_words.contains("not of type \"object\""),
        "{}",
        refusal.runtime_words
    );
}

/// A native reader of Strings on its input `cue`, which does nothing with them.
#[streamlib::sdk::node(
    execution = manual,
    input("cue", delivery_profile = "newest", pin_type = String, flow = Stream)
)]
pub(super) fn stream_tools_test_records_reader() {}

/// The UUID the graph document of the Graph Input test gives its `cue` Input.
const CUE_GRAPH_INPUT_NODE_UUID: &str = "c0ec0ec0-ec0e-4c0e-8c0e-c0ec0ec0ec0e";

/// The graph of `main` with a top-level String `GraphInput` declaring `cue`, holding `default`
/// as its config's, read by two [`StreamToolsTestRecordsReader`]s.
fn the_graph_of_a_stream_whose_cue_input_two_nodes_read(default: Value) -> Value {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<StreamToolsTestRecordsReader::Node>());
    let reader_type = StreamToolsTestRecordsReader::node_class_import_path();
    json!({
        "stream": "main",
        "nodes": [
            { "id": CUE_GRAPH_INPUT_NODE_UUID, "name": "cue", "type": "tatolab.stream:GraphInput",
              "input_values": { "port": "cue", "pin_type": { "type": "string" }, "default": default } },
            { "name": "left", "type": reader_type.as_str() },
            { "name": "right", "type": reader_type.as_str() },
        ],
        "links": [
            { "source": { "node": "cue", "port": "value" }, "target": { "node": "left", "port": "cue" } },
            { "source": { "node": "cue", "port": "value" }, "target": { "node": "right", "port": "cue" } },
        ],
    })
}

/// A stream's top-level Records `GraphInput` loads as a running node linked to both its readers,
/// rendered as an interface node carrying its default, and the load records nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_top_level_graph_input_loads_linked_to_its_readers() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_whose_cue_input_two_nodes_read(json!("intro")),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let document_before = project.the_graph_document_of("main");

    let live_graph = engine.the_live_graph_of(&stream_id);
    let cue = the_node_named(&live_graph, "cue");
    assert_eq!(cue["id"], CUE_GRAPH_INPUT_NODE_UUID, "{cue}");
    assert_eq!(cue["input_values"]["value"], json!("intro"), "{cue}");
    assert_eq!(
        cue["components"]["graph_interface"],
        json!({
            "direction": "input",
            "port": "cue",
            "pin_type": { "type": "string" },
            "current_value": "intro"
        }),
        "a node not running shows the value it carries at its next Play, its default: {cue}"
    );
    assert_eq!(cue["ports"]["outputs"][0]["name"], "value", "{cue}");
    let links_out_of_the_input: Vec<&Value> = live_graph["links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|link| link["source"]["node"] == "cue")
        .collect();
    assert_eq!(links_out_of_the_input.len(), 2, "{live_graph}");
    for link in links_out_of_the_input {
        assert!(
            !link["id"].as_str().unwrap_or_default().is_empty(),
            "a running Input's links are links the runtime runs: {link}"
        );
        assert_eq!(
            link["components"]["graph_interface"]["port"], "cue",
            "{link}"
        );
    }

    assert_eq!(project.the_graph_document_of("main"), document_before);
}

/// `set_node_input_value` naming no node sets the `value` of the top-level `GraphInput`
/// declaring the stream input it names: saved in the graph document, whose `default` it
/// replaces, and rendered as the Input's `default`. One naming an input no `GraphInput` declares
/// is refused with `port_not_found`, naming the inputs the stream has; one below the top level
/// is refused; neither records anything.
#[tokio::test(flavor = "multi_thread")]
async fn a_set_naming_no_node_saves_the_value_of_the_graph_input_declaring_the_input() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_whose_cue_input_two_nodes_read(json!("intro")),
    );
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");

    engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "input": "cue", "value": "drop",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the stream input is saved");

    let document = project.the_graph_document_of("main");
    let saved_cue = document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["id"] == CUE_GRAPH_INPUT_NODE_UUID)
        .unwrap_or_else(|| panic!("the document holds the Input: {document}"));
    assert_eq!(
        saved_cue["input_values"]["value"],
        json!("drop"),
        "{saved_cue}"
    );
    assert!(
        saved_cue["input_values"].get("default").is_none(),
        "{saved_cue}"
    );
    let cue = the_node_named(&engine.the_live_graph_of(&stream_id), "cue");
    assert_eq!(cue["input_values"]["value"], json!("drop"), "{cue}");
    let document_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "input": "fader", "value": 1,
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("an input no GraphInput declares is refused");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::PortNotFound,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_input.as_deref(),
        Some("fader"),
        "{refusal:?}"
    );
    assert!(
        refusal.runtime_words.contains("`fader`") && refusal.runtime_words.contains("`cue`"),
        "the refusal names the inputs the stream has: {refusal:?}"
    );
    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "level": [CUE_GRAPH_INPUT_NODE_UUID],
                    "input": "cue", "value": "outro",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a set naming no node below the top level is refused");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::Refused,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_params_pointer.as_deref(),
        Some("/level"),
        "{refusal:?}"
    );
    assert!(
        refusal.runtime_words.contains(
            "a `set_node_input_value` naming no node sets an input of the stream, which a \
             top-level `GraphInput` declares, so its `level` is the top level; the input `cue` \
             was named at a level below it. To fix: leave `level` out, or name the node whose \
             input it is"
        ),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
}

/// A `GraphInput` whose `default` its pin type does not take is refused at load, naming the node.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_input_default_its_pin_type_does_not_take_is_refused_at_load() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_whose_cue_input_two_nodes_read(json!(3)),
    );

    let refusal = engine
        .a_stream_refused_loading_from_its_graph_document(&project, "main")
        .to_string();

    assert!(
        refusal.contains("`cue`") && refusal.contains("`default`") && refusal.contains("String"),
        "{refusal}"
    );
}
