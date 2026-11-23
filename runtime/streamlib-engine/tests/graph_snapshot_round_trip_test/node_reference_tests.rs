// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A config key naming another node: refused when the graph does not hold it,
//! rendered as a reference edge, and loaded back in any order.

use super::*;

#[test]
#[serial]
fn a_live_add_or_config_update_naming_a_node_the_graph_does_not_hold_is_refused_and_a_reference_renders_cast()
 {
    let (gateway, track_import_path) = register_a_gateway_and_a_track_naming_one();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "live-references");
    stream
        .add_node(
            NodeSpec::new(gateway, serde_json::json!({})).with_display_name("Cloudflare Gateway"),
        )
        .expect("the gateway adds");
    let track = stream
        .add_node(
            NodeSpec::new(
                track_import_path.clone(),
                serde_json::json!({"gateway": {"node": "Cloudflare Gateway"}}),
            )
            .with_display_name("camera-track"),
        )
        .expect("the track adds");

    assert_eq!(
        the_graph_document_of(&stream)["references"],
        serde_json::json!([
            {"node": "camera-track", "config_key": "gateway", "referenced_node": "cloudflare-gateway"}
        ])
    );
    let refusal = stream
        .update_node_config(&track, serde_json::json!({"gateway": {"node": "nowhere"}}))
        .expect_err("a reference to a node the graph does not hold is refused")
        .to_string();
    assert!(
        refusal.contains("names node `nowhere`") && refusal.contains("cloudflare-gateway"),
        "{refusal}"
    );
    let refusal = stream
        .add_node(
            NodeSpec::new(
                track_import_path,
                serde_json::json!({"gateway": {"node": "nowhere"}}),
            )
            .with_display_name("screen-track"),
        )
        .expect_err("a live add naming a node the graph does not hold is refused")
        .to_string();
    assert!(refusal.contains("names node `nowhere`"), "{refusal}");
    let names_in_the_graph: Vec<serde_json::Value> = the_graph_document_of(&stream)["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].clone())
        .collect();
    assert!(
        !names_in_the_graph.contains(&serde_json::json!("screen-track")),
        "the refused node is not left in the graph: {names_in_the_graph:?}"
    );
}

#[test]
#[serial]
fn a_load_listing_a_referrer_before_the_node_it_names_loads() {
    let (gateway, track_import_path) = register_a_gateway_and_a_track_naming_one();
    let graph = serde_json::json!({
        "stream": "referrer-listed-first",
        "nodes": [
            {
                "name": "camera-track",
                "type": track_import_path.as_str(),
                "input_values": {"gateway": {"node": "cloudflare"}}
            },
            {"name": "cloudflare", "type": gateway.as_str()}
        ]
    });

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(&runner, project_directory.path(), graph)
        .expect("a referrer listed before the node it names loads");
    assert_eq!(
        the_graph_document_of(&stream)["references"],
        serde_json::json!([
            {"node": "camera-track", "config_key": "gateway", "referenced_node": "cloudflare"}
        ])
    );
}

#[test]
#[serial]
fn a_reference_edge_in_a_config_renders_beside_the_links_and_loads_back() {
    let (gateway, track_import_path) = register_a_gateway_and_a_track_naming_one();
    let graph = serde_json::json!({
        "stream": "reference-edges",
        "nodes": [
            {"name": "cloudflare", "type": gateway.as_str()},
            {
                "name": "camera-track",
                "type": track_import_path.as_str(),
                "input_values": {"gateway": {"node": "cloudflare"}}
            }
        ]
    });

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph).expect("the stream loads");
    let rendered = the_graph_document_of(&first);
    assert_eq!(
        rendered["references"],
        serde_json::json!([
            {"node": "camera-track", "config_key": "gateway", "referenced_node": "cloudflare"}
        ])
    );
    assert!(
        rendered["links"].as_array().unwrap().is_empty(),
        "a reference edge is never a link"
    );

    let second_runner = Runner::new().unwrap();
    let second = load(&second_runner, project_directory.path(), rendered.clone())
        .expect("a rendered reference edge loads back");
    assert_eq!(
        the_graph_document_of(&second)["references"],
        rendered["references"]
    );
}
