// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use serde_json::Value;

use super::{
    ALinkConnected, ALinkConnectedOrSetAside, LiveGraphChangeOnOneStream,
    WhenTheEngineCannotWireALink, connect_impl,
};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration,
    PinType, PortDescriptor,
};
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::pubsub::LoadedStreamIdentity;
use crate::core::{Error, Result};

const STRING_PRODUCER: &str = "PinTypeCheckStringProducer";
const TENSOR_CONSUMER: &str = "PinTypeCheckTensorConsumer";
const MESSAGE_PASSER: &str = "PinTypeCheckMessagePasser";

fn class_path(class_name: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

fn register_a_class(class_name: &str, inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>) {
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(class_name).unwrap(),
        class_path(class_name),
        class_name,
    );
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    NODE_REGISTRY
        .register_descriptor_only(descriptor)
        .expect("register the fixture descriptor");
}

fn a_port_declaring(name: &str, pin_type: DeclaredPinType) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PinDeclaration {
            pin_type,
            ..PinDeclaration::exactly(PinType::String {})
        },
    )
}

fn register_the_fixture_classes() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register_a_class(
            STRING_PRODUCER,
            Vec::new(),
            vec![PortDescriptor::new(
                "label",
                "label",
                true,
                PinDeclaration::exactly(PinType::String {}),
            )],
        );
        register_a_class(
            TENSOR_CONSUMER,
            vec![
                PortDescriptor::new(
                    "tensor",
                    "tensor",
                    true,
                    PinDeclaration::exactly(PinType::Tensor {}),
                )
                .with_delivery_profile("newest"),
            ],
            Vec::new(),
        );
        register_a_class(
            MESSAGE_PASSER,
            vec![a_port_declaring("value", DeclaredPinType::Any).with_delivery_profile("newest")],
            vec![a_port_declaring(
                "result",
                DeclaredPinType::SameAsInput {
                    input_port_name: "value".into(),
                },
            )],
        );
    });
}

fn a_node_of(compiler: &Compiler, class_name: &str, config: Value) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(NodeSpec::new(class_path(class_name), config))
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    })
}

fn connect_on_this_thread(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(connect_impl(
            LiveGraphChangeOnOneStream {
                compiler: Arc::clone(compiler),
                live: None,
                stream: LoadedStreamIdentity {
                    runtime_id: "Rpintypecheck".to_string(),
                    stream_id: "main".to_string(),
                    stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
                },
                held_for_one_commit: None,
                one_input_value_change_at_a_time: Default::default(),
            },
            from,
            to,
            ALinkConnected::MadeNow,
            WhenTheEngineCannotWireALink::Refuse,
        ))
        .and_then(ALinkConnectedOrSetAside::connected)
}

fn the_links_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().e(()).iter().count())
}

#[test]
fn a_link_its_pin_types_refuse_is_refused_before_any_link_exists() {
    register_the_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, STRING_PRODUCER, Value::Null);
    let model = a_node_of(&compiler, TENSOR_CONSUMER, Value::Null);

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "label"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect_err("a String output never feeds a Tensor input");

    let Error::InvalidLink(refusal) = refusal else {
        panic!("a link of types that do not connect is an invalid link: {refusal}");
    };
    assert!(refusal.contains("String"), "{refusal}");
    assert!(refusal.contains("Tensor"), "{refusal}");
    assert_eq!(the_links_in(&compiler), 0, "the refused link is never added");
}

#[test]
fn an_output_following_an_input_is_checked_as_the_output_linked_into_that_input() {
    register_the_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, STRING_PRODUCER, Value::Null);
    let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let model = a_node_of(&compiler, TENSOR_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "label"),
        InputLinkPortRef::new(gate.clone(), "value"),
    )
    .expect("the passer's input takes any type");

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(gate, "result"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect_err("the passer carries the String on, which no Tensor input takes")
    .to_string();
    assert!(refusal.contains("String"), "{refusal}");
}

#[test]
fn a_link_into_an_input_an_output_follows_is_checked_against_what_that_output_feeds() {
    register_the_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, STRING_PRODUCER, Value::Null);
    let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let model = a_node_of(&compiler, TENSOR_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(gate.clone(), "result"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect("an output following an unlinked input is not typed yet");

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "label"),
        InputLinkPortRef::new(gate, "value"),
    )
    .expect_err("the String would be carried on into a Tensor input")
    .to_string();
    assert!(refusal.contains(".tensor"), "{refusal}");
}

#[test]
fn a_long_chain_of_outputs_following_inputs_is_checked_as_the_output_at_its_head() {
    register_the_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, STRING_PRODUCER, Value::Null);
    let model = a_node_of(&compiler, TENSOR_CONSUMER, Value::Null);
    let mut carried_on_from = OutputLinkPortRef::new(source, "label");
    for _ in 0..80 {
        let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
        connect_on_this_thread(
            &compiler,
            carried_on_from,
            InputLinkPortRef::new(gate.clone(), "value"),
        )
        .expect("each passer's input takes any type");
        carried_on_from = OutputLinkPortRef::new(gate, "result");
    }

    let refusal = connect_on_this_thread(
        &compiler,
        carried_on_from,
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect_err("the String reaches the end of the chain")
    .to_string();
    assert!(refusal.contains("String"), "{refusal}");
}

#[test]
fn a_link_into_a_chain_of_passers_is_checked_against_every_input_the_chain_feeds() {
    register_the_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let source = a_node_of(&compiler, STRING_PRODUCER, Value::Null);
    let first_gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let second_gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let model = a_node_of(&compiler, TENSOR_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(first_gate.clone(), "result"),
        InputLinkPortRef::new(second_gate.clone(), "value"),
    )
    .expect("two passers with nothing upstream are not typed yet");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(second_gate, "result"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect("a chain with nothing upstream is not typed yet");

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "label"),
        InputLinkPortRef::new(first_gate, "value"),
    )
    .expect_err("the String would reach the Tensor input through both passers")
    .to_string();
    assert!(refusal.contains(".tensor"), "{refusal}");
}
