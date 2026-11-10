// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Value, json};
use streamlib_project_repository::{
    AUTHORED_FRAGMENT_NODE_TYPE, GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE,
    STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentLinkEnd,
    StreamGraphOperation,
};
use uuid::Uuid;

const SOURCE: &str = "00000000-0000-4000-8000-00000000000a";
const BLUR: &str = "00000000-0000-4000-8000-00000000000b";
const TINT: &str = "00000000-0000-4000-8000-00000000000c";
const SINK: &str = "00000000-0000-4000-8000-00000000000d";
const PREVIEW: &str = "00000000-0000-4000-8000-00000000000e";

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
    StreamGraphDocument::from_value(json!({
        "asset": "00000000-0000-4000-8000-000000000001",
        "display_name": "main",
        "nodes": [
            node(SOURCE, "source"), node(BLUR, "blur"), node(TINT, "tint"),
            node(SINK, "sink"), node(PREVIEW, "preview"),
        ],
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

fn collapse_blur_and_tint(document: &StreamGraphDocument) -> StreamGraphOperation {
    StreamGraphOperation::FoldIntoFragment {
        fold: document
            .the_fold_of_nodes_into_an_authored_fragment(
                &[uuid(BLUR), uuid(TINT)],
                Uuid::from_u128(0x99),
                "look",
                &every_output_port_is_an_image,
                &mut fresh_uuids(),
            )
            .unwrap(),
    }
}

#[test]
fn a_collapsed_selection_is_one_authored_fragment_whose_ports_carry_the_crossing_links() {
    let document = a_chain_of_five_nodes();

    let collapsed = applied(&document, &collapse_blur_and_tint(&document));

    let names: Vec<String> = collapsed
        .nodes()
        .into_iter()
        .map(|node| node.name)
        .collect();
    assert_eq!(names, ["source", "look", "sink", "preview"]);
    let look = collapsed.node(Uuid::from_u128(0x99)).unwrap();
    assert_eq!(look.node_type, AUTHORED_FRAGMENT_NODE_TYPE);
    assert_eq!(
        links_of(&collapsed),
        [
            "look/tint-video -> preview/video",
            "look/tint-video -> sink/video",
            "source/video -> look/blur-video",
        ]
    );
    let stored = look.fragment.unwrap();
    let inner: Vec<(String, String, Value)> = stored["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            (
                node["name"].as_str().unwrap().to_owned(),
                node["type"].as_str().unwrap().to_owned(),
                node["config"].clone(),
            )
        })
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
    assert_eq!(stored["links"].as_array().unwrap().len(), 3, "{stored}");
}

#[test]
fn a_collapse_and_its_undo_give_back_the_exact_earlier_document() {
    let document = a_chain_of_five_nodes();
    let collapse = collapse_blur_and_tint(&document);
    let collapsed = applied(&document, &collapse);

    let undone = applied(&collapsed, &collapse.inverse());

    assert_eq!(undone.to_value(), document.to_value());
    assert_eq!(applied(&undone, &collapse).to_value(), collapsed.to_value());
}

#[test]
fn an_unlocked_fragment_puts_its_nodes_in_place_and_its_undo_gives_back_the_fragment() {
    let document = a_chain_of_five_nodes();
    let collapsed = applied(&document, &collapse_blur_and_tint(&document));
    let unlock = StreamGraphOperation::UnfoldFragment {
        fold: collapsed
            .the_fold_a_fragment_unfolds(Uuid::from_u128(0x99))
            .unwrap(),
    };

    let unlocked = applied(&collapsed, &unlock);

    let names: Vec<String> = unlocked.nodes().into_iter().map(|node| node.name).collect();
    assert_eq!(names, ["source", "blur", "tint", "sink", "preview"]);
    assert_eq!(links_of(&unlocked), links_of(&document));
    assert!(unlocked.fragment_nodes().is_empty());
    assert_eq!(
        applied(&unlocked, &unlock.inverse()).to_value(),
        collapsed.to_value()
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
        fold: document.the_fold_a_fragment_unfolds(uuid(look)).unwrap(),
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
fn a_stream_interface_node_a_placed_stream_and_a_name_clash_are_refused() {
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
        document.the_fold_of_nodes_into_an_authored_fragment(
            nodes,
            Uuid::from_u128(0x99),
            name,
            &every_output_port_is_an_image,
            &mut fresh_uuids(),
        )
    };

    let interface = fold(&[uuid(input), uuid(BLUR)], "sub").unwrap_err();
    assert!(interface.contains("interface"), "{interface}");
    let taken = fold(&[uuid(BLUR)], "look").unwrap_err();
    assert!(taken.contains("`look`"), "{taken}");
    let placed_stream = document
        .the_fold_a_fragment_unfolds(uuid(placed))
        .unwrap_err();
    assert!(placed_stream.contains("not unlocked"), "{placed_stream}");
    let clash = document
        .the_fold_a_fragment_unfolds(uuid(look))
        .unwrap_err();
    assert!(clash.contains("`blur`"), "{clash}");
}

#[test]
fn a_collapse_is_refused_once_a_link_reached_a_folded_node_since() {
    let document = a_chain_of_five_nodes();
    let collapse = collapse_blur_and_tint(&document);
    let mut changed = document.clone();
    changed
        .apply(&StreamGraphOperation::Connect {
            link: serde_json::from_value(a_link(SOURCE, "video", TINT, "mask")).unwrap(),
        })
        .unwrap();

    let refused = changed.apply(&collapse).unwrap_err();

    assert!(refused.contains("since"), "{refused}");
}
