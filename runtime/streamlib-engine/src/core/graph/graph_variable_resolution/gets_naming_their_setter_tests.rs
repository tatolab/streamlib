// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `GetVariable` naming the `SetVariable` it reads by that node's UUID.

use super::tests::{
    a_camera_through, a_camera_variable_read_by_two_displays, a_graph_response_of, a_rendered_node,
    a_shade_set_inside, link, refusal_of, resolved, resolved_once_flattened,
};
use super::*;

const SET_CAMERA_UUID: &str = "6a1f3c2e-8b4d-4f1a-9c7e-2d5b8a9f0e13";

/// [`a_camera_variable_read_by_two_displays`] with each get naming its setter by its UUID, and
/// the setter's variable renamed `lens`.
fn a_renamed_camera_variable_its_gets_name_by_its_setter() -> serde_json::Value {
    let mut graph = a_camera_variable_read_by_two_displays();
    graph["nodes"][1]["id"] = serde_json::json!(SET_CAMERA_UUID);
    graph["nodes"][1]["input_values"]["variable"] = serde_json::json!("lens");
    for get in [2, 3] {
        graph["nodes"][get]["input_values"] =
            serde_json::json!({ GET_VARIABLE_SET_VARIABLE_CONFIG_KEY: SET_CAMERA_UUID });
    }
    graph
}

#[test]
fn a_get_naming_its_setter_by_uuid_reads_it_after_its_variable_is_renamed() {
    let resolution = resolved(a_renamed_camera_variable_its_gets_name_by_its_setter()).unwrap();
    assert_eq!(
        resolution.graph_the_runtime_runs.links,
        [
            link(("camera", "video"), ("front", "video_in")),
            link(("camera", "video"), ("back", "video_in")),
        ]
    );
    let reads = &resolution.variables_as_drawn.what_each_get_reads["get-camera"];
    assert_eq!(reads.variable.as_deref(), Some("lens"));
    assert_eq!(reads.variables_it_may_read, ["lens"]);
    assert!(
        resolution
            .variables_as_drawn
            .links_resolved_from_a_variable
            .iter()
            .all(|resolved| resolved.variable == "lens")
    );
}

#[test]
fn a_get_naming_a_setter_that_is_gone_is_refused_naming_it_and_the_variables_set() {
    let mut graph = a_renamed_camera_variable_its_gets_name_by_its_setter();
    graph["nodes"][3]["input_values"][GET_VARIABLE_SET_VARIABLE_CONFIG_KEY] =
        serde_json::json!("0b7e9d4c-1a2f-4e3b-8c5d-6f7a8b9c0d1e");
    let refusal = refusal_of(graph);
    assert!(
        refusal.contains(
            "`GetVariable` node `get-camera-2` reads the `SetVariable` node \
             `0b7e9d4c-1a2f-4e3b-8c5d-6f7a8b9c0d1e`, which is gone from its level and every \
             enclosing one. It can read: lens"
        ),
        "{refusal}"
    );
}

#[test]
fn a_get_naming_its_setter_by_something_other_than_a_uuid_is_refused() {
    let mut graph = a_renamed_camera_variable_its_gets_name_by_its_setter();
    graph["nodes"][2]["input_values"][GET_VARIABLE_SET_VARIABLE_CONFIG_KEY] =
        serde_json::json!("set-camera");
    let refusal = refusal_of(graph);
    assert!(
        refusal.contains("`GetVariable` node `get-camera`'s `set_variable` holds \"set-camera\""),
        "{refusal}"
    );
}

#[test]
fn a_get_naming_its_setter_renders_the_variable_its_setter_sets_now() {
    let resolution = resolved(a_renamed_camera_variable_its_gets_name_by_its_setter()).unwrap();
    let mut graph_response = a_graph_response_of(vec![
        a_rendered_node("camera"),
        a_rendered_node("front"),
        a_rendered_node("back"),
    ]);
    resolution
        .variables_as_drawn
        .render_into(&mut graph_response);

    let get = graph_response
        .nodes
        .iter()
        .find(|node| node.name == "get-camera")
        .expect("the get renders");
    assert_eq!(
        serde_json::Value::Object(get.input_values.clone()),
        serde_json::json!({ GET_VARIABLE_SET_VARIABLE_CONFIG_KEY: SET_CAMERA_UUID }),
        "the input values `graph` serves are the ones the document holds"
    );
    let variable_read = get.ports.inputs[0]
        .value_read
        .as_ref()
        .and_then(|read| read.value.clone());
    assert_eq!(variable_read, Some(serde_json::json!("lens")));
    assert_eq!(
        get.components
            .graph_variable
            .as_ref()
            .and_then(|variable| variable.variable.as_deref()),
        Some("lens")
    );
}

const SET_SHADE_UUID: &str = "2f8e4a1c-7b3d-4c5e-9a6f-0d1e2f3a4b5c";

/// `fragment` with its `get-shade` naming the setter of `setter_uuid`, and its own `set-shade`,
/// when it holds one, of that UUID.
fn with_its_get_naming(mut fragment: serde_json::Value, setter_uuid: &str) -> serde_json::Value {
    for node in fragment["nodes"].as_array_mut().unwrap() {
        match node["name"].as_str() {
            Some("set-shade") => node["id"] = serde_json::json!(setter_uuid),
            Some("get-shade") => {
                node["input_values"] =
                    serde_json::json!({ GET_VARIABLE_SET_VARIABLE_CONFIG_KEY: setter_uuid });
            }
            _ => {}
        }
    }
    fragment
}

const SET_COOL_SHADE_UUID: &str = "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d";

/// `fragment` with its `get-shade` naming the setter of `read_uuid` and its own `set-shade` of
/// `setter_uuid`.
fn with_its_setter_and_get_naming(
    fragment: serde_json::Value,
    setter_uuid: &str,
    read_uuid: &str,
) -> serde_json::Value {
    let mut fragment = with_its_get_naming(fragment, read_uuid);
    for node in fragment["nodes"].as_array_mut().unwrap() {
        if node["name"] == "set-shade" {
            node["id"] = serde_json::json!(setter_uuid);
        }
    }
    fragment
}

#[test]
fn a_fragment_placed_twice_reads_each_placements_own_setter_by_its_uuid() {
    let resolution = resolved_once_flattened(a_camera_through(
        &[
            (
                "warm",
                with_its_get_naming(a_shade_set_inside(), SET_SHADE_UUID),
            ),
            (
                "cool",
                with_its_get_naming(a_shade_set_inside(), SET_COOL_SHADE_UUID),
            ),
        ],
        serde_json::json!([]),
        serde_json::json!([]),
    ))
    .expect("each placement reads its own setter");
    let links = &resolution.graph_the_runtime_runs.links;
    assert!(links.contains(&link(("warm/palette", "colour"), ("warm/tint", "shade_in"))));
    assert!(links.contains(&link(("cool/palette", "colour"), ("cool/tint", "shade_in"))));
}

/// Mental-revert: finding a setter by its UUID on any level reads the `warm` placement's setter
/// from inside `cool`, a level that never encloses it.
#[test]
fn a_get_naming_a_setter_of_a_level_beside_its_own_is_refused() {
    let refusal = resolved_once_flattened(a_camera_through(
        &[
            (
                "warm",
                with_its_get_naming(a_shade_set_inside(), SET_SHADE_UUID),
            ),
            (
                "cool",
                with_its_setter_and_get_naming(
                    a_shade_set_inside(),
                    SET_COOL_SHADE_UUID,
                    SET_SHADE_UUID,
                ),
            ),
        ],
        serde_json::json!([]),
        serde_json::json!([]),
    ))
    .expect_err("a get reads its own level or an enclosing one")
    .to_string();
    assert!(
        refusal.contains(&format!(
            "`GetVariable` node `cool/get-shade` reads the `SetVariable` node `{SET_SHADE_UUID}`, \
             which is gone from its level and every enclosing one"
        )),
        "{refusal}"
    );
}

#[test]
fn a_get_inside_a_fragment_reads_the_enclosing_setter_its_uuid_names() {
    let resolution = resolved_once_flattened(a_camera_through(
        &[(
            "look",
            with_its_get_naming(
                super::tests::a_fragment_whose_tint_reads_the_variable_shade(
                    serde_json::json!([]),
                    serde_json::json!([]),
                ),
                SET_SHADE_UUID,
            ),
        )],
        serde_json::json!([
            {"name": "palette", "type": "rig:Palette"},
            {"id": SET_SHADE_UUID, "name": "set-shade", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "tone"}},
        ]),
        serde_json::json!([
            {"source": {"node": "palette", "port": "colour"},
             "target": {"node": "set-shade", "port": "value"}},
        ]),
    ))
    .expect("the get reads the stream's setter of its UUID");
    assert!(
        resolution
            .graph_the_runtime_runs
            .links
            .contains(&link(("palette", "colour"), ("look/tint", "shade_in")))
    );
}
