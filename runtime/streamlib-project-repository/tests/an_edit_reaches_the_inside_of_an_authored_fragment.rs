// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    GRAPH_OUTPUT_NODE_TYPE, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphOperation,
};
use uuid::Uuid;

const SOURCE: &str = "00000000-0000-4000-8000-00000000000a";
const BLUR: &str = "00000000-0000-4000-8000-00000000000b";
const TINT: &str = "00000000-0000-4000-8000-00000000000c";
const SINK: &str = "00000000-0000-4000-8000-00000000000d";
const PREVIEW: &str = "00000000-0000-4000-8000-00000000000e";
const LOOK: u128 = 0x99;

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

fn a_link(source: &str, source_port: &str, target: &str, target_port: &str) -> Value {
    json!({ "source": { "node": source, "port": source_port },
            "target": { "node": target, "port": target_port } })
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

/// `source -> look -> sink` and `look -> preview`, `look` the authored fragment a collapse of
/// `blur -> tint` makes.
fn a_stream_with_blur_and_tint_collapsed_into_look() -> StreamGraphDocument {
    let node = |id: &str, name: &str| {
        json!({ "id": id, "name": name, "type": format!("looks.nodes:{name}"),
                "config": { "strength": 1 } })
    };
    let mut document = StreamGraphDocument::from_value(json!({
        "asset": "00000000-0000-4000-8000-000000000001",
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
    .unwrap();
    let collapse = StreamGraphOperation::FoldIntoFragment {
        fold: document
            .the_fold_of_nodes_into_an_authored_fragment(
                &[uuid(BLUR), uuid(TINT)],
                Uuid::from_u128(LOOK),
                "look",
                &every_output_port_is_an_image,
                &mut fresh_uuids(),
            )
            .unwrap(),
    };
    document.apply(&collapse).unwrap();
    document
}

fn a_plain_node(id: Uuid, name: &str) -> StreamGraphDocumentNode {
    StreamGraphDocumentNode {
        id,
        name: name.to_owned(),
        node_type: format!("looks.nodes:{name}"),
        config: json!({ "strength": 2 }),
        pins: Map::new(),
        exposed_config_fields: Vec::new(),
        studio: Default::default(),
        fragment: None,
    }
}

fn a_document_link(
    source: Uuid,
    source_port: &str,
    target: Uuid,
    target_port: &str,
) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: source,
            port: source_port.to_owned(),
        },
        target: StreamGraphDocumentLinkEnd {
            node: target,
            port: target_port.to_owned(),
        },
    }
}

fn applied_with_its_undo_checked(
    document: &StreamGraphDocument,
    operation: &StreamGraphOperation,
) -> StreamGraphDocument {
    let mut applied = document.clone();
    applied.apply(operation).unwrap();
    let mut undone = applied.clone();
    undone.apply(&operation.inverse()).unwrap();
    assert_eq!(
        undone.to_value(),
        document.to_value(),
        "the undo of {operation:?} gives back the exact earlier document"
    );
    applied
}

fn names_inside_look(document: &StreamGraphDocument) -> Vec<String> {
    document
        .the_level_inside(&[Uuid::from_u128(LOOK)])
        .unwrap()
        .nodes()
        .into_iter()
        .map(|node| node.name)
        .collect()
}

#[test]
fn a_node_added_wired_and_configured_inside_a_collapsed_selection_lands_there_and_undoes() {
    let document = a_stream_with_blur_and_tint_collapsed_into_look();
    let look = [Uuid::from_u128(LOOK)];
    let grain = Uuid::from_u128(0x200);

    let add = document
        .the_edit_at_the_level(
            &look,
            StreamGraphOperation::AddNode {
                node: a_plain_node(grain, "grain"),
                links: Vec::new(),
            },
        )
        .unwrap();
    let added = applied_with_its_undo_checked(&document, &add);
    assert!(names_inside_look(&added).contains(&"grain".to_owned()));
    assert!(
        added.node(grain).is_err(),
        "the stream's own level holds no node the edit placed inside the fragment"
    );

    let connect = added
        .the_edit_at_the_level(
            &look,
            StreamGraphOperation::Connect {
                link: a_document_link(uuid(TINT), "video", grain, "video"),
            },
        )
        .unwrap();
    let connected = applied_with_its_undo_checked(&added, &connect);
    assert!(
        connected
            .the_level_inside(&look)
            .unwrap()
            .links()
            .contains(&a_document_link(uuid(TINT), "video", grain, "video"))
    );

    let configure = connected
        .the_edit_at_the_level(
            &look,
            StreamGraphOperation::SetNodeConfig {
                node: uuid(BLUR),
                from: json!({ "strength": 1 }),
                to: json!({ "strength": 5 }),
            },
        )
        .unwrap();
    let configured = applied_with_its_undo_checked(&connected, &configure);
    assert_eq!(
        configured
            .the_level_inside(&look)
            .unwrap()
            .node(uuid(BLUR))
            .unwrap()
            .config,
        json!({ "strength": 5 })
    );
}

#[test]
fn a_name_inside_a_fragment_clashes_only_with_a_name_of_its_own_level() {
    let document = a_stream_with_blur_and_tint_collapsed_into_look();

    let named_as_the_streams_sink = document
        .the_edit_at_the_level(
            &[Uuid::from_u128(LOOK)],
            StreamGraphOperation::AddNode {
                node: a_plain_node(Uuid::from_u128(0x201), "sink"),
                links: Vec::new(),
            },
        )
        .unwrap();
    applied_with_its_undo_checked(&document, &named_as_the_streams_sink);

    let named_as_blur_inside = document.the_edit_at_the_level(
        &[Uuid::from_u128(LOOK)],
        StreamGraphOperation::AddNode {
            node: a_plain_node(Uuid::from_u128(0x202), "blur"),
            links: Vec::new(),
        },
    );
    assert!(
        named_as_blur_inside
            .unwrap_err()
            .contains("`blur`: node 00000000-0000-4000-8000-00000000000b has that name")
    );
}

#[test]
fn removing_a_fragments_output_takes_out_the_links_on_its_port_and_its_undo_puts_them_back() {
    let document = a_stream_with_blur_and_tint_collapsed_into_look();
    let look = [Uuid::from_u128(LOOK)];
    let inside = document.the_level_inside(&look).unwrap();
    let graph_output = inside
        .nodes()
        .into_iter()
        .find(|node| node.node_type == GRAPH_OUTPUT_NODE_TYPE)
        .unwrap();
    let remove = document
        .the_edit_at_the_level(
            &look,
            StreamGraphOperation::RemoveNode {
                links: inside.links_touching(graph_output.id),
                node: graph_output,
            },
        )
        .unwrap();

    let removed = applied_with_its_undo_checked(&document, &remove);

    let links_left: Vec<StreamGraphDocumentLink> = removed.links();
    assert_eq!(
        links_left,
        [a_document_link(
            uuid(SOURCE),
            "video",
            Uuid::from_u128(LOOK),
            "blur-video"
        )],
        "only the link into the port still declared is left"
    );
}

#[test]
fn a_fragment_inside_a_fragment_is_edited_through_the_path_of_both() {
    let document = a_stream_with_blur_and_tint_collapsed_into_look();
    let look = Uuid::from_u128(LOOK);
    let inner = Uuid::from_u128(0x300);
    let inside = document.the_level_inside(&[look]).unwrap();
    let collapse_tint_inside = document
        .the_edit_at_the_level(
            &[look],
            StreamGraphOperation::FoldIntoFragment {
                fold: inside
                    .the_fold_of_nodes_into_an_authored_fragment(
                        &[uuid(TINT)],
                        inner,
                        "tint-look",
                        &every_output_port_is_an_image,
                        &mut fresh_uuids(),
                    )
                    .unwrap(),
            },
        )
        .unwrap();
    let nested = applied_with_its_undo_checked(&document, &collapse_tint_inside);
    assert!(names_inside_look(&nested).contains(&"tint-look".to_owned()));

    let configure_tint = nested
        .the_edit_at_the_level(
            &[look, inner],
            StreamGraphOperation::SetNodeConfig {
                node: uuid(TINT),
                from: json!({ "strength": 1 }),
                to: json!({ "strength": 9 }),
            },
        )
        .unwrap();
    let configured = applied_with_its_undo_checked(&nested, &configure_tint);
    assert_eq!(
        configured
            .the_level_inside(&[look, inner])
            .unwrap()
            .node(uuid(TINT))
            .unwrap()
            .config,
        json!({ "strength": 9 })
    );
}

#[test]
fn an_edit_inside_a_generators_output_or_a_placed_stream_is_refused_and_changes_nothing() {
    let generated = Uuid::from_u128(0x400);
    let placed = Uuid::from_u128(0x401);
    let generated_inner = "00000000-0000-4000-8000-000000000402";
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [
            { "id": generated.to_string(), "name": "soft-look", "type": "generators.looks:soft_look",
              "config": {}, "fragment": { "nodes": [
                  { "id": generated_inner, "name": "blur", "type": "looks.nodes:blur", "config": {} }
              ], "links": [] } },
            { "id": placed.to_string(), "name": "camera-rig", "type": STREAM_FRAGMENT_NODE_TYPE,
              "config": { "stream": { "asset": "00000000-0000-4000-8000-000000000500" } } },
            { "id": SINK, "name": "sink", "type": "looks.nodes:sink", "config": {} },
        ],
        "links": [],
    }))
    .unwrap();
    let configure_inside = |fragment: Uuid| {
        document.the_edit_at_the_level(
            &[fragment],
            StreamGraphOperation::SetNodeConfig {
                node: uuid(generated_inner),
                from: json!({}),
                to: json!({ "strength": 3 }),
            },
        )
    };

    let inside_the_generators_output = configure_inside(generated).unwrap_err();
    assert!(
        inside_the_generators_output.contains("graph generator `generators.looks:soft_look`")
            && inside_the_generators_output.contains("unlock_generator_fragment"),
        "{inside_the_generators_output}"
    );
    let inside_the_placed_stream = configure_inside(placed).unwrap_err();
    assert!(
        inside_the_placed_stream.contains("is edited as its own stream"),
        "{inside_the_placed_stream}"
    );
    let inside_a_plain_node = configure_inside(uuid(SINK)).unwrap_err();
    assert!(
        inside_a_plain_node.contains("is no subgraph"),
        "{inside_a_plain_node}"
    );

    let recorded_by_hand = StreamGraphOperation::EditInsideFragment {
        fragment: generated,
        edit: Box::new(StreamGraphOperation::SetNodeConfig {
            node: uuid(generated_inner),
            from: json!({}),
            to: json!({ "strength": 3 }),
        }),
        port_links_taken_out: Vec::new(),
        port_links_put_back: Vec::new(),
    };
    let mut edited = document.clone();
    assert!(edited.apply(&recorded_by_hand).is_err());
    assert_eq!(edited.to_value(), document.to_value());
}

#[test]
fn renaming_a_fragments_port_inside_carries_the_links_outside_to_the_new_name() {
    let document = a_stream_with_blur_and_tint_collapsed_into_look();
    let look = Uuid::from_u128(LOOK);
    let graph_output = document
        .the_level_inside(&[look])
        .unwrap()
        .nodes()
        .into_iter()
        .find(|node| node.node_type == GRAPH_OUTPUT_NODE_TYPE)
        .unwrap();
    let rename_the_port = document
        .the_edit_at_the_level(
            &[look],
            StreamGraphOperation::SetNodeConfig {
                node: graph_output.id,
                from: graph_output.config.clone(),
                to: json!({ "port": "graded", "family": "image" }),
            },
        )
        .unwrap();

    let renamed = applied_with_its_undo_checked(&document, &rename_the_port);

    assert_eq!(
        renamed.links(),
        [
            a_document_link(uuid(SOURCE), "video", look, "blur-video"),
            a_document_link(look, "graded", uuid(SINK), "video"),
            a_document_link(look, "graded", uuid(PREVIEW), "video"),
        ]
    );
}
