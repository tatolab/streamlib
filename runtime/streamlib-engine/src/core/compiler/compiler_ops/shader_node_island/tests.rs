// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::super::function_node_island::tests::{a_function_node_type, add_a_node, link};
use super::super::function_node_island::{HowALinkIsWired, how_a_link_is_wired};
use super::super::shader_node_island_program::tests::shader_program;
use super::*;
use crate::core::compiler::compiler_ops::running_node_restart::forget_the_build_of_each_node_restarted;
use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PortDescriptor,
};
use crate::core::descriptors::{PinDeclaration, PinType};
use crate::core::graph::{GraphEdgeWithComponents, NodeMetrics, PendingDeletionComponent};
use crate::core::nodes::NodeState;
use crate::core::nodes::{HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves};

fn a_descriptor(
    import_path: &str,
    node_kind: NodeKind,
    inputs: &[(&str, PinType)],
    outputs: &[&str],
) -> NodeDescriptor {
    let import_path = NodeClassImportPath::new(import_path).unwrap();
    let short_name = import_path.as_str().rsplit(':').next().unwrap().to_string();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        import_path,
        "a node type for a test",
    );
    descriptor.node_kind = node_kind;
    for (port, pin_type) in inputs {
        descriptor = descriptor.with_input(
            PortDescriptor::new(*port, "", true, PinDeclaration::exactly(pin_type.clone()))
                .with_delivery_profile("newest"),
        );
    }
    for port in outputs {
        descriptor = descriptor.with_output(PortDescriptor::new(
            *port,
            "",
            true,
            PinDeclaration::exactly(PinType::Texture {
                pixel_formats: Vec::new(),
            }),
        ));
    }
    descriptor
}

fn a_constructor_that_builds_nothing() -> crate::core::nodes::DynamicNodeConstructorFn {
    Box::new(|node| {
        Err(Error::Configuration(format!(
            "`{}` builds no node in this test",
            node.id
        )))
    })
}

/// A graph resolving `source` and `sink`, two process types carrying
/// Images, `tint`, `blur` and `blend`, three shader types, `programless`, a
/// shader type the stream holds no program for, and `lfo`, a function node
/// type.
fn a_graph_resolving_shader_and_process_types() -> Graph {
    let node_types = NodeTypesOneStreamResolves::new();
    for descriptor in [
        a_descriptor("test_streams:source", NodeKind::Process, &[], &["video"]),
        a_descriptor(
            "test_streams:sink",
            NodeKind::Process,
            &[(
                "video",
                PinType::Texture {
                    pixel_formats: Vec::new(),
                },
            )],
            &[],
        ),
    ] {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                    a_constructor_that_builds_nothing(),
                ),
            )
            .unwrap();
    }
    for (function_name, image_ports, float_ports, slang_body) in [
        (
            "tint",
            &["image"][..],
            &["amount"][..],
            "return image.pixel * amount;",
        ),
        (
            "blur",
            &["image"][..],
            &[][..],
            "return image.sample(float2(0.5, 0.5));",
        ),
        (
            "blend",
            &["a", "b"][..],
            &[][..],
            "return lerp(a.pixel, b.pixel, 0.5);",
        ),
    ] {
        let inputs: Vec<(&str, PinType)> = image_ports
            .iter()
            .map(|port| {
                (
                    *port,
                    PinType::Texture {
                        pixel_formats: Vec::new(),
                    },
                )
            })
            .chain(
                float_ports
                    .iter()
                    .map(|port| (*port, PinType::Float { exact_size: None })),
            )
            .collect();
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                a_descriptor(
                    &format!("test_shaders:{function_name}"),
                    NodeKind::Shader,
                    &inputs,
                    &["result"],
                ),
                HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                    shader_node_program: shader_program(
                        function_name,
                        slang_body,
                        image_ports,
                        float_ports,
                    ),
                    evaluator_of_the_island_it_hosts: a_constructor_that_builds_nothing(),
                },
            )
            .unwrap();
    }
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_descriptor(
                "test_shaders:programless",
                NodeKind::Shader,
                &[(
                    "image",
                    PinType::Texture {
                        pixel_formats: Vec::new(),
                    },
                )],
                &["result"],
            ),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                a_constructor_that_builds_nothing(),
            ),
        )
        .unwrap();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_function_node_type("test_ops:lfo", &[], &["result"]),
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: a_constructor_that_builds_nothing(),
            },
        )
        .unwrap();
    crate::core::test_support::ensure_test_mocks_registered();
    Graph::new_resolving_node_types_through(Arc::new(node_types))
}

fn add(graph: &mut Graph, import_path: &str) -> NodeRunId {
    add_a_node(graph, import_path, serde_json::json!({}))
}

struct SourceIntoTintIntoBlurIntoTintIntoSink {
    graph: Graph,
    source: NodeRunId,
    tint: NodeRunId,
    blur: NodeRunId,
    tint_again: NodeRunId,
    sink: NodeRunId,
    links: Vec<LinkUniqueId>,
}

fn source_into_tint_into_blur_into_tint_into_sink() -> SourceIntoTintIntoBlurIntoTintIntoSink {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let source = add(&mut graph, "test_streams:source");
    let tint = add(&mut graph, "test_shaders:tint");
    let blur = add(&mut graph, "test_shaders:blur");
    let tint_again = add(&mut graph, "test_shaders:tint");
    let sink = add(&mut graph, "test_streams:sink");
    let links = vec![
        link(&mut graph, (&source, "video"), (&tint, "image")),
        link(&mut graph, (&tint, "result"), (&blur, "image")),
        link(&mut graph, (&blur, "result"), (&tint_again, "image")),
        link(&mut graph, (&tint_again, "result"), (&sink, "video")),
    ];
    SourceIntoTintIntoBlurIntoTintIntoSink {
        graph,
        source,
        tint,
        blur,
        tint_again,
        sink,
        links,
    }
}

#[test]
fn a_shader_chain_is_one_island_evaluated_by_the_node_whose_image_leaves_it() {
    let stream = source_into_tint_into_blur_into_tint_into_sink();
    for member in [&stream.tint, &stream.blur, &stream.tint_again] {
        assert_eq!(
            the_host_of_the_shader_node_island_of(&stream.graph, member),
            stream.tint_again
        );
    }
    assert!(is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.tint
    ));
    assert!(!is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.tint_again
    ));
    let (island, member_nodes) =
        the_shader_node_island_hosted_by(&stream.graph, &stream.tint_again).unwrap();
    assert_eq!(
        member_nodes,
        [
            stream.tint.clone(),
            stream.blur.clone(),
            stream.tint_again.clone()
        ]
    );
    assert_eq!(
        island.the_input_port_each_evaluation_waits_on(),
        the_host_input_port_a_fused_shader_node_input_is_read_at(
            &stream.tint_again,
            &stream.tint,
            "image"
        )
        .port_name()
    );
}

/// A link is wired as its islands were placed, so a link a live edit removes is unwired as it
/// was wired, whatever the edit leaves.
#[test]
fn links_inside_an_island_are_calls_and_a_link_into_a_fused_node_reads_at_its_host() {
    let mut stream = the_chain_placed_and_running();
    let second_sink = add(&mut stream.graph, "test_streams:sink");
    link(
        &mut stream.graph,
        (&stream.blur, "result"),
        (&second_sink, "video"),
    );
    remove_live(&mut stream.graph, &stream.links[1]);
    let how_each_link_is_wired: Vec<HowALinkIsWired> = stream
        .links
        .iter()
        .map(|link_id| how_a_link_is_wired(&stream.graph, link_id).unwrap())
        .collect();
    assert_eq!(
        how_each_link_is_wired,
        [
            HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(
                the_host_input_port_a_fused_shader_node_input_is_read_at(
                    &stream.tint_again,
                    &stream.tint,
                    "image"
                )
            ),
            HowALinkIsWired::AsACallInsideAShaderNodeIsland,
            HowALinkIsWired::AsACallInsideAShaderNodeIsland,
            HowALinkIsWired::ThroughAChannel,
        ]
    );
}

#[test]
fn a_shader_whose_image_fans_out_or_reaches_a_non_shader_node_is_not_fused() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let second_sink = add(&mut stream.graph, "test_streams:sink");
    link(
        &mut stream.graph,
        (&stream.tint, "result"),
        (&second_sink, "video"),
    );
    assert!(!is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.tint
    ));
    assert_eq!(
        the_host_of_the_shader_node_island_of(&stream.graph, &stream.tint),
        stream.tint
    );
    assert_eq!(
        the_host_of_the_shader_node_island_of(&stream.graph, &stream.blur),
        stream.tint_again
    );
    assert!(!is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.source
    ));
}

#[test]
fn placing_the_islands_hands_the_island_to_its_host_and_its_group_to_every_member() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    let placed = place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    assert_eq!(
        placed.fused_into_another,
        HashSet::from([stream.tint.clone(), stream.blur.clone()])
    );
    assert!(placed.in_a_cycle_no_node_evaluates.is_empty());
    let host = stream
        .graph
        .traversal()
        .v(&stream.tint_again)
        .first()
        .unwrap();
    let hosted = host.get::<ShaderNodeIslandHostedHereComponent>().unwrap();
    assert_eq!(hosted.island.members().len(), 3);
    for member in [&stream.tint, &stream.blur, &stream.tint_again] {
        let node = stream.graph.traversal().v(member).first().unwrap();
        let fused_group = node.get::<ShaderNodeFusedGroupComponent>().unwrap();
        assert!(Arc::ptr_eq(&fused_group.0, &hosted.fused_group));
    }
    let rendered = stream
        .graph
        .traversal()
        .v(&stream.tint)
        .first()
        .unwrap()
        .render_components();
    let rendered = serde_json::to_value(rendered).unwrap();
    assert_eq!(
        rendered["fused_group"]["evaluated_by"],
        stream
            .graph
            .traversal()
            .v(&stream.tint_again)
            .first()
            .unwrap()
            .display_name
    );
}

fn sorted<T: Ord + Clone>(items: &[T]) -> Vec<T> {
    let mut sorted = items.to_vec();
    sorted.sort();
    sorted
}

/// The chain of [`source_into_tint_into_blur_into_tint_into_sink`], placed as a running stream
/// places it.
fn the_chain_placed_and_running() -> SourceIntoTintIntoBlurIntoTintIntoSink {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    stream
}

/// Mark `link_id` pending deletion, as a live edit removing it leaves it until its compile.
fn remove_live(graph: &mut Graph, link_id: &LinkUniqueId) {
    graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .unwrap()
        .insert(PendingDeletionComponent);
}

/// Take what a live edit removed out of the graph, then place the running nodes it placed anew
/// with the nodes it adds, and hand each running host its island, as the compile does.
fn compile_the_placement(
    graph: &mut Graph,
    placed_anew: &ShaderNodeIslandsALiveEditPlacesAnew,
    nodes_to_add: &[NodeRunId],
) {
    let removed: Vec<LinkUniqueId> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| !graph.stands(link))
        .map(|link| link.id.clone())
        .collect();
    for link_id in &removed {
        graph.traversal_mut().e(link_id).drop();
    }
    forget_the_build_of_each_node_restarted(graph, &placed_anew.nodes_whose_place_changes);
    let nodes_to_place = [nodes_to_add, &placed_anew.the_running_nodes_placed_anew()].concat();
    place_each_shader_node_island_on_its_host(graph, &nodes_to_place).unwrap();
    hand_each_running_host_the_island_placed_on_it(
        graph,
        &placed_anew.hosts_handed_their_island_anew,
    );
}

/// The hand-over slot of the host `host_id`.
fn the_hand_over_of(
    graph: &Graph,
    host_id: &NodeRunId,
) -> Arc<ShaderNodeIslandHandedToItsEvaluator> {
    Arc::clone(
        &graph
            .traversal()
            .v(host_id)
            .first()
            .unwrap()
            .get::<ShaderNodeIslandHostedHereComponent>()
            .unwrap()
            .handed_to_its_evaluator,
    )
}

fn the_name_of(graph: &Graph, node_id: &NodeRunId) -> String {
    graph
        .traversal()
        .v(node_id)
        .first()
        .unwrap()
        .display_name
        .clone()
}

/// A shader node added inside a running fused group is placed in it, and the group's host,
/// still the same node, is handed the group with it, so nothing is stopped or built again: the
/// links into the group are read where they were.
///
/// Mental-revert: restarting the island stops and builds again its host, `tint_again`.
#[test]
fn a_shader_node_added_inside_a_running_group_is_handed_to_its_host_restarting_nothing() {
    let mut stream = the_chain_placed_and_running();
    let hand_over_before = the_hand_over_of(&stream.graph, &stream.tint_again);
    let vignette = add(&mut stream.graph, "test_shaders:tint");
    let tint_into_blur = stream.links[1].clone();
    remove_live(&mut stream.graph, &tint_into_blur);
    let tint_into_vignette = link(
        &mut stream.graph,
        (&stream.tint, "result"),
        (&vignette, "image"),
    );
    let vignette_into_blur = link(
        &mut stream.graph,
        (&vignette, "result"),
        (&stream.blur, "image"),
    );

    let placed_anew = the_shader_node_islands_a_live_edit_places_anew(
        &stream.graph,
        std::slice::from_ref(&vignette),
        &[],
        &[tint_into_blur, tint_into_vignette, vignette_into_blur],
    )
    .unwrap();
    assert_eq!(
        placed_anew,
        ShaderNodeIslandsALiveEditPlacesAnew {
            hosts_handed_their_island_anew: vec![stream.tint_again.clone()],
            fused_nodes_placed_anew: sorted(&[stream.tint.clone(), stream.blur.clone()]),
            nodes_whose_place_changes: Vec::new(),
            links_wired_anew: Vec::new(),
        }
    );

    compile_the_placement(
        &mut stream.graph,
        &placed_anew,
        std::slice::from_ref(&vignette),
    );
    let hand_over_after = the_hand_over_of(&stream.graph, &stream.tint_again);
    assert!(
        Arc::ptr_eq(&hand_over_before, &hand_over_after),
        "the evaluator already running on the host takes the new group"
    );
    let ShaderNodeIslandWithItsFusedGroup {
        island,
        fused_group,
    } = hand_over_after
        .take()
        .expect("the host is handed its group");
    let names: Vec<String> = [&stream.tint, &vignette, &stream.blur, &stream.tint_again]
        .into_iter()
        .map(|node_id| the_name_of(&stream.graph, node_id))
        .collect();
    assert_eq!(island.member_names(), names);
    let vignette_node = stream.graph.traversal().v(&vignette).first().unwrap();
    assert!(vignette_node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>());
    assert!(Arc::ptr_eq(
        &vignette_node
            .get::<ShaderNodeFusedGroupComponent>()
            .unwrap()
            .0,
        &fused_group
    ));
}

/// A shader node removed from the middle of a running fused group, the nodes either side of it
/// linked in its place, leaves the group's host handed the group without it.
///
/// Mental-revert: skipping the links of a removed node leaves the host evaluating the node gone.
#[test]
fn a_shader_node_removed_from_a_running_group_is_handed_out_of_its_host_restarting_nothing() {
    let mut stream = the_chain_placed_and_running();
    for link_id in [&stream.links[1], &stream.links[2]] {
        remove_live(&mut stream.graph, link_id);
    }
    stream
        .graph
        .traversal_mut()
        .v(&stream.blur)
        .first_mut()
        .unwrap()
        .insert(PendingDeletionComponent);
    let tint_into_tint_again = link(
        &mut stream.graph,
        (&stream.tint, "result"),
        (&stream.tint_again, "image"),
    );

    let placed_anew = the_shader_node_islands_a_live_edit_places_anew(
        &stream.graph,
        &[],
        std::slice::from_ref(&stream.blur),
        std::slice::from_ref(&tint_into_tint_again),
    )
    .unwrap();
    assert_eq!(
        placed_anew,
        ShaderNodeIslandsALiveEditPlacesAnew {
            hosts_handed_their_island_anew: vec![stream.tint_again.clone()],
            fused_nodes_placed_anew: vec![stream.tint.clone()],
            nodes_whose_place_changes: Vec::new(),
            links_wired_anew: Vec::new(),
        }
    );

    stream.graph.traversal_mut().v(&stream.blur).drop();
    compile_the_placement(&mut stream.graph, &placed_anew, &[]);
    let island = the_hand_over_of(&stream.graph, &stream.tint_again)
        .take()
        .expect("the host is handed its group")
        .island;
    assert_eq!(
        island.member_names(),
        [
            the_name_of(&stream.graph, &stream.tint),
            the_name_of(&stream.graph, &stream.tint_again)
        ]
    );
}

/// A second reader of a fused node's Image splits the running group: the host keeps its
/// evaluator, handed what is left, the fused node becomes the host of the nodes before it, and
/// only the links whose wiring changes are wired again.
///
/// Mental-revert: restarting the whole island stops the host, `tint_again`, and every link of
/// the island.
#[test]
fn a_second_reader_of_a_fused_node_splits_the_running_group_restarting_no_host() {
    let mut stream = the_chain_placed_and_running();
    let second_sink = add(&mut stream.graph, "test_streams:sink");
    let into_a_fused_node_output = link(
        &mut stream.graph,
        (&stream.blur, "result"),
        (&second_sink, "video"),
    );
    let placed_anew = the_shader_node_islands_a_live_edit_places_anew(
        &stream.graph,
        std::slice::from_ref(&second_sink),
        &[],
        std::slice::from_ref(&into_a_fused_node_output),
    )
    .unwrap();
    assert_eq!(
        placed_anew,
        ShaderNodeIslandsALiveEditPlacesAnew {
            hosts_handed_their_island_anew: vec![stream.tint_again.clone()],
            fused_nodes_placed_anew: vec![stream.tint.clone()],
            nodes_whose_place_changes: vec![stream.blur.clone()],
            links_wired_anew: sorted(&stream.links[0..3]),
        }
    );
}

/// A link fusing two running groups hands the group to the host of the island it reads into,
/// and the other host, now fused, stops evaluating; the links into it are read at the host it
/// is fused into from then on.
///
/// Mental-revert: restarting both islands stops and builds again `kaleidoscope`.
#[test]
fn a_link_fusing_two_running_groups_hands_both_to_the_host_restarting_it_not() {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let camera = add(&mut graph, "test_streams:source");
    let test_pattern = add(&mut graph, "test_streams:source");
    let mix = add(&mut graph, "test_shaders:blend");
    let kaleidoscope = add(&mut graph, "test_shaders:tint");
    let program = add(&mut graph, "test_streams:sink");
    let into_the_mix = vec![
        link(&mut graph, (&camera, "video"), (&mix, "a")),
        link(&mut graph, (&test_pattern, "video"), (&mix, "b")),
    ];
    link(&mut graph, (&kaleidoscope, "result"), (&program, "video"));
    place_each_shader_node_island_on_its_host(
        &mut graph,
        &[
            camera.clone(),
            test_pattern.clone(),
            mix.clone(),
            kaleidoscope.clone(),
            program.clone(),
        ],
    )
    .unwrap();
    let mix_into_kaleidoscope = link(&mut graph, (&mix, "result"), (&kaleidoscope, "image"));

    let placed_anew = the_shader_node_islands_a_live_edit_places_anew(
        &graph,
        &[],
        &[],
        std::slice::from_ref(&mix_into_kaleidoscope),
    )
    .unwrap();
    assert_eq!(
        placed_anew,
        ShaderNodeIslandsALiveEditPlacesAnew {
            hosts_handed_their_island_anew: vec![kaleidoscope.clone()],
            fused_nodes_placed_anew: Vec::new(),
            nodes_whose_place_changes: vec![mix.clone()],
            links_wired_anew: sorted(&into_the_mix),
        }
    );
}

/// A link from outside a running group into its host or into a fused member changes no group,
/// so nothing is placed anew and the link is wired where its reader is read.
///
/// Mental-revert: refusing a link from a function node into a running group asks for a stop.
#[test]
fn a_link_into_a_running_group_from_outside_it_places_nothing_anew() {
    let mut stream = the_chain_placed_and_running();
    let lfo = add(&mut stream.graph, "test_ops:lfo");
    let into_the_host = link(
        &mut stream.graph,
        (&lfo, "result"),
        (&stream.tint_again, "amount"),
    );
    let into_a_fused_node = link(
        &mut stream.graph,
        (&lfo, "result"),
        (&stream.tint, "amount"),
    );
    let placed_anew = the_shader_node_islands_a_live_edit_places_anew(
        &stream.graph,
        std::slice::from_ref(&lfo),
        &[],
        &[into_the_host, into_a_fused_node],
    )
    .unwrap();
    assert_eq!(placed_anew, ShaderNodeIslandsALiveEditPlacesAnew::default());
}

/// Removing every node of a running stream places nothing anew.
#[test]
fn a_teardown_places_nothing_anew() {
    let stream = the_chain_placed_and_running();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    let placed_anew = the_shader_node_islands_a_live_edit_places_anew(
        &stream.graph,
        &[],
        &every_node,
        &stream.links,
    )
    .unwrap();
    assert_eq!(placed_anew, ShaderNodeIslandsALiveEditPlacesAnew::default());
}

/// A group the edit leaves that does not build refuses the edit with its own kind before
/// anything changes, so the running group is placed as it was and its host is handed nothing.
///
/// Mental-revert: building the group only as it is placed fails the compile after the links it
/// removes are gone.
#[test]
fn a_group_that_does_not_build_refuses_the_edit_and_leaves_the_running_group_as_it_was() {
    let mut stream = the_chain_placed_and_running();
    let (island_before, _) =
        the_shader_node_island_hosted_by(&stream.graph, &stream.tint_again).unwrap();
    let programless = add(&mut stream.graph, "test_shaders:programless");
    let blur_into_tint_again = stream.links[2].clone();
    remove_live(&mut stream.graph, &blur_into_tint_again);
    let added = [
        link(
            &mut stream.graph,
            (&stream.blur, "result"),
            (&programless, "image"),
        ),
        link(
            &mut stream.graph,
            (&programless, "result"),
            (&stream.tint_again, "image"),
        ),
    ];

    let refusal = the_shader_node_islands_a_live_edit_places_anew(
        &stream.graph,
        std::slice::from_ref(&programless),
        &[],
        &[[blur_into_tint_again].as_slice(), &added].concat(),
    )
    .unwrap_err();
    match &refusal {
        Error::ShaderNodeFusedGroupNotBuilt { evaluated_by, why } => {
            assert_eq!(
                evaluated_by,
                &the_name_of(&stream.graph, &stream.tint_again)
            );
            assert!(why.contains("holds no shader program"), "{why}");
        }
        refusal => panic!("refused as another kind: {refusal:?}"),
    }
    assert!(
        !refusal.to_string().contains("stop the stream"),
        "{refusal}"
    );
    let host = stream
        .graph
        .traversal()
        .v(&stream.tint_again)
        .first()
        .unwrap();
    let hosted = host.get::<ShaderNodeIslandHostedHereComponent>().unwrap();
    assert_eq!(hosted.island, island_before);
    assert!(hosted.handed_to_its_evaluator.take().is_none());
}

#[test]
fn a_node_whose_place_changes_reads_its_old_host_and_forgets_every_part_of_its_build() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    assert_eq!(
        the_host_the_island_of_a_fused_node_was_placed_on(&stream.graph, &stream.tint),
        Some(stream.tint_again.clone())
    );
    let island = [
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
    ];
    for node_id in &island {
        stream
            .graph
            .traversal_mut()
            .v(node_id)
            .first_mut()
            .unwrap()
            .insert(NodeMetrics::default());
    }
    forget_the_build_of_each_node_restarted(&mut stream.graph, &island);
    for node_id in &island {
        let node = stream.graph.traversal().v(node_id).first().unwrap();
        assert!(!node.has::<NodeMetrics>());
        assert!(!node.has::<ShaderNodeIslandHostedHereComponent>());
        assert!(!node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>());
        assert!(!node.has::<ShaderNodeFusedGroupComponent>());
    }
}

#[test]
fn the_host_input_port_of_a_fused_input_is_already_in_cast_form() {
    let port = the_host_input_port_a_fused_shader_node_input_is_read_at(
        &NodeRunId::from("HostNodeId"),
        &NodeRunId::from("Plh868rqg4MCMX"),
        "image",
    );
    assert_eq!(
        cast_exposed_name_to_url_safe(port.port_name()).unwrap(),
        port.port_name()
    );
}

/// Shader nodes reading their own Image in a cycle are each failed with why on its node and
/// placed on no island, so the compile goes on and the rest of the stream plays; a shader node
/// outside the cycle is placed as ever.
///
/// Mental-revert: refusing the placement fails the whole stream at its start.
#[test]
fn shader_nodes_reading_their_own_image_in_a_cycle_are_failed_on_their_nodes() {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let first = add(&mut graph, "test_shaders:blend");
    let second = add(&mut graph, "test_shaders:tint");
    let lone = add(&mut graph, "test_shaders:tint");
    link(&mut graph, (&first, "result"), (&second, "image"));
    link(&mut graph, (&second, "result"), (&first, "a"));
    let placed = place_each_shader_node_island_on_its_host(
        &mut graph,
        &[first.clone(), second.clone(), lone.clone()],
    )
    .expect("a cycle fails its nodes, not the compile");
    assert_eq!(
        placed.in_a_cycle_no_node_evaluates,
        vec![first.clone(), second.clone()]
    );
    assert!(placed.fused_into_another.is_empty());
    for node_id in [&first, &second] {
        let node = graph.traversal().v(node_id).first().unwrap();
        let state = node.get::<StateComponent>().unwrap();
        assert_eq!(state.current(), NodeState::Error);
        let failed_because = node.render_components().failed_because;
        assert!(
            failed_because
                .as_deref()
                .is_some_and(|why| why.contains("break the cycle")),
            "{failed_because:?}"
        );
        assert!(!node.has::<ShaderNodeIslandHostedHereComponent>());
    }
    assert!(
        graph
            .traversal()
            .v(&lone)
            .first()
            .unwrap()
            .has::<ShaderNodeIslandHostedHereComponent>()
    );
}

/// The members of an island are each named once: a shader node linked into one member through
/// two inputs is named at most once, and shader nodes calling each other in a cycle are each
/// named once.
///
/// Mental-revert: walking each link without checking the members already named never ends on
/// the cycle.
#[test]
fn each_member_of_an_island_is_named_once() {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let tint = add(&mut graph, "test_shaders:tint");
    let blend = add(&mut graph, "test_shaders:blend");
    let sink = add(&mut graph, "test_streams:sink");
    link(&mut graph, (&tint, "result"), (&blend, "a"));
    link(&mut graph, (&tint, "result"), (&blend, "b"));
    link(&mut graph, (&blend, "result"), (&sink, "video"));
    assert_eq!(
        the_members_of_the_shader_node_island_hosted_by(&graph, &blend),
        std::slice::from_ref(&blend),
        "a node whose Image two inputs read is read twice, so not fused, and not named twice"
    );

    let mut graph = a_graph_resolving_shader_and_process_types();
    let first = add(&mut graph, "test_shaders:blend");
    let second = add(&mut graph, "test_shaders:tint");
    link(&mut graph, (&first, "result"), (&second, "image"));
    link(&mut graph, (&second, "result"), (&first, "a"));
    assert_eq!(
        the_members_of_the_shader_node_island_hosted_by(&graph, &first),
        [first.clone(), second.clone()]
    );
}
