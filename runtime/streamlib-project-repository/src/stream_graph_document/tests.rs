// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[test]
fn an_asset_reference_nested_in_another_is_made_an_import_reference_too() {
    let project = Uuid::new_v4();
    let outer = Uuid::new_v4().to_string();
    let inner = Uuid::new_v4().to_string();
    let mut config = serde_json::json!({ "asset": outer, "lut": { "asset": inner } });

    asset_references_made_references_into_the_import(&mut config, project);

    assert_eq!(
        config,
        serde_json::json!({
            "asset": outer, "from": project.to_string(),
            "lut": { "asset": inner, "from": project.to_string() },
        })
    );
}

use serde_json::json;

use super::*;

fn a_node(name: &str) -> StreamGraphDocumentNode {
    StreamGraphDocumentNode {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        node_type: "nodes.effect:Effect".to_owned(),
        input_values: Map::new(),
        exposed_config_fields: Vec::new(),
        studio: Default::default(),
        fragment: None,
    }
}

fn a_link(
    source: &StreamGraphDocumentNode,
    target: &StreamGraphDocumentNode,
) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: source.id,
            port: "video".to_owned(),
            member: None,
        },
        target: StreamGraphDocumentLinkEnd {
            node: target.id,
            port: "video_in".to_owned(),
            member: None,
        },
    }
}

fn an_empty_document() -> StreamGraphDocument {
    StreamGraphDocument::from_value(json!({ "asset": Uuid::new_v4(), "nodes": [] })).unwrap()
}

#[test]
fn every_operation_and_its_inverse_leave_the_document_as_it_was() {
    let mut document = an_empty_document();
    let source = a_node("source");
    let sink = a_node("sink");
    for node in [&source, &sink] {
        document
            .apply(&StreamGraphOperation::AddNode {
                node: node.clone(),
                links: Vec::new(),
            })
            .unwrap();
    }
    let link = a_link(&source, &sink);
    let operations = [
        StreamGraphOperation::Connect { link: link.clone() },
        StreamGraphOperation::SetNodeConfig {
            node: sink.id,
            from: json!({}),
            to: json!({"gain": 2}),
        },
        StreamGraphOperation::SetPinValue {
            node: sink.id,
            input: "threshold".to_owned(),
            from: None,
            to: Some(json!(0.5)),
        },
        StreamGraphOperation::RenameNode {
            node: sink.id,
            from: "sink".to_owned(),
            to: "meter".to_owned(),
        },
        StreamGraphOperation::SetStudioFields {
            node: sink.id,
            from: Default::default(),
            to: Box::new(
                StreamGraphNodeStudioFields::try_from(
                    json!({"position": [10, 20]}).as_object().unwrap().clone(),
                )
                .unwrap(),
            ),
        },
        StreamGraphOperation::SetExposedConfigFields {
            node: sink.id,
            from: Vec::new(),
            to: vec!["gain".to_owned()],
        },
    ];
    for operation in &operations {
        let before = document.clone();
        document.apply(operation).unwrap();
        assert_ne!(document, before, "{operation:?} changed nothing");
        let after = document.clone();
        document.apply(&operation.inverse()).unwrap();
        assert_eq!(
            document.canonical_bytes().unwrap(),
            before.canonical_bytes().unwrap(),
            "{operation:?}"
        );
        document = after;
    }
    let removal = StreamGraphOperation::RemoveNode {
        node: document.node(sink.id).unwrap(),
        links: document.links_touching(sink.id),
    };
    let before = document.clone();
    document.apply(&removal).unwrap();
    assert!(document.links().is_empty());
    document.apply(&removal.inverse()).unwrap();
    assert_eq!(
        document.node(sink.id).unwrap(),
        before.node(sink.id).unwrap()
    );
    assert_eq!(document.links(), before.links());
}

fn a_look_fragment_run(blur_radius: u32, with_tint: bool) -> Value {
    let mut nodes = vec![
        json!({"name": "graphinput", "type": "tatolab.stream:GraphInput",
               "config": {"port": "video", "family": "packets"}}),
        json!({"name": "soft", "type": "generators.soft:soft", "config": {},
        "fragment": {
            "nodes": [{"name": "blur", "type": "nodes.blur:Blur",
                       "config": {"radius": blur_radius}}],
            "links": [],
        }}),
    ];
    if with_tint {
        nodes.push(json!({"name": "tint", "type": "nodes.tint:Tint", "config": {}}));
    }
    json!({
        "nodes": nodes,
        "links": [
            {"source": {"node": "graphinput", "port": "value"},
             "target": {"node": "soft", "port": "video"}},
        ],
    })
}

fn uuid_of_the_node_at(fragment: &Value, path: &[&str]) -> Uuid {
    let (name, inner_path) = path.split_first().unwrap();
    let node = fragment["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == *name)
        .unwrap();
    if inner_path.is_empty() {
        Uuid::parse_str(node["id"].as_str().unwrap()).unwrap()
    } else {
        uuid_of_the_node_at(&node["fragment"], inner_path)
    }
}

#[test]
fn a_rerun_keeps_the_uuid_of_each_node_whose_name_is_unchanged_at_every_level() {
    let first_run = fragment_with_each_node_uuid_kept_by_its_name(
        &a_look_fragment_run(4, false),
        None,
        &mut Uuid::new_v4,
    )
    .unwrap();
    let second_run = fragment_with_each_node_uuid_kept_by_its_name(
        &a_look_fragment_run(8, true),
        Some(&first_run),
        &mut Uuid::new_v4,
    )
    .unwrap();

    for path in [&["graphinput"][..], &["soft"], &["soft", "blur"]] {
        assert_eq!(
            uuid_of_the_node_at(&second_run, path),
            uuid_of_the_node_at(&first_run, path),
            "{path:?}"
        );
    }
    assert_eq!(
        second_run["nodes"][1]["fragment"]["nodes"][0]["input_values"]["radius"],
        8
    );
    let tint = uuid_of_the_node_at(&second_run, &["tint"]);
    assert!(
        ![&["graphinput"][..], &["soft"], &["soft", "blur"]]
            .iter()
            .any(|path| uuid_of_the_node_at(&first_run, path) == tint)
    );
    assert_eq!(
        second_run["links"][0]["source"]["node"],
        json!(uuid_of_the_node_at(&first_run, &["graphinput"]).to_string())
    );
    assert_eq!(
        second_run["links"][0]["target"]["node"],
        json!(uuid_of_the_node_at(&first_run, &["soft"]).to_string())
    );
}

#[test]
fn a_fragment_refreshed_and_refreshed_back_is_the_document_it_was() {
    let mut document = an_empty_document();
    let first_run = fragment_with_each_node_uuid_kept_by_its_name(
        &a_look_fragment_run(4, false),
        None,
        &mut Uuid::new_v4,
    )
    .unwrap();
    let mut look = a_node("look");
    look.node_type = "generators.look:look".to_owned();
    look.fragment = Some(Box::new(first_run.clone()));
    document
        .apply(&StreamGraphOperation::AddNode {
            node: look.clone(),
            links: Vec::new(),
        })
        .unwrap();
    let refresh = StreamGraphOperation::RefreshFragment {
        node: look.id,
        from: StreamGraphFragmentPlacement {
            generator: look.node_type.clone(),
            input_values: Map::new(),
            fragment: first_run.clone(),
        },
        to: StreamGraphFragmentPlacement {
            generator: look.node_type.clone(),
            input_values: Map::from_iter([("radius".to_owned(), json!(8))]),
            fragment: fragment_with_each_node_uuid_kept_by_its_name(
                &a_look_fragment_run(8, true),
                Some(&first_run),
                &mut Uuid::new_v4,
            )
            .unwrap(),
        },
    };
    let before = document.canonical_bytes().unwrap();

    document.apply(&refresh).unwrap();
    assert_eq!(
        document.node(look.id).unwrap().input_values,
        Map::from_iter([("radius".to_owned(), json!(8))])
    );
    document.apply(&refresh.inverse()).unwrap();

    assert_eq!(document.canonical_bytes().unwrap(), before);
}

#[test]
fn a_refresh_of_a_node_holding_no_fragment_is_refused_naming_it() {
    let mut document = an_empty_document();
    let blur = a_node("blur");
    document
        .apply(&StreamGraphOperation::AddNode {
            node: blur.clone(),
            links: Vec::new(),
        })
        .unwrap();
    let placement = StreamGraphFragmentPlacement {
        generator: "generators.look:look".to_owned(),
        input_values: Map::new(),
        fragment: json!({"nodes": [], "links": []}),
    };

    let refusal = document
        .apply(&StreamGraphOperation::RefreshFragment {
            node: blur.id,
            from: placement.clone(),
            to: placement,
        })
        .unwrap_err();

    assert_eq!(
        refusal,
        format!(
            "node {} `blur` is no fragment, so no graph generator's run refreshes it",
            blur.id
        )
    );
}

#[test]
fn an_operation_naming_what_the_document_does_not_hold_is_refused_naming_it() {
    let mut document = an_empty_document();
    let held = a_node("held");
    document
        .apply(&StreamGraphOperation::AddNode {
            node: held.clone(),
            links: Vec::new(),
        })
        .unwrap();
    let absent = Uuid::new_v4();
    let refusal = document
        .apply(&StreamGraphOperation::RenameNode {
            node: absent,
            from: "a".to_owned(),
            to: "b".to_owned(),
        })
        .unwrap_err();
    assert!(refusal.contains(&absent.to_string()), "{refusal}");
    assert!(refusal.contains(&held.id.to_string()), "{refusal}");
    let refusal = document
        .apply(&StreamGraphOperation::AddNode {
            node: held.clone(),
            links: Vec::new(),
        })
        .unwrap_err();
    assert!(refusal.contains("never reused"), "{refusal}");
    let mut same_name = a_node("held");
    same_name.id = Uuid::new_v4();
    let refusal = document
        .apply(&StreamGraphOperation::AddNode {
            node: same_name,
            links: Vec::new(),
        })
        .unwrap_err();
    assert!(refusal.contains("has that name"), "{refusal}");
}

#[test]
fn a_link_end_naming_its_node_by_name_reads_as_its_uuid() {
    let source = a_node("source");
    let sink = a_node("sink");
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [source, sink],
        "links": [{
            "source": {"node": "source", "port": "video"},
            "target": {"node": sink.id, "port": "video_in"},
        }],
    }))
    .unwrap();
    assert_eq!(document.links(), [a_link(&source, &sink)]);
}

#[test]
fn input_values_reference_their_projects_assets_at_any_depth_and_leave_another_projects_to_it() {
    let image = Uuid::new_v4();
    let lut = Uuid::new_v4();
    let input_values = json!({
        "image": {"asset": image},
        "grading": [{"lut": {"asset": lut}}],
        "font": {"asset": Uuid::new_v4(), "from": Uuid::new_v4()},
        "label": "asset",
    });
    let Value::Object(input_values) = input_values else {
        unreachable!()
    };
    assert_eq!(
        assets_a_nodes_input_values_reference(&input_values),
        [image, lut]
    );
}

#[test]
fn an_operation_records_with_its_operation_tag() {
    let node = a_node("source");
    let recorded = serde_json::to_value(StreamGraphOperation::SetNodeConfig {
        node: node.id,
        from: json!({}),
        to: json!({"gain": 1}),
    })
    .unwrap();
    assert_eq!(recorded["operation"], "set_node_config");
    assert_eq!(
        serde_json::from_value::<StreamGraphOperation>(recorded)
            .unwrap()
            .node(),
        Some(node.id)
    );
}

#[test]
fn a_fragment_refresh_runs_its_own_generator_and_each_nested_ones() {
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [
            { "id": Uuid::new_v4(), "name": "camera", "type": "nodes.camera:Camera" },
            {
                "id": Uuid::new_v4(), "name": "look", "type": "generators.looks:look",
                "fragment": { "nodes": [
                    { "id": Uuid::new_v4(), "name": "blur", "type": "nodes.blur:Blur" },
                    {
                        "id": Uuid::new_v4(), "name": "soft", "type": "generators.soft:soft",
                        "fragment": { "nodes": [], "links": [] },
                    },
                ], "links": [] },
            },
        ],
        "links": [],
    }))
    .unwrap();

    let fragment_nodes = document.fragment_nodes();

    assert_eq!(fragment_nodes.len(), 1);
    assert_eq!(
        fragment_nodes[0].graph_generators_a_refresh_runs(),
        ["generators.looks:look", "generators.soft:soft"]
    );
}

fn a_group_titled(title: &str) -> StreamGraphDocumentGroup {
    StreamGraphDocumentGroup {
        id: Uuid::new_v4(),
        fields: StreamGraphGroupFields {
            title: title.to_owned(),
            description: "what these nodes do".to_owned(),
            colour: "#3b82f6".to_owned(),
        },
    }
}

fn a_document_holding(nodes: &[&StreamGraphDocumentNode]) -> StreamGraphDocument {
    let mut document = an_empty_document();
    for node in nodes {
        document
            .apply(&StreamGraphOperation::AddNode {
                node: (*node).clone(),
                links: Vec::new(),
            })
            .unwrap();
    }
    document
}

#[test]
fn a_group_operation_and_its_inverse_leave_the_document_as_it_was() {
    let source = a_node("source");
    let sink = a_node("sink");
    let mut document = a_document_holding(&[&source, &sink]);
    document
        .apply(&StreamGraphOperation::SetStudioFields {
            node: sink.id,
            from: Default::default(),
            to: Box::new(
                StreamGraphNodeStudioFields::try_from(
                    json!({"position": [10, 20]}).as_object().unwrap().clone(),
                )
                .unwrap(),
            ),
        })
        .unwrap();
    let sources = a_group_titled("sources");
    let operations = [
        StreamGraphOperation::AddGroup {
            group: sources.clone(),
            nodes: vec![source.id, sink.id],
        },
        StreamGraphOperation::SetGroupFields {
            group: sources.id,
            from: sources.fields.clone(),
            to: StreamGraphGroupFields {
                title: "every source".to_owned(),
                description: String::new(),
                colour: "#22c55e".to_owned(),
            },
        },
    ];
    for operation in &operations {
        let before = document.clone();
        document.apply(operation).unwrap();
        assert_ne!(document, before, "{operation:?} changed nothing");
        let after = document.clone();
        document.apply(&operation.inverse()).unwrap();
        assert_eq!(
            document.canonical_bytes().unwrap(),
            before.canonical_bytes().unwrap(),
            "{operation:?}"
        );
        document = after;
    }

    let removal = StreamGraphOperation::RemoveGroup {
        group: document.group(sources.id).unwrap(),
        nodes: document.nodes_of_the_group(sources.id),
    };
    let before = document.clone();
    document.apply(&removal).unwrap();
    assert!(document.groups().unwrap().is_empty());
    assert!(document.nodes_of_the_group(sources.id).is_empty());
    assert_eq!(
        *document.node(sink.id).unwrap().studio,
        StreamGraphNodeStudioFields::try_from(
            json!({"position": [10, 20]}).as_object().unwrap().clone()
        )
        .unwrap()
    );
    assert!(document.node(source.id).unwrap().studio.is_empty());
    document.apply(&removal.inverse()).unwrap();
    assert_eq!(
        document.canonical_bytes().unwrap(),
        before.canonical_bytes().unwrap()
    );
    assert_eq!(
        document.nodes_of_the_group(sources.id),
        vec![source.id, sink.id]
    );
}

#[test]
fn a_node_of_one_group_is_refused_by_another_naming_the_group_it_belongs_to() {
    let source = a_node("source");
    let mut document = a_document_holding(&[&source]);
    let sources = a_group_titled("sources");
    document
        .apply(&StreamGraphOperation::AddGroup {
            group: sources.clone(),
            nodes: vec![source.id],
        })
        .unwrap();
    let before = document.clone();

    let refusal = document
        .apply(&StreamGraphOperation::AddGroup {
            group: a_group_titled("inputs"),
            nodes: vec![source.id],
        })
        .unwrap_err();

    assert!(refusal.contains(&sources.id.to_string()), "{refusal}");
    assert!(refusal.contains("`sources`"), "{refusal}");
    assert_eq!(document, before);
}

#[test]
fn a_studio_group_naming_no_group_reads_as_no_member_of_any() {
    let document = StreamGraphDocument::from_value(json!({
        "nodes": [{ "id": Uuid::new_v4(), "name": "camera", "type": "nodes.camera:Camera",
                    "studio": { "group": "sources" } }],
    }))
    .unwrap();
    let group = a_group_titled("sources");

    assert!(document.groups().unwrap().is_empty());
    assert!(document.nodes_of_the_group(group.id).is_empty());
    assert!(
        document
            .group(group.id)
            .unwrap_err()
            .contains("no group at all")
    );
}

#[test]
fn a_generators_group_takes_a_uuid_kept_by_its_title_and_marks_each_node_it_names() {
    let a_run_grouping = |title: &str| {
        json!({
            "nodes": [
                {"name": "blur", "type": "nodes.blur:Blur", "config": {}},
                {"name": "tint", "type": "nodes.tint:Tint", "config": {}},
            ],
            "links": [],
            "groups": [{"title": title, "colour": "#f97316", "nodes": ["blur", "tint"]}],
        })
    };
    let first_run = fragment_with_each_node_uuid_kept_by_its_name(
        &a_run_grouping("soften"),
        None,
        &mut Uuid::new_v4,
    )
    .unwrap();
    let group_uuid = first_run["groups"][0]["id"].clone();

    assert_eq!(
        first_run["groups"],
        json!([{"id": group_uuid, "title": "soften", "colour": "#f97316"}])
    );
    for node in first_run["nodes"].as_array().unwrap() {
        assert_eq!(node["studio"]["group"], group_uuid);
    }
    let rerun = fragment_with_each_node_uuid_kept_by_its_name(
        &a_run_grouping("soften"),
        Some(&first_run),
        &mut Uuid::new_v4,
    )
    .unwrap();
    assert_eq!(rerun, first_run);
    let retitled = fragment_with_each_node_uuid_kept_by_its_name(
        &a_run_grouping("blur then tint"),
        Some(&first_run),
        &mut Uuid::new_v4,
    )
    .unwrap();
    assert_ne!(retitled["groups"][0]["id"], group_uuid);
}

#[test]
fn a_generators_group_naming_a_node_the_fragment_does_not_hold_is_refused_naming_both() {
    let refusal = fragment_with_each_node_uuid_kept_by_its_name(
        &json!({
            "nodes": [{"name": "blur", "type": "nodes.blur:Blur"}],
            "groups": [{"title": "soften", "nodes": ["glow"]}],
        }),
        None,
        &mut Uuid::new_v4,
    )
    .unwrap_err();

    assert!(refusal.contains("`soften`"), "{refusal}");
    assert!(refusal.contains("`glow`"), "{refusal}");
}

#[test]
fn a_node_holding_its_config_or_pins_as_no_object_is_refused_naming_it_and_the_key() {
    for key in ["config", "pins"] {
        let node = Uuid::new_v4();
        let mut recorded = json!({"id": node, "name": "tone", "type": "nodes.tone:Tone"});
        recorded[key] = json!([1]);
        let refusal = serde_json::from_value::<StreamGraphDocumentNode>(recorded.clone())
            .expect_err("refused")
            .to_string();
        assert!(
            refusal.contains(&format!("node {node} `tone`"))
                && refusal.contains(&format!("`{key}`")),
            "{refusal}"
        );
        let document = StreamGraphDocument::from_value(json!({"nodes": [recorded]})).unwrap();
        let NodeNotRead::DoesNotRead(refusal) = document.node(node).expect_err("refused") else {
            panic!("the document holds node {node}");
        };
        assert!(refusal.contains(&format!("`{key}`")), "{refusal}");
    }
}

/// A node holds one map of input values, so a `set_node_config` naming one of the node's inputs
/// in `to` sets that input's value, and each field `from` names that `to` does not is taken out.
#[test]
fn a_set_node_config_naming_an_input_in_to_sets_the_value_of_that_input() {
    let mut document = an_empty_document();
    let mut tone = a_node("tone");
    tone.input_values = Map::from_iter([
        ("gain".to_owned(), json!(1)),
        ("pitch".to_owned(), json!(220)),
    ]);
    document
        .apply(&StreamGraphOperation::AddNode {
            node: tone.clone(),
            links: Vec::new(),
        })
        .unwrap();

    let set_node_config = StreamGraphOperation::set_node_config_of(
        &tone,
        json!({"gain": 1}),
        Map::from_iter([
            ("pitch".to_owned(), json!(440)),
            ("waveform".to_owned(), json!("saw")),
        ]),
    );
    assert_eq!(
        set_node_config,
        StreamGraphOperation::SetNodeConfig {
            node: tone.id,
            from: json!({"gain": 1, "pitch": 220}),
            to: json!({"pitch": 440, "waveform": "saw"}),
        }
    );

    document.apply(&set_node_config).unwrap();
    assert_eq!(
        document.node(tone.id).unwrap().input_values,
        Map::from_iter([
            ("pitch".to_owned(), json!(440)),
            ("waveform".to_owned(), json!("saw")),
        ])
    );
    document.apply(&set_node_config.inverse()).unwrap();
    assert_eq!(
        document.node(tone.id).unwrap().input_values,
        tone.input_values
    );
}

#[test]
fn a_clear_of_an_exposed_config_fields_input_is_refused_and_any_other_set() {
    let mut tone = a_node("tone");
    tone.input_values = Map::from_iter([
        ("gain".to_owned(), json!(1)),
        ("pitch".to_owned(), json!(220)),
    ]);
    tone.exposed_config_fields = vec!["gain".to_owned()];

    let refusal = StreamGraphOperation::set_pin_value_of(&tone, "gain".to_owned(), None)
        .expect_err("a clear of an exposed config field is refused");
    assert!(
        refusal.contains("`gain`") && refusal.contains("set_node_config"),
        "{refusal}"
    );
    assert_eq!(
        StreamGraphOperation::set_pin_value_of(&tone, "pitch".to_owned(), Some(Value::Null)),
        Ok(StreamGraphOperation::SetNodeInputValue {
            member: None,
            node: tone.id,
            input: "pitch".to_owned(),
            from: Some(json!(220)),
            to: None,
        })
    );
    assert!(
        StreamGraphOperation::set_pin_value_of(&tone, "gain".to_owned(), Some(json!(2))).is_ok()
    );
}

#[test]
fn the_input_values_an_edit_sets_include_a_value_on_one_input_and_a_retired_shape_upgraded() {
    let node = Uuid::new_v4();
    let lut = Uuid::new_v4();
    let set_pin_value = StreamGraphOperation::SetPinValue {
        node,
        input: "lut".to_owned(),
        from: None,
        to: Some(json!({"asset": lut})),
    };
    assert_eq!(
        set_pin_value.input_values_it_sets(),
        [(
            node,
            Map::from_iter([("lut".to_owned(), json!({"asset": lut}))])
        )]
    );

    let inner = Uuid::new_v4();
    let mut add_node = a_node("look");
    add_node.fragment = Some(Box::new(json!({
        "nodes": [{"id": inner, "name": "grade", "type": "nodes.grade:Grade",
                   "config": {"lut": {"asset": lut}}}],
        "links": [],
    })));
    let add_node = StreamGraphOperation::AddNode {
        node: add_node.clone(),
        links: Vec::new(),
    };
    assert_eq!(
        add_node.input_values_it_sets()[1],
        (
            inner,
            Map::from_iter([("lut".to_owned(), json!({"asset": lut}))])
        )
    );
}

/// An edit recorded before a node held one map of input values still reads and applies, so it
/// undoes and redoes: what it places takes the current shape, at every level of a fragment.
#[test]
fn an_add_node_recorded_before_input_values_places_its_node_in_the_current_shape() {
    let node = Uuid::new_v4();
    let recorded = json!({
        "operation": "add_node",
        "node": {
            "id": node, "name": "look", "type": "generators.look:look",
            "config": {"radius": 3, "amount": 1}, "pins": {"amount": 2},
            "fragment": {
                "nodes": [{"id": Uuid::new_v4(), "name": "lfo", "type": "tatolab.ops:Lfo",
                           "config": {"rate_hz": 2}}],
                "links": [],
            },
        },
        "links": [],
    });
    let mut document = an_empty_document();

    document
        .apply(&serde_json::from_value(recorded).expect("an old edit reads"))
        .unwrap();

    let placed = &document.to_value()["nodes"][0];
    assert_eq!(placed["input_values"], json!({"radius": 3, "amount": 2}));
    assert!(placed.get("config").is_none() && placed.get("pins").is_none());
    assert_eq!(
        placed["fragment"]["nodes"][0],
        json!({"id": placed["fragment"]["nodes"][0]["id"], "name": "lfo",
               "type": "imports/core-nodes/core_nodes.animation:Lfo",
               "input_values": {"rate_hz": 2}})
    );
}

/// Mental-revert: a link end read without its `member` reopens as a link from the whole port.
#[test]
fn a_link_from_one_member_of_a_bundle_reopens_from_that_member() {
    let mut document = an_empty_document();
    let source = a_node("sensor");
    let sink = a_node("meter");
    for node in [&source, &sink] {
        document
            .apply(&StreamGraphOperation::AddNode {
                node: node.clone(),
                links: Vec::new(),
            })
            .unwrap();
    }
    let mut link = a_link(&source, &sink);
    link.source.member = Some("level".to_owned());
    document
        .apply(&StreamGraphOperation::Connect { link: link.clone() })
        .unwrap();

    let saved = document.to_value();
    assert_eq!(saved["links"][0]["source"]["member"], json!("level"));
    assert!(saved["links"][0]["target"].get("member").is_none());
    let reopened = StreamGraphDocument::from_value(saved).unwrap();

    assert_eq!(reopened.links(), [link]);
}
