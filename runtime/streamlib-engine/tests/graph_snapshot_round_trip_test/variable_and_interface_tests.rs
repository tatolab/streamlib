// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's variables and interface: rendered as drawn, loaded back, and
//! refused when a family does not match.

use super::*;

/// Register a descriptor-only type whose ports declare `family`.
fn register_a_family_type(
    short: &str,
    inputs: &[(&str, streamlib::sdk::descriptors::PortFamily)],
    outputs: &[(&str, streamlib::sdk::descriptors::PortFamily)],
) -> NodeClassImportPath {
    use streamlib::sdk::descriptors::PortFamilyDeclaration;
    let import_path = NodeClassImportPath::new(format!("{}::{short}", module_path!())).unwrap();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short).unwrap(),
        import_path.clone(),
        "graph variable test",
    );
    for (name, family) in inputs {
        descriptor = descriptor.with_input(PortDescriptor::new(
            *name,
            "",
            false,
            PortFamilyDeclaration::of_family(*family),
        ));
    }
    for (name, family) in outputs {
        descriptor = descriptor.with_output(PortDescriptor::new(
            *name,
            "",
            false,
            PortFamilyDeclaration::of_family(*family),
        ));
    }
    let _ = NODE_REGISTRY.register_descriptor_only(descriptor);
    import_path
}

fn an_image_variable_read_by_two_readers(
    camera: &NodeClassImportPath,
    second_reader: &NodeClassImportPath,
) -> serde_json::Value {
    serde_json::json!({
        "stream": "image-variable",
        "nodes": [
            {"name": "camera", "type": camera.as_str()},
            {"name": "set-camera", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "camera"}},
            {"name": "get-camera", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "camera"}},
            {"name": "get-camera-2", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "camera"}},
            {"name": "front", "type": camera.as_str().replace("VariableCamera", "VariableDisplay")},
            {"name": "back", "type": second_reader.as_str()}
        ],
        "links": [
            {"source": {"node": "camera", "port": "video"}, "target": {"node": "set-camera", "port": "value"}},
            {"source": {"node": "get-camera", "port": "value"}, "target": {"node": "front", "port": "video_in"}},
            {"source": {"node": "get-camera-2", "port": "value"}, "target": {"node": "back", "port": "video_in"}}
        ]
    })
}

#[test]
#[serial]
fn an_image_variable_read_twice_runs_as_two_links_and_renders_as_drawn() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("VariableCamera", &[], &[("video", PortFamily::Image)]);
    let display =
        register_a_family_type("VariableDisplay", &[("video_in", PortFamily::Image)], &[]);
    let graph = an_image_variable_read_by_two_readers(&camera, &display);

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph.clone())
        .expect("an Image variable read twice loads");
    let rendered = the_graph_document_of(&first);
    let link_ids_by_drawn_source: Vec<(&str, &str)> = rendered["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| {
            (
                link["source"]["node"].as_str().unwrap(),
                link["id"].as_str().unwrap(),
            )
        })
        .collect();
    let ids_of_links_out_of_a_get: Vec<&str> = link_ids_by_drawn_source
        .iter()
        .filter(|(source, _)| source.starts_with("get-camera"))
        .map(|(_, id)| *id)
        .collect();
    assert_eq!(ids_of_links_out_of_a_get.len(), 2);
    assert!(
        ids_of_links_out_of_a_get.iter().all(|id| !id.is_empty())
            && ids_of_links_out_of_a_get[0] != ids_of_links_out_of_a_get[1],
        "each get's drawn link carries the id of the link the runtime runs: {link_ids_by_drawn_source:?}"
    );
    for node in rendered["nodes"].as_array().unwrap() {
        assert!(
            uuid::Uuid::parse_str(node["id"].as_str().unwrap()).is_ok(),
            "every node renders its UUID, a variable node too: {node}"
        );
    }

    assert_eq!(
        the_spec_without_its_node_uuids_in(rendered.clone()),
        the_spec_in(graph),
        "graph renders the variables as drawn"
    );
    let second_runner = Runner::new().unwrap();
    let second = load(&second_runner, project_directory.path(), rendered.clone())
        .expect("a rendered graph holding variables loads back");
    assert_eq!(
        the_spec_in(the_graph_document_of(&second)),
        the_spec_in(rendered)
    );
}

#[test]
#[serial]
fn a_get_read_as_the_wrong_family_is_refused_at_load_naming_the_get_and_its_variable() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("VariableCamera", &[], &[("video", PortFamily::Image)]);
    register_a_family_type("VariableDisplay", &[("video_in", PortFamily::Image)], &[]);
    let records_reader = register_a_family_type(
        "VariableRecordsReader",
        &[("video_in", PortFamily::Records)],
        &[],
    );

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let refusal = the_refusal_of(
        load(
            &runner,
            project_directory.path(),
            an_image_variable_read_by_two_readers(&camera, &records_reader),
        ),
        "a get read as Records from an Image variable must be refused",
    )
    .to_string();
    assert!(
        refusal.contains("`GetVariable` node `get-camera-2` reads the variable `camera`"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_variable_holding_a_gateway_reference_renders_a_reference_edge_and_loads_back() {
    let gateway = register_test_type("VariableGateway", "_unused_in", "_unused_out");
    let track_import_path =
        NodeClassImportPath::new(format!("{}::VariableTrack", module_path!())).unwrap();
    let _ = NODE_REGISTRY.register_descriptor_only(
        NodeDescriptor::new(
            NodeClassShortName::new("VariableTrack").unwrap(),
            track_import_path.clone(),
            "graph variable test",
        )
        .with_config_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "gateway": {streamlib::sdk::descriptors::NODE_REFERENCE_SCHEMA_KEYWORD: {}}
            }
        })),
    );
    let graph = serde_json::json!({
        "stream": "gateway-variable",
        "nodes": [
            {"name": "cloudflare", "type": gateway.as_str()},
            {"name": "set-connection", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "connection", "node_reference": {"node": "cloudflare"}}},
            {"name": "get-connection", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "connection"}},
            {"name": "camera-track", "type": track_import_path.as_str(),
             "input_values": {"gateway": {"node": "get-connection"}}}
        ]
    });

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph.clone())
        .expect("a gateway held in a variable loads");
    let rendered = the_graph_document_of(&first);
    assert_eq!(
        rendered["references"],
        serde_json::json!([
            {"node": "camera-track", "config_key": "gateway", "referenced_node": "cloudflare"}
        ])
    );
    assert_eq!(
        the_spec_without_its_node_uuids_in(rendered.clone()),
        the_spec_in(graph)
    );

    let second_runner = Runner::new().unwrap();
    load(&second_runner, project_directory.path(), rendered)
        .expect("a rendered gateway variable loads back");
}

#[test]
#[serial]
fn a_variable_read_by_a_node_named_as_typed_renders_its_link_once() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("VariableCamera", &[], &[("video", PortFamily::Image)]);
    let display =
        register_a_family_type("VariableDisplay", &[("video_in", PortFamily::Image)], &[]);
    let mut graph = an_image_variable_read_by_two_readers(&camera, &display);
    graph["nodes"][5]["name"] = serde_json::json!("Back Display");
    graph["links"][2]["target"]["node"] = serde_json::json!("Back Display");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(&runner, project_directory.path(), graph).expect("the stream loads");
    let links_into = |rendered: &serde_json::Value, node: &str| -> Vec<serde_json::Value> {
        rendered["links"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|link| link["target"]["node"] == node)
            .cloned()
            .collect()
    };
    let rendered = the_graph_document_of(&stream);
    let into_the_back_display = links_into(&rendered, "back-display");
    assert_eq!(into_the_back_display.len(), 1, "{into_the_back_display:?}");
    assert_eq!(into_the_back_display[0]["source"]["node"], "get-camera-2");
    assert!(!into_the_back_display[0]["id"].as_str().unwrap().is_empty());
}

#[test]
#[serial]
fn a_streams_interface_renders_as_drawn_loads_back_and_a_family_mismatch_is_refused() {
    use streamlib::sdk::descriptors::PortFamily;
    let camera = register_a_family_type("InterfaceCamera", &[], &[("video", PortFamily::Image)]);
    let display =
        register_a_family_type("InterfaceDisplay", &[("video_in", PortFamily::Image)], &[]);
    let graph_offering = |output_family: &str| {
        serde_json::json!({
            "stream": "interface",
            "nodes": [
                {"name": "camera", "type": camera.as_str()},
                {"name": "camera-out", "type": "tatolab.stream:GraphOutput",
                 "input_values": {"port": "Camera", "family": output_family}},
                {"name": "overlay-in", "type": "tatolab.stream:GraphInput",
                 "input_values": {"port": "overlay", "family": "image"}},
                {"name": "display", "type": display.as_str()}
            ],
            "links": [
                {"source": {"node": "camera", "port": "video"},
                 "target": {"node": "camera-out", "port": "value"}},
                {"source": {"node": "overlay-in", "port": "value"},
                 "target": {"node": "display", "port": "video_in"}}
            ]
        })
    };
    let graph = graph_offering("image");

    let project_directory = tempfile::tempdir().expect("a project directory");
    let first_runner = Runner::new().unwrap();
    let first = load(&first_runner, project_directory.path(), graph.clone())
        .expect("a stream declaring an Output and an Input loads");
    let rendered = the_graph_document_of(&first);
    let camera_out = rendered["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "camera-out")
        .expect("the GraphOutput renders");
    assert_eq!(
        camera_out["components"]["graph_interface"],
        serde_json::json!({
            "direction": "output", "port": "camera", "family": "image",
            "pin_type": {"type": "texture"},
        }),
        "a GraphOutput renders the type its link carries"
    );
    assert_eq!(
        the_spec_without_its_node_uuids_in(rendered.clone()),
        the_spec_in(graph),
        "graph renders the interface as drawn"
    );
    let camera_id = first
        .the_node_named("camera")
        .expect("the camera node is in the graph")
        .node_run_id;
    let removal_refusal = first
        .remove_node(&camera_id)
        .expect_err("removing the node the GraphOutput's link names is refused")
        .to_string();
    assert!(
        removal_refusal.contains("node `camera` cannot be removed"),
        "{removal_refusal}"
    );
    let name_refusal = first
        .add_node(
            NodeSpec::new(display.clone(), serde_json::json!({})).with_display_name("Overlay In"),
        )
        .expect_err("a node added live under an interface node's name is refused");
    assert!(
        matches!(&name_refusal, Error::NodeNameTaken { cast, .. } if cast == "overlay-in"),
        "{name_refusal:?}"
    );
    assert_eq!(
        the_spec_in(the_graph_document_of(&first)),
        the_spec_in(rendered.clone()),
        "the refused changes leave the graph as drawn"
    );

    let second_runner = Runner::new().unwrap();
    let second = load(&second_runner, project_directory.path(), rendered.clone())
        .expect("a rendered graph holding interface nodes loads back");
    assert_eq!(
        the_spec_in(the_graph_document_of(&second)),
        the_spec_in(rendered)
    );

    let refusal_runner = Runner::new().unwrap();
    let refusal = the_refusal_of(
        load(
            &refusal_runner,
            tempfile::tempdir().expect("a project directory").path(),
            graph_offering("packets"),
        ),
        "an Output declared as another family than its port writes is refused",
    )
    .to_string();
    assert!(
        refusal.contains("`GraphOutput` node `camera-out` declares `Camera` as Packets")
            && refusal.contains("camera.video (Image)"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_live_config_update_under_which_a_declared_output_no_longer_matches_its_port_is_refused() {
    use streamlib::sdk::descriptors::{PortFamily, PortFamilyDeclaration};
    let import_path =
        NodeClassImportPath::new(format!("{}::InterfaceTrackReceive", module_path!())).unwrap();
    let _ = NODE_REGISTRY.register_descriptor_only(
        NodeDescriptor::new(
            NodeClassShortName::new("InterfaceTrackReceive").unwrap(),
            import_path.clone(),
            "graph interface test",
        )
        .with_output(PortDescriptor::new(
            "content",
            "",
            false,
            PortFamilyDeclaration {
                family_from_config_key: Some("family".into()),
                ..PortFamilyDeclaration::of_family(PortFamily::Packets)
            },
        )),
    );
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().unwrap();
    let stream = load(
        &runner,
        project_directory.path(),
        serde_json::json!({
            "stream": "interface-live-update",
            "nodes": [
                {"name": "receive", "type": import_path.as_str(),
                 "input_values": {"family": "packets"}},
                {"name": "received", "type": "tatolab.stream:GraphOutput",
                 "input_values": {"port": "received", "family": "packets"}}
            ],
            "links": [
                {"source": {"node": "receive", "port": "content"},
                 "target": {"node": "received", "port": "value"}}
            ]
        }),
    )
    .expect("an Output fed by a port whose family its config names loads");
    let receive_id = stream
        .the_node_named("receive")
        .expect("the receive node is in the graph")
        .node_run_id;

    stream
        .update_node_config(&receive_id, serde_json::json!({"family": "packets"}))
        .expect("an update keeping the family is taken");
    let refusal = stream
        .update_node_config(&receive_id, serde_json::json!({"family": "records"}))
        .expect_err("an update changing the family under the declared Output is refused")
        .to_string();
    assert!(
        refusal.contains("`GraphOutput` node `received` declares `received` as Packets")
            && refusal.contains("receive.content (Records)"),
        "{refusal}"
    );
}
