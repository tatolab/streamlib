// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Reading and writing a graph document: its keys, node UUIDs, link ends, and the node types and settings it is validated against.

use super::*;

#[test]
fn a_graph_document_parses_with_its_live_keys_ignored() {
    let graph_document = serde_json::json!({
        "stream": "main",
        "nodes": [
            {"id": "abc", "name": "camera", "type": A_CAMERA_CLASS, "config": {"fps": 30},
             "config_checksum": 7, "ports": {"inputs": [], "outputs": []}, "components": {}},
            {"id": "def", "name": "display", "type": "my_app.nodes:DisplayNode", "config": {}}
        ],
        "links": [
            {"id": "l1", "state": "wired", "capacity": 4,
             "source": {"node": "camera", "port": "video"},
             "target": {"node": "display", "port": "video"}}
        ],
        "extensions": [],
        "machine": "desk"
    });

    let graph = GraphSnapshot::from_graph_document(graph_document).unwrap();

    assert_eq!(graph.stream.as_deref(), Some("main"));
    assert_eq!(graph.nodes[0].name, "camera");
    assert_eq!(graph.nodes[0].node_type.as_str(), A_CAMERA_CLASS);
    assert_eq!(graph.nodes[0].config, serde_json::json!({"fps": 30}));
    assert_eq!(
        graph.links[0].source,
        LinkPortRefOutput {
            node: "camera".into(),
            port: "video".into()
        }
    );
}

#[test]
fn a_graph_serializes_exactly_the_spec_keys() {
    let graph = GraphSnapshot {
        asset: None,
        display_name: None,
        stream: None,
        clock: StreamClockSpec::Real,
        nodes: vec![GraphSnapshotNode {
            id: None,
            name: "camera".into(),
            node_type: NodeClassImportPath::new(A_CAMERA_CLASS).unwrap(),
            config: serde_json::json!({}),
            pins: serde_json::Map::new(),
            exposed_config_fields: Vec::new(),
            studio: serde_json::Map::new(),
            fragment: None,
        }],
        links: vec![],
        groups: vec![],
        studio: None,
    };

    let serialized = serde_json::to_value(&graph).unwrap();

    assert_eq!(
        serialized,
        serde_json::json!({
            "nodes": [{"name": "camera", "type": A_CAMERA_CLASS, "config": {}}],
            "links": []
        })
    );
    assert_eq!(
        GraphSnapshot::from_json_str(&graph.to_json_string().unwrap()).unwrap(),
        graph
    );
}

fn a_graph_document_of_a_camera_feeding_a_display() -> (Uuid, Uuid, Uuid, String) {
    let asset = Uuid::new_v4();
    let camera = Uuid::new_v4();
    let display = Uuid::new_v4();
    let document = serde_json::json!({
        "asset": asset.to_string(),
        "nodes": [
            {"id": camera.to_string(), "name": "camera", "type": A_CAMERA_CLASS,
             "config": {}, "pins": {"fps": 30}, "exposed_config_fields": ["exposure"],
             "studio": {"position": [10, 20]}},
            {"id": display.to_string(), "name": "display", "type": A_CAMERA_CLASS,
             "config": {}},
        ],
        "links": [{"source": {"node": camera.to_string(), "port": "video"},
                   "target": {"node": display.to_string(), "port": "video"}}],
    });
    (asset, camera, display, document.to_string())
}

#[test]
fn a_graph_document_names_link_ends_by_node_uuid_and_reads_back_by_name() {
    let (asset, camera, display, document) = a_graph_document_of_a_camera_feeding_a_display();

    let graph = GraphSnapshot::from_json_str(&document).unwrap();

    assert_eq!(graph.asset, Some(asset));
    assert_eq!(graph.nodes[0].id, Some(camera));
    assert_eq!(graph.nodes[0].pins["fps"], serde_json::json!(30));
    assert_eq!(
        graph.nodes[0].exposed_config_fields,
        vec!["exposure".to_string()]
    );
    assert_eq!(
        graph.nodes[0].studio["position"],
        serde_json::json!([10, 20])
    );
    assert_eq!(graph.links[0].source.node, "camera");
    assert_eq!(graph.links[0].target.node, "display");

    let written: serde_json::Value =
        serde_json::from_slice(&graph.graph_document_bytes().unwrap()).unwrap();
    assert_eq!(written["links"][0]["source"]["node"], camera.to_string());
    assert_eq!(written["links"][0]["target"]["node"], display.to_string());
    assert_eq!(GraphSnapshot::from_graph_document(written).unwrap(), graph);
}

#[test]
fn a_graph_document_is_written_the_same_bytes_every_time() {
    let (_, _, _, document) = a_graph_document_of_a_camera_feeding_a_display();
    let graph = GraphSnapshot::from_json_str(&document).unwrap();
    let bytes = graph.graph_document_bytes().unwrap();

    assert!(bytes.ends_with(b"}\n"));
    assert_eq!(
        GraphSnapshot::from_json_str(std::str::from_utf8(&bytes).unwrap())
            .unwrap()
            .graph_document_bytes()
            .unwrap(),
        bytes
    );
}

#[test]
fn two_nodes_of_one_uuid_are_refused() {
    let (_, camera, display, document) = a_graph_document_of_a_camera_feeding_a_display();
    let document = document.replace(&display.to_string(), &camera.to_string());

    let refusal = GraphSnapshot::from_json_str(&document)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("both have the UUID"), "got {refusal}");
}

#[test]
fn a_graph_with_no_asset_or_a_node_with_no_uuid_writes_no_document() {
    let (asset, camera, _, document) = a_graph_document_of_a_camera_feeding_a_display();
    let no_asset = document.replace(&format!("\"asset\":\"{asset}\","), "");
    assert!(
        GraphSnapshot::from_json_str(&no_asset)
            .unwrap()
            .graph_document_bytes()
            .is_err()
    );
    let no_uuid = document.replace(&format!("\"id\":\"{camera}\","), "");
    let refusal = GraphSnapshot::from_json_str(&no_uuid)
        .unwrap()
        .graph_document_bytes()
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("`camera` has no UUID"), "got {refusal}");
}

#[test]
fn a_node_id_that_is_no_uuid_reads_as_no_uuid() {
    let graph = GraphSnapshot::from_json_str(
        &serde_json::json!({"nodes": [{"id": "P1abc", "name": "camera",
                                       "type": A_CAMERA_CLASS, "config": {}}],
                            "links": []})
        .to_string(),
    )
    .unwrap();
    assert_eq!(graph.nodes[0].id, None);
}

#[test]
fn the_retired_alias_shape_does_not_parse() {
    let aliases = r#"{"nodes": [{"alias": "camera", "type": "a:B", "config": {}}],
                          "connections": [{"from": "camera.video", "to": "display.video"}]}"#;

    assert!(GraphSnapshot::from_json_str(aliases).is_err());
}

#[test]
fn two_nodes_whose_names_cast_alike_are_refused_naming_the_name() {
    let camera_class = a_registered_camera_class();
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [
            {"name": "FrontCam", "type": camera_class},
            {"name": "frontcam", "type": camera_class}
        ]
    }))
    .unwrap();

    match graph.validate(&NodeTypesOneStreamResolves::new()) {
        Err(Error::NodeNameTaken { name, cast }) => {
            assert_eq!(name, "frontcam");
            assert_eq!(cast, "frontcam");
        }
        other => panic!("expected NodeNameTaken, got {other:?}"),
    }
}

#[test]
fn a_link_naming_a_node_the_graph_does_not_hold_is_refused() {
    let camera_class = a_registered_camera_class();
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "camera", "type": camera_class}],
        "links": [{"source": {"node": "camera", "port": "video"},
                   "target": {"node": "display", "port": "video"}}]
    }))
    .unwrap();

    let refusal = graph
        .validate(&NodeTypesOneStreamResolves::new())
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("`display`"), "{refusal}");
    assert!(refusal.contains("camera"), "{refusal}");
}

#[test]
fn a_link_end_naming_a_port_its_node_does_not_have_in_that_direction_is_refused() {
    let camera_class = a_registered_camera_class();
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "front", "type": camera_class}, {"name": "back", "type": camera_class}],
        "links": [{"source": {"node": "front", "port": "frames_in"},
                   "target": {"node": "back", "port": "frames_in"}}]
    }))
    .unwrap();

    let refusal = graph
        .validate(&NodeTypesOneStreamResolves::new())
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("no output port `frames_in`"), "{refusal}");
    assert!(refusal.contains("video"), "{refusal}");
}

#[test]
fn an_unregistered_type_is_refused_as_unknown() {
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "ghost", "type": "my_app.nodes:NotARegisteredNode"}]
    }))
    .unwrap();

    match graph.validate(&NodeTypesOneStreamResolves::new()) {
        Err(Error::UnknownNodeType { ident }) => {
            assert_eq!(ident.as_str(), "my_app.nodes:NotARegisteredNode");
        }
        other => panic!("expected UnknownNodeType, got {other:?}"),
    }
}

/// A stream names no runtime version, so the refusal names this runtime's
/// own beside the type it lacks.
#[test]
fn a_built_in_type_this_runtime_lacks_is_refused_naming_it_and_this_runtimes_version() {
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "newthing", "type": "tatolab.stream:NewThing"}]
    }))
    .unwrap();

    let refusal = graph
        .validate(&NodeTypesOneStreamResolves::new())
        .unwrap_err()
        .to_string();

    assert_eq!(
        refusal,
        format!(
            "this runtime ({}) has no node type `tatolab.stream:NewThing`",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn a_built_in_that_moved_to_a_module_is_refused_naming_the_module_to_install() {
    let moved_out = NodeClassImportPath::of_built_in_node("GraphSnapshotTestMovedOut");
    NODE_REGISTRY.register_built_in_node_type_moved_to_a_module(
        moved_out.clone(),
        NodeClassImportPath::new("tatolab.snapshottest:GraphSnapshotTestMovedOut").unwrap(),
        "tatolab-snapshottest",
    );
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "moved", "type": moved_out.as_str()}]
    }))
    .unwrap();

    let refusal = graph
        .validate(&NodeTypesOneStreamResolves::new())
        .unwrap_err();

    assert!(
        matches!(&refusal, Error::BuiltInNodeTypeMovedToAModule { node_type, .. } if *node_type == moved_out),
        "{refusal:?}"
    );
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("`tatolab.snapshottest:GraphSnapshotTestMovedOut`"),
        "{refusal}"
    );
    assert!(
        refusal.contains("pip install tatolab-snapshottest"),
        "{refusal}"
    );
}

#[test]
fn a_setting_the_nodes_type_does_not_take_is_refused_naming_the_node_its_type_and_the_setting() {
    crate::core::test_support::ensure_test_mocks_registered();
    let source_type =
        crate::core::test_support::MockSourceTakingOneSetting::node_class_import_path();
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "Front", "type": source_type.as_str(),
                   "config": {"frame_widht": 640}}]
    }))
    .unwrap();

    match graph.validate(&NodeTypesOneStreamResolves::new()) {
        Err(Error::NodeConfigRefused {
            node_name,
            node_type,
            refusal,
        }) => {
            assert_eq!(node_name, "Front");
            assert_eq!(node_type, source_type);
            assert!(refusal.contains("`frame_widht`"), "{refusal}");
            assert!(refusal.contains("`frame_width`"), "{refusal}");
        }
        other => panic!("expected NodeConfigRefused, got {other:?}"),
    }
}

#[test]
fn a_setting_the_nodes_type_takes_passes() {
    crate::core::test_support::ensure_test_mocks_registered();
    let source_type =
        crate::core::test_support::MockSourceTakingOneSetting::node_class_import_path();
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "Front", "type": source_type.as_str(),
                   "config": {"frame_width": 640}},
                  {"name": "Back", "type": source_type.as_str()}]
    }))
    .unwrap();

    graph.validate(&NodeTypesOneStreamResolves::new()).unwrap();
}

#[test]
fn a_node_written_without_a_config_takes_the_config_naming_no_setting() {
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "camera", "type": A_CAMERA_CLASS}]
    }))
    .unwrap();

    assert_eq!(graph.nodes[0].config, serde_json::json!({}));
}

/// A misspelled key is read past by serde and the graph loads as something
/// its author did not write, so every key that is neither read nor one of
/// `graph`'s live keys is refused, at every level, naming where it sits.
#[test]
fn a_key_this_runtime_does_not_read_is_refused_naming_it_and_where_it_sits() {
    let a_node = serde_json::json!({"name": "camera", "type": A_CAMERA_CLASS});
    let a_link = serde_json::json!({"source": {"node": "camera", "port": "video"},
                                    "target": {"node": "display", "port": "video"}});
    let with = |mut object: serde_json::Value, key: &str| {
        object[key] = serde_json::json!(1);
        object
    };
    for (graph_document, refusal_names) in [
        (
            serde_json::json!({"nodes": [], "linkz": []}),
            [
                "the graph carries the key `linkz`",
                "`asset`, `display_name`, `stream`, `clock`, `nodes`, `links`, `groups` and `studio`",
            ],
        ),
        (
            serde_json::json!({"nodes": [with(a_node.clone(), "confg")]}),
            [
                "node `camera` carries the key `confg`",
                "`id`, `name`, `type`, `config`, `pins`, `exposed_config_fields`, `studio` and `fragment`",
            ],
        ),
        (
            serde_json::json!({"nodes": [{"name": "camera", "typ": A_CAMERA_CLASS}]}),
            [
                "node `camera` carries the key `typ`",
                "`id`, `name`, `type`, `config`, `pins`, `exposed_config_fields`, `studio` and `fragment`",
            ],
        ),
        (
            serde_json::json!({"nodes": [a_node.clone()], "links": [with(a_link.clone(), "sourec")]}),
            [
                "the link `camera/video` → `display/video` carries the key `sourec`",
                "`source` and `target`",
            ],
        ),
        (
            serde_json::json!({"nodes": [a_node.clone()], "links": [{
                "source": {"node": "camera", "port": "video", "prot": "x"},
                "target": {"node": "display", "port": "video"}}]}),
            [
                "the source end of the link `camera/video` → `display/video` carries the key `prot`",
                "`node` and `port`",
            ],
        ),
    ] {
        let refusal = GraphSnapshot::from_graph_document(graph_document)
            .expect_err("a key the runtime does not read is refused")
            .to_string();
        for named in refusal_names {
            assert!(refusal.contains(named), "{named:?} not in: {refusal}");
        }
    }
}

#[test]
fn a_key_this_runtime_does_not_read_is_refused_from_a_json_string_too() {
    let refusal = GraphSnapshot::from_json_str(r#"{"nodes": [], "nodez": []}"#)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("`nodez`"), "{refusal}");
}

/// `graph` renders spec and live keys together and a load reads it back, so
/// every key the render can carry has to be one a load reads or skips, and
/// every spec key one the render carries.
#[test]
fn every_key_graph_renders_is_one_a_load_reads_or_skips() {
    let render_schema = serde_json::to_value(schemars::schema_for!(
        crate::core::json_schema::GraphResponse
    ))
    .unwrap();
    let definitions = render_schema
        .get("definitions")
        .or_else(|| render_schema.get("$defs"))
        .expect("the render's schema defines its nested objects");
    let keys_rendered_on = |object_schema: &serde_json::Value| -> HashSet<String> {
        object_schema["properties"]
            .as_object()
            .expect("an object schema lists its properties")
            .keys()
            .cloned()
            .collect()
    };

    for (object_schema, keys) in [
        (&render_schema, &GRAPH_DOCUMENT_KEYS),
        (&definitions["GraphNodeOutput"], &GRAPH_NODE_KEYS),
        (&definitions["LinkOutput"], &GRAPH_LINK_KEYS),
        (&definitions["LinkPortRefOutput"], &GRAPH_LINK_END_KEYS),
    ] {
        let rendered = keys_rendered_on(object_schema);
        for key in &rendered {
            assert!(
                keys.spec_keys.contains(&key.as_str()) || keys.live_keys.contains(&key.as_str()),
                "`graph` renders `{key}` on {}, which a load neither reads nor skips",
                keys.object_kind
            );
        }
        for spec_key in keys.spec_keys {
            assert!(
                rendered.contains(*spec_key),
                "a load reads `{spec_key}` on {}, which `graph` does not render",
                keys.object_kind
            );
        }
    }
}

/// A type described in the stream validates against the stream's own
/// ports; a stream that described nothing refuses it as unknown.
#[test]
fn a_type_described_in_the_stream_validates_against_that_streams_ports() {
    let described_class = "graph_snapshot_tests.nodes:DescribedInTheStream";
    let node_types = NodeTypesOneStreamResolves::new();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            crate::core::descriptors::NodeDescriptor::new(
                crate::core::descriptors::NodeClassShortName::new("DescribedInTheStream").unwrap(),
                NodeClassImportPath::new(described_class).unwrap(),
                "graph snapshot test",
            )
            .with_output(crate::core::descriptors::PortDescriptor::new(
                "video",
                "",
                false,
                crate::core::descriptors::PortFamilyDeclaration::of_family(
                    crate::core::descriptors::PortFamily::Image,
                ),
            )),
            crate::core::nodes::HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                Box::new(|_node| Err(Error::NotSupported("never constructed".into()))),
            ),
        )
        .expect("the type is described into the stream");
    let graph = GraphSnapshot::from_graph_document(serde_json::json!({
        "nodes": [{"name": "described", "type": described_class}]
    }))
    .unwrap();

    graph
        .validate(&node_types)
        .expect("the stream resolves the type it described");
    assert!(
        matches!(
            graph.validate(&NodeTypesOneStreamResolves::new()),
            Err(Error::UnknownNodeType { .. })
        ),
        "a stream that described nothing resolved another stream's type"
    );
}
