// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Value, json};
use streamlib_project_repository::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentLinkEnd, StreamGraphOperation,
    StreamGraphSelectionCollapsedIntoAPlacedStream,
};
use uuid::Uuid;

const SOURCE: &str = "00000000-0000-4000-8000-00000000000a";
const BLUR: &str = "00000000-0000-4000-8000-00000000000b";
const TINT: &str = "00000000-0000-4000-8000-00000000000c";
const SINK: &str = "00000000-0000-4000-8000-00000000000d";
const PREVIEW: &str = "00000000-0000-4000-8000-00000000000e";
const LOOK_STREAM: &str = "00000000-0000-4000-8000-000000000002";

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

fn a_link(source: &str, source_port: &str, target: &str, target_port: &str) -> Value {
    json!({ "source": { "node": source, "port": source_port },
            "target": { "node": target, "port": target_port } })
}

/// `source -> blur -> tint -> sink`, with `tint` also feeding `preview`.
fn a_chain_of_five_nodes() -> StreamGraphDocument {
    let node = |id: &str, name: &str| {
        json!({ "id": id, "name": name, "type": format!("looks.nodes:{name}"),
                "config": { "strength": 1 }, "studio": { "x": 10 } })
    };
    let grouped = |id: &str, name: &str| {
        json!({ "id": id, "name": name, "type": format!("looks.nodes:{name}"),
                "config": { "strength": 1 },
                "studio": { "x": 10, "group": "00000000-0000-4000-8000-0000000000c0" } })
    };
    StreamGraphDocument::from_value(json!({
        "asset": "00000000-0000-4000-8000-000000000001",
        "display_name": "main",
        "nodes": [
            node(SOURCE, "source"), grouped(BLUR, "blur"), node(TINT, "tint"),
            node(SINK, "sink"), node(PREVIEW, "preview"),
        ],
        "groups": [{ "id": "00000000-0000-4000-8000-0000000000c0", "title": "looks" }],
        "links": [
            a_link(SOURCE, "video", BLUR, "video"),
            a_link(BLUR, "video", TINT, "video"),
            a_link(TINT, "video", SINK, "video"),
            a_link(TINT, "video", PREVIEW, "video"),
        ],
    }))
    .unwrap()
}

fn every_output_port_is_an_image(_: &StreamGraphDocumentLinkEnd) -> Result<String, String> {
    Ok("image".to_owned())
}

fn fresh_uuids() -> impl FnMut() -> Uuid {
    let mut next = 0x100u128;
    move || {
        next += 1;
        Uuid::from_u128(next)
    }
}

fn applied(
    document: &StreamGraphDocument,
    operation: &StreamGraphOperation,
) -> StreamGraphDocument {
    let mut applied = document.clone();
    applied.apply(operation).unwrap();
    applied
}

fn links_of(document: &StreamGraphDocument) -> Vec<String> {
    let name_of = |node: Uuid| {
        document
            .node(node)
            .map(|node| node.name)
            .unwrap_or_else(|_| node.to_string())
    };
    let mut links: Vec<String> = document
        .links()
        .into_iter()
        .map(|link| {
            format!(
                "{}/{} -> {}/{}",
                name_of(link.source.node),
                link.source.port,
                name_of(link.target.node),
                link.target.port
            )
        })
        .collect();
    links.sort();
    links
}

fn collapse_blur_and_tint(
    document: &StreamGraphDocument,
) -> StreamGraphSelectionCollapsedIntoAPlacedStream {
    document
        .the_collapse_of_nodes_into_a_placed_stream(
            &[uuid(BLUR), uuid(TINT)],
            Uuid::from_u128(0x99),
            "look",
            uuid(LOOK_STREAM),
            &every_output_port_is_an_image,
            &mut fresh_uuids(),
        )
        .unwrap()
}

fn the_fold_of(collapse: &StreamGraphSelectionCollapsedIntoAPlacedStream) -> StreamGraphOperation {
    StreamGraphOperation::FoldIntoFragment {
        fold: collapse.fold.clone(),
    }
}

fn no_placed_stream_is_read(_: &Value) -> Result<StreamGraphDocument, String> {
    Err("no placed stream is read here".to_owned())
}

#[test]
fn a_collapsed_selection_is_a_new_stream_placed_where_its_nodes_were() {
    let document = a_chain_of_five_nodes();
    let collapse = collapse_blur_and_tint(&document);

    let collapsed = applied(&document, &the_fold_of(&collapse));

    assert_eq!(
        collapsed.groups().unwrap(),
        document.groups().unwrap(),
        "the frame stays in the stream that collapsed its node"
    );
    let names: Vec<String> = collapsed
        .nodes()
        .into_iter()
        .map(|node| node.name)
        .collect();
    assert_eq!(names, ["source", "look", "sink", "preview"]);
    let look = collapsed.node(Uuid::from_u128(0x99)).unwrap();
    assert_eq!(look.node_type, STREAM_FRAGMENT_NODE_TYPE);
    assert_eq!(look.config, json!({ "stream": { "asset": LOOK_STREAM } }));
    assert!(look.fragment.is_none());
    assert_eq!(
        links_of(&collapsed),
        [
            "look/tint-video -> preview/video",
            "look/tint-video -> sink/video",
            "source/video -> look/blur-video",
        ]
    );
    let saved = StreamGraphDocument::from_value(collapse.placed_stream_document.clone()).unwrap();
    assert_eq!(collapse.placed_stream_document["asset"], LOOK_STREAM);
    let inner: Vec<(String, String, Value)> = saved
        .nodes()
        .into_iter()
        .map(|node| (node.name, node.node_type, node.config))
        .collect();
    assert_eq!(
        inner,
        [
            (
                "blur".to_owned(),
                "looks.nodes:blur".to_owned(),
                json!({ "strength": 1 })
            ),
            (
                "tint".to_owned(),
                "looks.nodes:tint".to_owned(),
                json!({ "strength": 1 })
            ),
            (
                "blur-video".to_owned(),
                GRAPH_INPUT_NODE_TYPE.to_owned(),
                json!({ "port": "blur-video", "family": "image" })
            ),
            (
                "tint-video".to_owned(),
                GRAPH_OUTPUT_NODE_TYPE.to_owned(),
                json!({ "port": "tint-video", "family": "image" })
            ),
        ]
    );
    assert_eq!(
        saved.node(uuid(BLUR)).unwrap().studio,
        serde_json::from_value(json!({ "x": 10 })).unwrap(),
        "a node leaves the placing stream's groups behind"
    );
    assert_eq!(
        links_of(&saved),
        [
            "blur-video/value -> blur/video",
            "blur/video -> tint/video",
            "tint/video -> tint-video/value",
        ]
    );
}

#[test]
fn a_collapse_and_its_undo_give_back_the_exact_earlier_document() {
    let document = a_chain_of_five_nodes();
    let collapse = the_fold_of(&collapse_blur_and_tint(&document));
    let collapsed = applied(&document, &collapse);

    let undone = applied(&collapsed, &collapse.inverse());

    assert_eq!(undone.to_value(), document.to_value());
    assert_eq!(applied(&undone, &collapse).to_value(), collapsed.to_value());
}

#[test]
fn an_unlocked_placed_stream_puts_a_copy_of_its_nodes_in_place_and_its_undo_places_it_again() {
    let document = a_chain_of_five_nodes();
    let collapse = collapse_blur_and_tint(&document);
    let collapsed = applied(&document, &the_fold_of(&collapse));
    let saved = StreamGraphDocument::from_value(collapse.placed_stream_document.clone()).unwrap();
    let the_saved_stream = |held_stream: &Value| {
        assert_eq!(*held_stream, json!({ "asset": LOOK_STREAM }));
        Ok(saved.clone())
    };
    let unlock = StreamGraphOperation::UnfoldFragment {
        fold: collapsed
            .the_fold_a_fragment_unfolds(
                Uuid::from_u128(0x99),
                &the_saved_stream,
                &mut fresh_uuids(),
            )
            .unwrap(),
    };

    let unlocked = applied(&collapsed, &unlock);

    let names: Vec<String> = unlocked.nodes().into_iter().map(|node| node.name).collect();
    assert_eq!(names, ["source", "blur", "tint", "sink", "preview"]);
    assert_eq!(links_of(&unlocked), links_of(&document));
    assert!(unlocked.fragment_nodes().is_empty());
    for copied in ["blur", "tint"] {
        let copy = unlocked
            .nodes()
            .into_iter()
            .find(|node| node.name == copied)
            .unwrap();
        assert!(
            copy.id != uuid(BLUR) && copy.id != uuid(TINT),
            "`{copied}` comes back under a fresh UUID"
        );
    }
    assert_eq!(
        applied(&unlocked, &unlock.inverse()).to_value(),
        collapsed.to_value()
    );
}

#[test]
fn an_unlocked_placed_stream_passing_its_input_straight_to_its_output_links_the_source_to_the_reader()
 {
    let placing = "00000000-0000-4000-8000-0000000000a0";
    let passing_input = "00000000-0000-4000-8000-0000000000a1";
    let passing_output = "00000000-0000-4000-8000-0000000000a2";
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [
            { "id": SOURCE, "name": "source", "type": "looks.nodes:Source", "config": {} },
            { "id": placing, "name": "look", "type": STREAM_FRAGMENT_NODE_TYPE,
              "config": { "stream": { "asset": LOOK_STREAM } } },
            { "id": SINK, "name": "sink", "type": "looks.nodes:Sink", "config": {} },
        ],
        "links": [
            a_link(SOURCE, "video", placing, "video"),
            a_link(placing, "graded", SINK, "video"),
        ],
    }))
    .unwrap();
    let passing_straight_through = StreamGraphDocument::from_value(json!({
        "asset": LOOK_STREAM,
        "nodes": [
            { "id": passing_input, "name": "video", "type": GRAPH_INPUT_NODE_TYPE,
              "config": { "port": "video", "family": "image" } },
            { "id": passing_output, "name": "graded", "type": GRAPH_OUTPUT_NODE_TYPE,
              "config": { "port": "graded", "family": "image" } },
        ],
        "links": [a_link(passing_input, "value", passing_output, "value")],
    }))
    .unwrap();
    let unlock = StreamGraphOperation::UnfoldFragment {
        fold: document
            .the_fold_a_fragment_unfolds(
                uuid(placing),
                &|_: &Value| Ok(passing_straight_through.clone()),
                &mut fresh_uuids(),
            )
            .unwrap(),
    };

    let unlocked = applied(&document, &unlock);

    assert_eq!(links_of(&unlocked), ["source/video -> sink/video"]);
    assert_eq!(
        applied(&unlocked, &unlock.inverse()).to_value(),
        document.to_value()
    );
}

#[test]
fn a_generators_frozen_output_unlocked_is_plain_nodes_a_refresh_no_longer_reaches() {
    let generated_blur = "00000000-0000-4000-8000-0000000000f1";
    let generated_input = "00000000-0000-4000-8000-0000000000f2";
    let generated_output = "00000000-0000-4000-8000-0000000000f3";
    let look = "00000000-0000-4000-8000-0000000000f0";
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [
            { "id": SOURCE, "name": "source", "type": "looks.nodes:Source", "config": {} },
            { "id": look, "name": "look", "type": "looks.generators:look", "config": { "soft": true },
              "fragment": {
                  "nodes": [
                      { "id": generated_input, "name": "graphinput", "type": GRAPH_INPUT_NODE_TYPE,
                        "config": { "port": "video", "family": "image" } },
                      { "id": generated_blur, "name": "soft-blur", "type": "looks.nodes:Blur",
                        "config": { "radius": 3 } },
                      { "id": generated_output, "name": "graphoutput", "type": GRAPH_OUTPUT_NODE_TYPE,
                        "config": { "port": "out", "family": "image" } },
                  ],
                  "links": [
                      a_link(generated_input, "value", generated_blur, "video"),
                      a_link(generated_blur, "video", generated_output, "value"),
                  ],
              } },
            { "id": SINK, "name": "sink", "type": "looks.nodes:Sink", "config": {} },
        ],
        "links": [
            a_link(SOURCE, "video", look, "video"),
            a_link(look, "out", SINK, "video"),
        ],
    }))
    .unwrap();
    assert_eq!(
        document.fragment_nodes()[0].graph_generators_a_refresh_runs(),
        ["looks.generators:look"]
    );

    let unlock = StreamGraphOperation::UnfoldFragment {
        fold: document
            .the_fold_a_fragment_unfolds(uuid(look), &no_placed_stream_is_read, &mut fresh_uuids())
            .unwrap(),
    };
    let unlocked = applied(&document, &unlock);

    let names: Vec<String> = unlocked.nodes().into_iter().map(|node| node.name).collect();
    assert_eq!(names, ["source", "soft-blur", "sink"]);
    assert_eq!(
        unlocked.node(uuid(generated_blur)).unwrap().config,
        json!({ "radius": 3 })
    );
    assert_eq!(
        links_of(&unlocked),
        [
            "soft-blur/video -> sink/video",
            "source/video -> soft-blur/video"
        ]
    );
    assert!(unlocked.fragment_nodes().is_empty());
    assert_eq!(
        applied(&unlocked, &unlock.inverse()).to_value(),
        document.to_value()
    );
}

#[test]
fn a_stream_interface_node_and_a_name_clash_are_refused() {
    let input = "00000000-0000-4000-8000-0000000000a1";
    let placed = "00000000-0000-4000-8000-0000000000a2";
    let inner_blur = "00000000-0000-4000-8000-0000000000a4";
    let look = "00000000-0000-4000-8000-0000000000a3";
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [
            { "id": input, "name": "camera-in", "type": GRAPH_INPUT_NODE_TYPE,
              "config": { "port": "camera", "family": "image" } },
            { "id": BLUR, "name": "blur", "type": "looks.nodes:Blur", "config": {} },
            { "id": placed, "name": "other", "type": STREAM_FRAGMENT_NODE_TYPE,
              "config": { "stream": { "asset": "00000000-0000-4000-8000-000000000002" } } },
            { "id": look, "name": "look", "type": "looks.generators:look", "config": {},
              "fragment": { "nodes": [{ "id": inner_blur, "name": "blur",
                                        "type": "looks.nodes:Blur", "config": {} }],
                            "links": [] } },
        ],
        "links": [a_link(input, "value", BLUR, "video")],
    }))
    .unwrap();
    let fold = |nodes: &[Uuid], name: &str| {
        document.the_collapse_of_nodes_into_a_placed_stream(
            nodes,
            Uuid::from_u128(0x99),
            name,
            uuid(LOOK_STREAM),
            &every_output_port_is_an_image,
            &mut fresh_uuids(),
        )
    };

    let interface = fold(&[uuid(input), uuid(BLUR)], "sub").unwrap_err();
    assert!(interface.contains("interface"), "{interface}");
    let taken = fold(&[uuid(BLUR)], "look").unwrap_err();
    assert!(taken.contains("`look`"), "{taken}");
    let unreadable = document
        .the_fold_a_fragment_unfolds(uuid(placed), &no_placed_stream_is_read, &mut fresh_uuids())
        .unwrap_err();
    assert!(unreadable.contains("no placed stream"), "{unreadable}");
    let clash = document
        .the_fold_a_fragment_unfolds(uuid(look), &no_placed_stream_is_read, &mut fresh_uuids())
        .unwrap_err();
    assert!(clash.contains("`blur`"), "{clash}");
}

#[test]
fn a_collapse_is_refused_once_a_link_reached_a_folded_node_since() {
    let document = a_chain_of_five_nodes();
    let collapse = the_fold_of(&collapse_blur_and_tint(&document));
    let mut changed = document.clone();
    changed
        .apply(&StreamGraphOperation::Connect {
            link: serde_json::from_value(a_link(SOURCE, "video", TINT, "mask")).unwrap(),
        })
        .unwrap();

    let refused = changed.apply(&collapse).unwrap_err();

    assert!(refused.contains("since"), "{refused}");
}

#[test]
fn an_inline_subgraph_under_a_node_type_the_runtime_defines_is_refused_naming_its_node() {
    let inline = "00000000-0000-4000-8000-0000000000d1";
    let inline_subgraph = |at_the_level_inside_a_generator: bool| {
        let held_inline = json!({ "id": inline, "name": "sub", "type": "tatolab.project:Inline",
                                  "config": {}, "fragment": { "nodes": [], "links": [] } });
        let nodes = if at_the_level_inside_a_generator {
            json!([{ "id": BLUR, "name": "look", "type": "looks.generators:look", "config": {},
                     "fragment": { "nodes": [held_inline], "links": [] } }])
        } else {
            json!([held_inline])
        };
        StreamGraphDocument::from_value(json!({ "nodes": nodes, "links": [] }))
    };

    for at_the_level_inside_a_generator in [false, true] {
        let refused = inline_subgraph(at_the_level_inside_a_generator).unwrap_err();
        assert!(
            refused.contains(inline) && refused.contains("`sub`"),
            "{refused}"
        );
    }
}

#[test]
fn an_unlocked_placed_stream_holding_a_generators_output_copies_every_node_and_group_inside_it_anew()
 {
    let placing = "00000000-0000-4000-8000-0000000000b0";
    let subgraph_input = "00000000-0000-4000-8000-0000000000b1";
    let held_in_the_subgraph = "00000000-0000-4000-8000-0000000000b2";
    let subgraph_output = "00000000-0000-4000-8000-0000000000b3";
    let group_in_the_subgraph = "00000000-0000-4000-8000-0000000000b4";
    let placed_input = "00000000-0000-4000-8000-0000000000b5";
    let subgraph = "00000000-0000-4000-8000-0000000000b6";
    let placed_output = "00000000-0000-4000-8000-0000000000b7";
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [
            { "id": SOURCE, "name": "source", "type": "looks.nodes:Source", "config": {} },
            { "id": placing, "name": "look", "type": STREAM_FRAGMENT_NODE_TYPE,
              "config": { "stream": { "asset": LOOK_STREAM } } },
            { "id": SINK, "name": "sink", "type": "looks.nodes:Sink", "config": {} },
        ],
        "links": [
            a_link(SOURCE, "video", placing, "video"),
            a_link(placing, "graded", SINK, "video"),
        ],
    }))
    .unwrap();
    let placed_stream_holding_a_subgraph = StreamGraphDocument::from_value(json!({
        "asset": LOOK_STREAM,
        "nodes": [
            { "id": placed_input, "name": "video", "type": GRAPH_INPUT_NODE_TYPE,
              "config": { "port": "video", "family": "image" } },
            { "id": subgraph, "name": "grade", "type": "looks.generators:grade", "config": {},
              "fragment": {
                  "nodes": [
                      { "id": subgraph_input, "name": "video", "type": GRAPH_INPUT_NODE_TYPE,
                        "config": { "port": "video", "family": "image" } },
                      { "id": held_in_the_subgraph, "name": "tint", "type": "looks.nodes:Tint",
                        "config": {}, "studio": { "group": group_in_the_subgraph } },
                      { "id": subgraph_output, "name": "graded", "type": GRAPH_OUTPUT_NODE_TYPE,
                        "config": { "port": "graded", "family": "image" } },
                  ],
                  "groups": [{ "id": group_in_the_subgraph, "title": "tints" }],
                  "links": [
                      a_link(subgraph_input, "value", held_in_the_subgraph, "video"),
                      a_link(held_in_the_subgraph, "video", subgraph_output, "value"),
                  ],
              } },
            { "id": placed_output, "name": "graded", "type": GRAPH_OUTPUT_NODE_TYPE,
              "config": { "port": "graded", "family": "image" } },
        ],
        "links": [
            a_link(placed_input, "value", subgraph, "video"),
            a_link(subgraph, "graded", placed_output, "value"),
        ],
    }))
    .unwrap();
    let unlock = StreamGraphOperation::UnfoldFragment {
        fold: document
            .the_fold_a_fragment_unfolds(
                uuid(placing),
                &|_: &Value| Ok(placed_stream_holding_a_subgraph.clone()),
                &mut fresh_uuids(),
            )
            .unwrap(),
    };

    let unlocked = applied(&document, &unlock).to_value();

    let copied_subgraph = unlocked["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "grade")
        .expect("the subgraph is copied into place")["fragment"]
        .clone();
    let inner_uuids: Vec<&str> = copied_subgraph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    for held_uuid in [subgraph_input, held_in_the_subgraph, subgraph_output] {
        assert!(
            !inner_uuids.contains(&held_uuid),
            "{held_uuid} comes back under a fresh UUID: {inner_uuids:?}"
        );
    }
    let copied_tint = &copied_subgraph["nodes"][1];
    let copied_group = copied_subgraph["groups"][0]["id"].as_str().unwrap();
    assert_ne!(copied_group, group_in_the_subgraph);
    assert_eq!(copied_tint["studio"]["group"], copied_group);
    assert_eq!(
        copied_subgraph["links"],
        json!([
            a_link(inner_uuids[0], "value", inner_uuids[1], "video"),
            a_link(inner_uuids[1], "video", inner_uuids[2], "value"),
        ])
    );
}
