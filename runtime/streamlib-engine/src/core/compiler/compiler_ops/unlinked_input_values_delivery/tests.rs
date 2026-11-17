// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::link;
use crate::core::compiler::compiler_ops::input_port_pin_values::tests::{
    a_graph_resolving_a_process_node_type_with_channels_inputs, a_level_meter_in_a_helper,
    the_link_wiring_of,
};
use crate::core::descriptors::NodeValueLifetime;
use crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery;

fn the_uuid_of(graph: &Graph, node_id: &NodeRunId) -> uuid::Uuid {
    graph.traversal().v(node_id).first().unwrap().node_uuid
}

/// Hold `value` live on input `input` of `node`, then hand that node what it reads now.
fn hold_live_and_deliver(graph: &RwLock<Graph>, node_id: &NodeRunId, input: &str, value: Value) {
    let node = the_uuid_of(&graph.read(), node_id);
    graph
        .read()
        .live_input_values()
        .set(node, input, Some(value), NodeValueLifetime::Kept);
    deliver_the_unlinked_input_values_of_each_node(graph, std::slice::from_ref(node_id)).unwrap();
}

/// A helper's setup command carries the value each input reads while unlinked, a declared default
/// included, as `input_values` in place of `pins`; a later change is handed over whole, as it was
/// set, and values that did not change are not handed over again.
///
/// Mental-revert: carrying the values under `pins` leaves a helper reading `input_values` with
/// none; handing the number each reads turns `true` into 1.0 before the helper sees it.
#[test]
fn a_helpers_setup_carries_each_input_value_and_a_change_after_it_is_handed_over_whole() {
    let mut graph = a_graph_resolving_a_process_node_type_with_channels_inputs();
    let level_meter = a_level_meter_in_a_helper(&mut graph);
    let graph = RwLock::new(graph);
    deliver_the_unlinked_input_values_of_each_node(&graph, std::slice::from_ref(&level_meter))
        .unwrap();
    let ports = the_link_wiring_of(&graph.read(), &level_meter).as_setup_command_ports();
    assert_eq!(ports["input_values"], json!({"level": 0.5}));
    assert!(ports.get("pins").is_none(), "{ports}");

    let delivery = RecordingOutOfProcessFarSideLinkDelivery::default();
    the_link_wiring_of(&graph.read(), &level_meter)
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery.clone())
        .unwrap();
    graph
        .write()
        .traversal_mut()
        .v(&level_meter)
        .first_mut()
        .unwrap()
        .pins
        .insert("gain".to_owned(), json!(2));
    hold_live_and_deliver(&graph, &level_meter, "level", json!(true));
    deliver_the_unlinked_input_values_of_each_node(&graph, std::slice::from_ref(&level_meter))
        .unwrap();

    assert_eq!(
        *delivery.input_values_handed_over.lock(),
        vec![
            json!({"gain": 2, "level": true})
                .as_object()
                .unwrap()
                .clone()
        ]
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod a_shader_node_in_this_process {
    use super::*;
    use crate::core::compiler::compiler_ops::shader_node_island::place_each_shader_node_island_on_its_host;
    use crate::core::compiler::compiler_ops::shader_node_island_program::tests::shader_program;
    use crate::core::compiler::compiler_ops::shader_node_native_evaluator::ShaderNodeNativeEvaluator;
    use crate::core::descriptors::{
        NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind, PinDeclaration, PinType,
        PortDescriptor, PortFamily, PortFamilyDeclaration,
    };
    use crate::core::error::Error;
    use crate::core::nodes::{HowANodeOfADescribedTypeRuns, NodeSpec, NodeTypesOneStreamResolves};

    const TINT: &str = "test_shaders:tint";

    fn the_tint_shader_node_type() -> NodeDescriptor {
        let mut tint = NodeDescriptor::new(
            NodeClassShortName::new("tint").unwrap(),
            NodeClassImportPath::new(TINT).unwrap(),
            "a shader node type for a test",
        );
        tint.node_kind = NodeKind::Shader;
        tint.with_input(
            PortDescriptor::new(
                "image",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Image),
            )
            .with_delivery_profile("newest"),
        )
        .with_input(
            PortDescriptor::new(
                "amount",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )
            .with_delivery_profile("newest")
            .with_pin_declaration(PinDeclaration::exactly(PinType::Float { exact_size: None })),
        )
        .with_output(PortDescriptor::new(
            "result",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Image),
        ))
    }

    fn the_tint_program()
    -> crate::core::compiler::compiler_ops::shader_node_program::ShaderNodeProgram {
        shader_program(
            "tint",
            "return image.pixel * amount;",
            &["image"],
            &["amount"],
        )
    }

    /// Two `tint` shader nodes, the first fused into the island the second hosts, whose
    /// evaluator is built in this process as a running shader's is.
    pub(crate) fn a_shader_node_fused_into_the_island_another_hosts_and_evaluates()
    -> (RwLock<Graph>, NodeRunId, NodeRunId) {
        let node_types = NodeTypesOneStreamResolves::new();
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                the_tint_shader_node_type(),
                HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                    shader_node_program: the_tint_program(),
                    evaluator_of_the_island_it_hosts: Box::new(|node| {
                        Err(Error::Configuration(format!(
                            "`{}` builds no node in this test",
                            node.id
                        )))
                    }),
                },
            )
            .unwrap();
        crate::core::test_support::ensure_test_mocks_registered();
        let mut graph = Graph::new_resolving_node_types_through(Arc::new(node_types));
        let mut add_a_tint = || {
            graph
                .traversal_mut()
                .add_v(NodeSpec::new(
                    NodeClassImportPath::new(TINT).unwrap(),
                    Value::Null,
                ))
                .unwrap()
                .first()
                .unwrap()
                .id
                .clone()
        };
        let fused = add_a_tint();
        let host = add_a_tint();
        link(&mut graph, (&fused, "result"), (&host, "image"));
        place_each_shader_node_island_on_its_host(&mut graph, &[fused.clone(), host.clone()])
            .unwrap();
        let evaluator = ShaderNodeNativeEvaluator::for_the_node(
            graph.traversal().v(&host).first().unwrap(),
            the_tint_shader_node_type(),
            the_tint_program(),
        );
        graph
            .traversal_mut()
            .v(&host)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(NodeInstanceComponent(Arc::new(Mutex::new(
                NodeInstance::new(Box::new(evaluator)),
            ))));
        (RwLock::new(graph), fused, host)
    }

    pub(crate) fn the_value_the_host_reads(
        graph: &RwLock<Graph>,
        host: &NodeRunId,
        input_port: &str,
    ) -> f32 {
        let node_instance = Arc::clone(
            &graph
                .read()
                .traversal()
                .v(host)
                .first()
                .unwrap()
                .get::<NodeInstanceComponent>()
                .unwrap()
                .0,
        );
        let mut node_instance = node_instance.lock();
        node_instance
            .as_any_mut()
            .downcast_mut::<ShaderNodeNativeEvaluator>()
            .expect("the host runs the shader evaluator")
            .the_value_a_float_input_reads(input_port)
            .unwrap_or_else(|| panic!("the island reads `{input_port}`"))
    }

    /// The value a running shader node's input reads, and one fused into its island, is handed
    /// to the evaluator built in this process, at the input each is read at.
    ///
    /// Mental-revert: keying the fused node's value by its own input name hands the host's
    /// `amount` 0.25 and leaves the fused input at 0; handing the host only its own values
    /// leaves the fused input at 0.
    #[test]
    fn each_value_reaches_the_evaluator_of_the_running_island_at_the_input_it_is_read_at() {
        let (graph, fused, host) =
            a_shader_node_fused_into_the_island_another_hosts_and_evaluates();
        let fused_input =
            the_host_input_port_a_fused_shader_node_input_is_read_at(&host, &fused, "amount")
                .port_name()
                .to_string();
        assert_eq!(the_value_the_host_reads(&graph, &host, "amount"), 0.0);
        assert_eq!(the_value_the_host_reads(&graph, &host, &fused_input), 0.0);

        hold_live_and_deliver(&graph, &fused, "amount", json!(0.25));
        hold_live_and_deliver(&graph, &host, "amount", json!(0.75));

        assert_eq!(the_value_the_host_reads(&graph, &host, &fused_input), 0.25);
        assert_eq!(the_value_the_host_reads(&graph, &host, "amount"), 0.75);

        let host_uuid = the_uuid_of(&graph.read(), &host);
        graph
            .read()
            .live_input_values()
            .set(host_uuid, "amount", None, NodeValueLifetime::Kept);
        deliver_the_unlinked_input_values_of_each_node(&graph, std::slice::from_ref(&host))
            .unwrap();
        assert_eq!(
            the_value_the_host_reads(&graph, &host, "amount"),
            0.75,
            "an input no value reaches keeps the value it last read"
        );
    }
}
