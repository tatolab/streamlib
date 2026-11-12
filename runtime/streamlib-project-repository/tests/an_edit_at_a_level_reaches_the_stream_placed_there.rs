// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE, STREAM_FRAGMENT_NODE_TYPE, StreamGraphDocument,
    StreamGraphDocumentLink, StreamGraphDocumentLinkEnd, StreamGraphDocumentNode,
    StreamGraphLevelOfAPlacedStream, StreamGraphOperation,
};
use uuid::Uuid;

const MAIN_STREAM: &str = "00000000-0000-4000-8000-000000000001";
const LOOK_STREAM: &str = "00000000-0000-4000-8000-000000000002";
const GRAIN_STREAM: &str = "00000000-0000-4000-8000-000000000003";
const SOURCE: &str = "00000000-0000-4000-8000-00000000000a";
const LOOK: &str = "00000000-0000-4000-8000-00000000000b";
const SINK: &str = "00000000-0000-4000-8000-00000000000c";
const LOOK_AGAIN: &str = "00000000-0000-4000-8000-00000000000d";
const PREVIEW: &str = "00000000-0000-4000-8000-00000000000e";
const BLUR: &str = "00000000-0000-4000-8000-0000000000b1";
const TINT: &str = "00000000-0000-4000-8000-0000000000b2";
const LOOK_INPUT: &str = "00000000-0000-4000-8000-0000000000b3";
const LOOK_OUTPUT: &str = "00000000-0000-4000-8000-0000000000b4";
const GRAIN_PLACED_IN_LOOK: &str = "00000000-0000-4000-8000-0000000000b5";
const NOISE: &str = "00000000-0000-4000-8000-0000000000c1";

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

fn a_link(source: &str, source_port: &str, target: &str, target_port: &str) -> Value {
    json!({ "source": { "node": source, "port": source_port },
            "target": { "node": target, "port": target_port } })
}

fn a_node(id: &str, name: &str) -> Value {
    json!({ "id": id, "name": name, "type": format!("looks.nodes:{name}"),
            "config": { "strength": 1 } })
}

fn a_placement_of(id: &str, name: &str, stream: &str) -> Value {
    json!({ "id": id, "name": name, "type": STREAM_FRAGMENT_NODE_TYPE,
            "config": { "stream": { "asset": stream } } })
}

/// `source -> look -> sink` and `source -> look-again -> preview`, both placing `look`.
fn main_placing_look_twice() -> StreamGraphDocument {
    StreamGraphDocument::from_value(json!({
        "asset": MAIN_STREAM,
        "nodes": [
            a_node(SOURCE, "source"),
            a_placement_of(LOOK, "look", LOOK_STREAM),
            a_node(SINK, "sink"),
            a_placement_of(LOOK_AGAIN, "look-again", LOOK_STREAM),
            a_node(PREVIEW, "preview"),
        ],
        "links": [
            a_link(SOURCE, "video", LOOK, "video"),
            a_link(LOOK, "graded", SINK, "video"),
            a_link(SOURCE, "video", LOOK_AGAIN, "video"),
            a_link(LOOK_AGAIN, "graded", PREVIEW, "video"),
        ],
    }))
    .unwrap()
}

/// `video -> blur -> tint -> graded`, with `grain` placed beside `tint`.
fn look() -> StreamGraphDocument {
    StreamGraphDocument::from_value(json!({
        "asset": LOOK_STREAM,
        "nodes": [
            { "id": LOOK_INPUT, "name": "video", "type": GRAPH_INPUT_NODE_TYPE,
              "config": { "port": "video", "family": "image" } },
            a_node(BLUR, "blur"),
            a_node(TINT, "tint"),
            a_placement_of(GRAIN_PLACED_IN_LOOK, "grain", GRAIN_STREAM),
            { "id": LOOK_OUTPUT, "name": "graded", "type": GRAPH_OUTPUT_NODE_TYPE,
              "config": { "port": "graded", "family": "image" } },
        ],
        "links": [
            a_link(LOOK_INPUT, "value", BLUR, "video"),
            a_link(BLUR, "video", TINT, "video"),
            a_link(TINT, "video", LOOK_OUTPUT, "value"),
        ],
    }))
    .unwrap()
}

fn grain() -> StreamGraphDocument {
    StreamGraphDocument::from_value(json!({
        "asset": GRAIN_STREAM,
        "nodes": [a_node(NOISE, "noise")],
        "links": [],
    }))
    .unwrap()
}

fn the_project_stream(stream: Uuid) -> Result<StreamGraphDocument, String> {
    match stream.to_string().as_str() {
        LOOK_STREAM => Ok(look()),
        GRAIN_STREAM => Ok(grain()),
        _ => Err(format!("the project holds no stream {stream}")),
    }
}

fn the_level(path: &[Uuid]) -> Result<StreamGraphLevelOfAPlacedStream, String> {
    main_placing_look_twice().the_level_a_path_of_placed_streams_reaches(
        uuid(MAIN_STREAM),
        path,
        &mut the_project_stream,
    )
}

fn drawn_inside(placing_node: &str, own: &str) -> Uuid {
    Uuid::new_v5(&uuid(placing_node), uuid(own).as_bytes())
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

/// The edit `operation_as_drawn` makes at `level`, applied to the placed stream's own document.
fn the_placed_stream_edited(
    level: &StreamGraphLevelOfAPlacedStream,
    operation_as_drawn: StreamGraphOperation,
) -> (StreamGraphOperation, StreamGraphDocument) {
    let operation = level
        .the_operation_on_the_document(operation_as_drawn)
        .unwrap();
    let edited = applied_with_its_undo_checked(&level.document, &operation);
    (operation, edited)
}

#[test]
fn a_level_ending_at_a_placed_stream_is_that_streams_document_drawn_under_the_placements_uuids() {
    let level = the_level(&[uuid(LOOK)]).unwrap();

    assert_eq!(level.stream, uuid(LOOK_STREAM));
    assert!(!level.is_the_streams_own_level());
    assert_eq!(level.document.to_value(), look().to_value());
    let drawn = level.the_document_as_drawn().unwrap();
    assert_eq!(drawn.node(drawn_inside(LOOK, BLUR)).unwrap().name, "blur");
    assert!(drawn.node(uuid(BLUR)).is_err());
    assert!(drawn.links().contains(&a_document_link(
        drawn_inside(LOOK, BLUR),
        "video",
        drawn_inside(LOOK, TINT),
        "video"
    )));
    let own_level = the_level(&[]).unwrap();
    assert!(own_level.is_the_streams_own_level());
    assert_eq!(own_level.stream, uuid(MAIN_STREAM));
}

#[test]
fn a_node_added_wired_and_configured_at_a_placed_level_lands_in_the_placed_stream_and_undoes() {
    let level = the_level(&[uuid(LOOK)]).unwrap();
    let speck = Uuid::from_u128(0x200);

    let (add, added) = the_placed_stream_edited(
        &level,
        StreamGraphOperation::AddNode {
            node: a_plain_node(speck, "speck"),
            links: vec![a_document_link(
                drawn_inside(LOOK, TINT),
                "video",
                speck,
                "video",
            )],
        },
    );
    assert_eq!(added.node(speck).unwrap().name, "speck");
    assert!(
        added
            .links()
            .contains(&a_document_link(uuid(TINT), "video", speck, "video")),
        "the link names `tint` by its own UUID in the placed stream"
    );
    assert_eq!(
        level.the_operation_as_drawn(&add, &added).unwrap(),
        StreamGraphOperation::AddNode {
            node: a_plain_node(drawn_inside(LOOK, &speck.to_string()), "speck"),
            links: vec![a_document_link(
                drawn_inside(LOOK, TINT),
                "video",
                drawn_inside(LOOK, &speck.to_string()),
                "video",
            )],
        },
        "the answer names the added node as the placing stream draws it"
    );

    let (_, configured) = the_placed_stream_edited(
        &level,
        StreamGraphOperation::SetNodeConfig {
            node: drawn_inside(LOOK, BLUR),
            from: json!({ "strength": 1 }),
            to: json!({ "strength": 5 }),
        },
    );
    assert_eq!(
        configured.node(uuid(BLUR)).unwrap().config,
        json!({ "strength": 5 })
    );
}

#[test]
fn a_config_naming_a_node_uuid_at_a_placed_level_is_kept_as_it_was_sent() {
    let level = the_level(&[uuid(LOOK)]).unwrap();
    let drawn_tint = drawn_inside(LOOK, TINT).to_string();

    let (configure, configured) = the_placed_stream_edited(
        &level,
        StreamGraphOperation::SetNodeConfig {
            node: drawn_inside(LOOK, BLUR),
            from: json!({ "strength": 1 }),
            to: json!({ "strength": 1, "follows": drawn_tint }),
        },
    );
    assert_eq!(
        configured.node(uuid(BLUR)).unwrap().config["follows"],
        json!(drawn_tint)
    );
    assert_eq!(
        level
            .the_operation_as_drawn(&configure, &configured)
            .unwrap(),
        StreamGraphOperation::SetNodeConfig {
            node: drawn_inside(LOOK, BLUR),
            from: json!({ "strength": 1 }),
            to: json!({ "strength": 1, "follows": drawn_tint }),
        }
    );

    let speck = Uuid::from_u128(0x201);
    let mut speck_naming_tint = a_plain_node(speck, "speck");
    speck_naming_tint.config = json!({ "follows": TINT });
    let add = StreamGraphOperation::AddNode {
        node: speck_naming_tint,
        links: Vec::new(),
    };
    let mut added = level.document.clone();
    added.apply(&add).unwrap();
    let StreamGraphOperation::AddNode { node, .. } =
        level.the_operation_as_drawn(&add, &added).unwrap()
    else {
        panic!("an add is drawn as an add");
    };
    assert_eq!(node.id, drawn_inside(LOOK, &speck.to_string()));
    assert_eq!(node.config, json!({ "follows": TINT }));
}

#[test]
fn studio_metadata_naming_a_node_uuid_at_a_placed_level_is_kept_as_it_was_sent() {
    let level = the_level(&[uuid(LOOK)]).unwrap();
    let drawn_tint = drawn_inside(LOOK, TINT).to_string();
    let speck = Uuid::from_u128(0x201);

    let mut speck_naming_drawn_tint = a_plain_node(speck, "speck");
    speck_naming_drawn_tint
        .studio
        .metadata
        .insert("id".to_owned(), json!(drawn_tint));
    let add = level
        .the_operation_on_the_document(StreamGraphOperation::AddNode {
            node: speck_naming_drawn_tint,
            links: Vec::new(),
        })
        .unwrap();
    let StreamGraphOperation::AddNode { node, .. } = &add else {
        panic!("an add is made as an add");
    };
    assert_eq!(node.studio.metadata["id"], json!(drawn_tint));

    let mut speck_naming_tint = a_plain_node(speck, "speck");
    speck_naming_tint
        .studio
        .metadata
        .insert("id".to_owned(), json!(TINT));
    let add = StreamGraphOperation::AddNode {
        node: speck_naming_tint,
        links: Vec::new(),
    };
    let mut added = level.document.clone();
    added.apply(&add).unwrap();
    let StreamGraphOperation::AddNode { node, .. } =
        level.the_operation_as_drawn(&add, &added).unwrap()
    else {
        panic!("an add is drawn as an add");
    };
    assert_eq!(node.studio.metadata["id"], json!(TINT));
}

#[test]
fn a_name_at_a_placed_level_clashes_only_with_a_name_of_the_placed_stream() {
    let level = the_level(&[uuid(LOOK)]).unwrap();
    let mut placed = level.document.clone();

    let named_as_mains_sink = level
        .the_operation_on_the_document(StreamGraphOperation::AddNode {
            node: a_plain_node(Uuid::from_u128(0x201), "sink"),
            links: Vec::new(),
        })
        .unwrap();
    applied_with_its_undo_checked(&placed, &named_as_mains_sink);

    let named_as_blur = level
        .the_operation_on_the_document(StreamGraphOperation::AddNode {
            node: a_plain_node(Uuid::from_u128(0x202), "blur"),
            links: Vec::new(),
        })
        .unwrap();
    let refused = placed.apply(&named_as_blur).unwrap_err();
    assert!(refused.contains("`blur`"), "{refused}");
    assert_eq!(placed.to_value(), look().to_value());
}

#[test]
fn removing_a_placed_streams_output_takes_its_links_out_in_every_placement_and_undoes() {
    let level = the_level(&[uuid(LOOK)]).unwrap();
    let main = main_placing_look_twice();
    let graph_output = level
        .the_document_as_drawn()
        .unwrap()
        .node(drawn_inside(LOOK, LOOK_OUTPUT))
        .unwrap();
    let (_, removed) = the_placed_stream_edited(
        &level,
        StreamGraphOperation::RemoveNode {
            links: vec![a_document_link(
                drawn_inside(LOOK, TINT),
                "video",
                graph_output.id,
                "value",
            )],
            node: graph_output,
        },
    );

    assert_eq!(
        main.each_node_placing_the_stream(uuid(LOOK_STREAM)),
        [uuid(LOOK), uuid(LOOK_AGAIN)]
    );
    let carry = main
        .the_links_carried_onto_the_ports_of_a_placed_stream(
            uuid(LOOK_STREAM),
            &level.document,
            &removed,
        )
        .unwrap()
        .unwrap();
    let carried_in_main = applied_with_its_undo_checked(&main, &carry);

    assert_eq!(
        carried_in_main.links(),
        [
            a_document_link(uuid(SOURCE), "video", uuid(LOOK), "video"),
            a_document_link(uuid(SOURCE), "video", uuid(LOOK_AGAIN), "video"),
        ],
        "only the links into the port still declared are left"
    );
}

#[test]
fn renaming_a_placed_streams_port_carries_the_links_in_every_placement_to_the_new_name() {
    let level = the_level(&[uuid(LOOK)]).unwrap();
    let main = main_placing_look_twice();
    let (_, renamed) = the_placed_stream_edited(
        &level,
        StreamGraphOperation::SetNodeConfig {
            node: drawn_inside(LOOK, LOOK_OUTPUT),
            from: json!({ "port": "graded", "family": "image" }),
            to: json!({ "port": "toned", "family": "image" }),
        },
    );

    let carry = main
        .the_links_carried_onto_the_ports_of_a_placed_stream(
            uuid(LOOK_STREAM),
            &level.document,
            &renamed,
        )
        .unwrap()
        .unwrap();
    let carried_in_main = applied_with_its_undo_checked(&main, &carry);

    assert_eq!(
        carried_in_main.links(),
        [
            a_document_link(uuid(SOURCE), "video", uuid(LOOK), "video"),
            a_document_link(uuid(LOOK), "toned", uuid(SINK), "video"),
            a_document_link(uuid(SOURCE), "video", uuid(LOOK_AGAIN), "video"),
            a_document_link(uuid(LOOK_AGAIN), "toned", uuid(PREVIEW), "video"),
        ]
    );
    let config_only = level
        .the_operation_on_the_document(StreamGraphOperation::SetNodeConfig {
            node: drawn_inside(LOOK, BLUR),
            from: json!({ "strength": 1 }),
            to: json!({ "strength": 4 }),
        })
        .unwrap();
    let mut blur_changed = level.document.clone();
    blur_changed.apply(&config_only).unwrap();
    assert_eq!(
        main.the_links_carried_onto_the_ports_of_a_placed_stream(
            uuid(LOOK_STREAM),
            &level.document,
            &blur_changed
        )
        .unwrap(),
        None,
        "an edit that keeps every port carries no link"
    );
}

#[test]
fn a_stream_placed_inside_a_placed_stream_is_reached_through_the_path_of_both() {
    let grain_drawn_in_main = drawn_inside(LOOK, GRAIN_PLACED_IN_LOOK);

    let level = the_level(&[uuid(LOOK), grain_drawn_in_main]).unwrap();

    assert_eq!(level.stream, uuid(GRAIN_STREAM));
    let noise_drawn_in_main = Uuid::new_v5(
        &uuid(LOOK),
        Uuid::new_v5(&uuid(GRAIN_PLACED_IN_LOOK), uuid(NOISE).as_bytes()).as_bytes(),
    );
    assert_eq!(level.the_drawn_uuid_of(uuid(NOISE)), noise_drawn_in_main);
    let (_, configured) = the_placed_stream_edited(
        &level,
        StreamGraphOperation::SetNodeConfig {
            node: noise_drawn_in_main,
            from: json!({ "strength": 1 }),
            to: json!({ "strength": 9 }),
        },
    );
    assert_eq!(
        configured.node(uuid(NOISE)).unwrap().config,
        json!({ "strength": 9 })
    );
}

#[test]
fn a_path_through_an_import_a_generators_output_or_a_plain_node_is_refused() {
    let generated = "00000000-0000-4000-8000-000000000400";
    let imported = "00000000-0000-4000-8000-000000000401";
    let main = StreamGraphDocument::from_value(json!({
        "asset": MAIN_STREAM,
        "nodes": [
            { "id": generated, "name": "soft-look", "type": "generators.looks:soft_look",
              "config": {}, "fragment": { "nodes": [
                  { "id": "00000000-0000-4000-8000-000000000402", "name": "blur",
                    "type": "looks.nodes:blur", "config": {} }
              ], "links": [] } },
            { "id": imported, "name": "camera-rig", "type": STREAM_FRAGMENT_NODE_TYPE,
              "config": { "stream": { "asset": "00000000-0000-4000-8000-000000000500",
                                      "from": "00000000-0000-4000-8000-000000000501" } } },
            a_node(SINK, "sink"),
        ],
        "links": [],
    }))
    .unwrap();
    let reached = |node: &str| {
        main.clone()
            .the_level_a_path_of_placed_streams_reaches(
                uuid(MAIN_STREAM),
                &[uuid(node)],
                &mut the_project_stream,
            )
            .unwrap_err()
    };

    let through_the_generator = reached(generated);
    assert!(
        through_the_generator.contains("graph generator `generators.looks:soft_look`")
            && through_the_generator.contains("unlock_subgraph"),
        "{through_the_generator}"
    );
    let through_the_import = reached(imported);
    assert!(
        through_the_import.contains("a stream of the import"),
        "{through_the_import}"
    );
    let through_a_plain_node = reached(SINK);
    assert!(
        through_a_plain_node.contains("is no subgraph"),
        "{through_a_plain_node}"
    );
    let through_a_node_held_nowhere = the_level(&[Uuid::from_u128(0x999)]).unwrap_err();
    assert!(
        through_a_node_held_nowhere.contains(&Uuid::from_u128(0x999).to_string()),
        "{through_a_node_held_nowhere}"
    );
}
