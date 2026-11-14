// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::Graph;

const MACHINE_NAME: &str = "desk";

/// No package extends the engine, so nothing a package registered renders.
#[test]
fn a_graph_renders_no_extensions_key_and_its_schema_declares_none() {
    let rendered =
        serde_json::to_value(Graph::new().to_graph_response(MACHINE_NAME.to_string())).unwrap();

    let keys: Vec<&String> = rendered.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["nodes", "links", "machine"]);

    let schema = serde_json::to_value(schemars::schema_for!(GraphResponse)).unwrap();
    let mut declared: Vec<&String> = schema["properties"]
        .as_object()
        .expect("the GraphResponse schema declares its properties")
        .keys()
        .collect();
    declared.sort();
    assert_eq!(
        declared,
        [
            "asset",
            "clock",
            "display_name",
            "groups",
            "links",
            "machine",
            "nodes",
            "references",
            "stream",
            "studio"
        ]
    );
}

/// An address begins with the machine, so `graph` carries the machine's
/// name as a top-level key, verbatim, and it reads back.
#[test]
fn the_machine_is_a_top_level_key_and_no_mesh_key_renders() {
    let rendered =
        serde_json::to_value(Graph::new().to_graph_response(MACHINE_NAME.to_string())).unwrap();

    assert_eq!(rendered["machine"], MACHINE_NAME);
    assert!(
        rendered.get("mesh").is_none(),
        "no mesh key renders: {rendered}"
    );

    let read_back: GraphResponse =
        serde_json::from_value(rendered).expect("a rendered graph deserializes");
    assert_eq!(read_back.machine, MACHINE_NAME);
}

/// The graph of every stream carries the machine at its top level beside
/// the streams, and nothing else.
#[test]
fn the_machine_wide_graph_is_the_machine_and_its_streams() {
    let machine_wide = MachineWideGraphResponse {
        machine: MACHINE_NAME.to_string(),
        streams: vec![Graph::new().to_graph_response(MACHINE_NAME.to_string())],
    };
    let rendered = serde_json::to_value(&machine_wide).unwrap();
    let keys: Vec<&String> = rendered.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["machine", "streams"]);
    assert_eq!(rendered["machine"], MACHINE_NAME);
}
