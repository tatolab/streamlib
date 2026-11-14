// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use serde_json::{Value, json};

use super::{LiveGraphChangeOnOneStream, connect_impl};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    OutputPortDeclaredNames, PortDescriptor, PortFamily, PortFamilyDeclaration,
    ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use crate::core::graph::{
    InputLinkPortRef, LinkUniqueId, OutputLinkPortRef, ProcessorUniqueId,
    refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use crate::core::pubsub::LoadedStreamIdentity;
use crate::core::{Error, Result};

const IMAGE_PRODUCER: &str = "FamilyCheckImageProducer";
const IMAGE_CONSUMER: &str = "FamilyCheckImageConsumer";
const ARRAY_CONSUMER: &str = "FamilyCheckArrayConsumer";
const CHANNELS_PRODUCER: &str = "FamilyCheckChannelsProducer";
const CHANNELS_CONSUMER: &str = "FamilyCheckChannelsConsumer";
const MESSAGE_PASSER: &str = "FamilyCheckMessagePasser";

fn class_path(class_name: &str) -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

fn register_a_class(class_name: &str, inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>) {
    let mut descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(class_name).unwrap(),
        class_path(class_name),
        class_name,
    );
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    PROCESSOR_REGISTRY
        .register_descriptor_only(descriptor)
        .expect("register the fixture descriptor");
}

fn an_input(name: &str, family: PortFamilyDeclaration) -> PortDescriptor {
    PortDescriptor::new(name, "input", true, family).with_delivery_profile("newest")
}

fn an_output(name: &str, family: PortFamilyDeclaration) -> PortDescriptor {
    PortDescriptor::new(name, "output", true, family)
}

fn register_the_family_fixture_classes() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        register_a_class(
            IMAGE_PRODUCER,
            Vec::new(),
            vec![an_output(
                "video",
                PortFamilyDeclaration {
                    formats: vec!["rgba16_float".into()],
                    ..PortFamilyDeclaration::of_family(PortFamily::Image)
                },
            )],
        );
        register_a_class(
            IMAGE_CONSUMER,
            vec![an_input(
                "video",
                PortFamilyDeclaration {
                    formats: vec!["rgba8_unorm".into()],
                    ..PortFamilyDeclaration::of_family(PortFamily::Image)
                },
            )],
            Vec::new(),
        );
        register_a_class(
            ARRAY_CONSUMER,
            vec![an_input(
                "tensor",
                PortFamilyDeclaration::of_family(PortFamily::Array),
            )],
            Vec::new(),
        );
        register_a_class(
            CHANNELS_PRODUCER,
            Vec::new(),
            vec![an_output(
                "signals",
                PortFamilyDeclaration {
                    names: Some(OutputPortDeclaredNames::FromConfig {
                        config_key: "channel_names".into(),
                    }),
                    ..PortFamilyDeclaration::of_family(PortFamily::Channels)
                },
            )],
        );
        register_a_class(
            MESSAGE_PASSER,
            vec![an_input(
                "value",
                PortFamilyDeclaration {
                    also_takes_families: vec![PortFamily::Array, PortFamily::Channels],
                    ..PortFamilyDeclaration::of_family(PortFamily::Image)
                },
            )],
            vec![an_output(
                "result",
                PortFamilyDeclaration {
                    family_follows_input_port: Some("value".into()),
                    ..PortFamilyDeclaration::of_family(PortFamily::Image)
                },
            )],
        );
        register_a_class(
            CHANNELS_CONSUMER,
            vec![an_input(
                "signals",
                PortFamilyDeclaration {
                    requires: vec!["rpm".into()],
                    ..PortFamilyDeclaration::of_family(PortFamily::Channels)
                },
            )],
            Vec::new(),
        );
    });
}

fn a_node_of(compiler: &Compiler, class_name: &str, config: Value) -> ProcessorUniqueId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(class_path(class_name), config))
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
                    runtime_id: "Rfamilycheck".to_string(),
                    stream_id: "main".to_string(),
                    stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
                },
            },
            from,
            to,
        ))
}

fn the_links_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().e(()).iter().count())
}

#[test]
fn a_family_mismatch_is_refused_before_any_link_exists() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let camera = a_node_of(&compiler, IMAGE_PRODUCER, Value::Null);
    let model = a_node_of(&compiler, ARRAY_CONSUMER, Value::Null);

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(camera, "video"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect_err("an Image output never feeds an Array input");

    let Error::InvalidLink(refusal) = refusal else {
        panic!("a family mismatch is an invalid link: {refusal}");
    };
    assert!(refusal.contains(".video (Image)"), "{refusal}");
    assert!(refusal.contains(".tensor (Array)"), "{refusal}");
    assert!(refusal.contains("ImageToArray"), "{refusal}");
    assert_eq!(
        the_links_in(&compiler),
        0,
        "the refused link is never added"
    );
}

#[test]
fn an_output_following_an_input_is_checked_as_the_output_linked_into_that_input() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let camera = a_node_of(&compiler, IMAGE_PRODUCER, Value::Null);
    let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let model = a_node_of(&compiler, ARRAY_CONSUMER, Value::Null);
    let compositor = a_node_of(&compiler, IMAGE_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(camera, "video"),
        InputLinkPortRef::new(gate.clone(), "value"),
    )
    .expect("the passer's input takes an Image");

    let into_an_array_input = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(gate.clone(), "result"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect_err("the passer carries the camera's Image on, which no Array input takes")
    .to_string();
    assert!(
        into_an_array_input.contains(".video (Image)"),
        "{into_an_array_input}"
    );

    let into_an_input_of_other_formats = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(gate, "result"),
        InputLinkPortRef::new(compositor, "video"),
    )
    .expect_err("the camera's rgba16_float never reaches an rgba8_unorm-only input")
    .to_string();
    assert!(
        into_an_input_of_other_formats.contains("rgba16_float"),
        "{into_an_input_of_other_formats}"
    );
}

#[test]
fn a_link_into_an_input_an_output_follows_is_checked_against_what_that_output_feeds() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let camera = a_node_of(&compiler, IMAGE_PRODUCER, Value::Null);
    let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let model = a_node_of(&compiler, ARRAY_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(gate.clone(), "result"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect("an output following an unlinked input has no family settled yet");

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(camera, "video"),
        InputLinkPortRef::new(gate, "value"),
    )
    .expect_err("the camera's Image would be carried on into an Array input")
    .to_string();
    assert!(refusal.contains("carries on what"), "{refusal}");
    assert!(refusal.contains(".tensor"), "{refusal}");
}

#[test]
fn image_formats_that_do_not_overlap_are_refused_at_connect() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let render = a_node_of(&compiler, IMAGE_PRODUCER, Value::Null);
    let compositor = a_node_of(&compiler, IMAGE_CONSUMER, Value::Null);

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(render, "video"),
        InputLinkPortRef::new(compositor, "video"),
    )
    .expect_err("rgba16_float never reaches an rgba8_unorm-only input")
    .to_string();

    assert!(
        refusal.contains("rgba16_float") && refusal.contains("rgba8_unorm"),
        "{refusal}"
    );
}

#[test]
fn names_from_config_satisfy_requires_and_a_live_update_dropping_one_is_refused() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let engine = a_node_of(
        &compiler,
        CHANNELS_PRODUCER,
        json!({ "channel_names": ["rpm", "torque"] }),
    );
    let gauge = a_node_of(&compiler, CHANNELS_CONSUMER, Value::Null);
    let unnamed_engine = a_node_of(&compiler, CHANNELS_PRODUCER, json!({}));

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(engine.clone(), "signals"),
        InputLinkPortRef::new(gauge.clone(), "signals"),
    )
    .expect("the names the config sets include rpm");
    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(unnamed_engine, "signals"),
        InputLinkPortRef::new(gauge, "signals"),
    )
    .expect_err("a config without the key resolves no names")
    .to_string();
    assert!(refusal.contains("channel_names"), "{refusal}");

    compiler.scope(|graph, _tx| {
        refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse(
            graph,
            &engine,
            &json!({ "channel_names": ["rpm"] }),
        )
        .expect("an update keeping rpm is taken");
        let refusal = refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse(
            graph,
            &engine,
            &json!({ "channel_names": ["torque"] }),
        )
        .expect_err("an update dropping rpm is refused")
        .to_string();
        assert!(refusal.contains("\"rpm\""), "{refusal}");
        assert!(
            refusal.contains("signals"),
            "the refusal names the link: {refusal}"
        );
    });
}

#[test]
fn a_long_chain_of_outputs_following_inputs_is_checked_as_the_output_at_its_head() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let camera = a_node_of(&compiler, IMAGE_PRODUCER, Value::Null);
    let model = a_node_of(&compiler, ARRAY_CONSUMER, Value::Null);
    let mut carried_on_from = OutputLinkPortRef::new(camera, "video");
    for _ in 0..80 {
        let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
        connect_on_this_thread(
            &compiler,
            carried_on_from,
            InputLinkPortRef::new(gate.clone(), "value"),
        )
        .expect("each passer's input takes an Image");
        carried_on_from = OutputLinkPortRef::new(gate, "result");
    }

    let refusal = connect_on_this_thread(
        &compiler,
        carried_on_from,
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect_err("the camera's Image reaches the end of the chain")
    .to_string();
    assert!(refusal.contains(".video (Image)"), "{refusal}");
}

#[test]
fn a_link_into_a_chain_of_passers_is_checked_against_every_input_the_chain_feeds() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let camera = a_node_of(&compiler, IMAGE_PRODUCER, Value::Null);
    let first_gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let second_gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let model = a_node_of(&compiler, ARRAY_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(first_gate.clone(), "result"),
        InputLinkPortRef::new(second_gate.clone(), "value"),
    )
    .expect("two passers with nothing upstream settle no family");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(second_gate, "result"),
        InputLinkPortRef::new(model, "tensor"),
    )
    .expect("a chain with nothing upstream settles no family");

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(camera, "video"),
        InputLinkPortRef::new(first_gate, "value"),
    )
    .expect_err("the camera's Image would reach the Array input through both passers")
    .to_string();
    assert!(refusal.contains(".tensor"), "{refusal}");
}

#[test]
fn a_live_update_is_checked_against_the_inputs_a_passer_carries_it_on_to() {
    register_the_family_fixture_classes();
    let compiler = Arc::new(Compiler::new());
    let engine = a_node_of(
        &compiler,
        CHANNELS_PRODUCER,
        json!({ "channel_names": ["rpm", "torque"] }),
    );
    let gate = a_node_of(&compiler, MESSAGE_PASSER, Value::Null);
    let gauge = a_node_of(&compiler, CHANNELS_CONSUMER, Value::Null);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(engine.clone(), "signals"),
        InputLinkPortRef::new(gate.clone(), "value"),
    )
    .expect("the passer takes Channels");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(gate, "result"),
        InputLinkPortRef::new(gauge, "signals"),
    )
    .expect("the engine's names include rpm");

    compiler.scope(|graph, _tx| {
        let refusal = refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse(
            graph,
            &engine,
            &json!({ "channel_names": ["torque"] }),
        )
        .expect_err("the gauge behind the passer requires rpm")
        .to_string();
        assert!(refusal.contains("\"rpm\""), "{refusal}");
    });
}
