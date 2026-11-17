// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::descriptors::{NodeDescriptor, PortDescriptor};
use crate::{NodeClassImportPath, NodeClassShortName, PortFamily, PortFamilyDeclaration};

fn texture() -> PinType {
    PinType::Texture {
        pixel_formats: vec![],
    }
}

fn float() -> PinType {
    PinType::Float { exact_size: None }
}

fn bundle_of(member_types: Vec<PinType>) -> PinType {
    PinType::Bundle {
        members: member_types
            .into_iter()
            .enumerate()
            .map(|(index, member_type)| BundleMember {
                name: format!("member_{index}"),
                member_type,
                meaning: None,
                default_value: None,
            })
            .collect(),
    }
}

fn declared_with_flow(pin_type: PinType, flow: PinFlow) -> PinDeclaration {
    PinDeclaration {
        flow,
        ..PinDeclaration::exactly(pin_type)
    }
}

#[test]
fn a_list_or_bundle_takes_the_flows_of_its_member_that_allows_least() {
    let list_of = |element_type| PinType::ListOf {
        element_type: Box::new(element_type),
    };
    let value_then_stream: &[PinFlow] = &[PinFlow::Value, PinFlow::Stream];
    let stream_then_value: &[PinFlow] = &[PinFlow::Stream, PinFlow::Value];
    let stream_only: &[PinFlow] = &[PinFlow::Stream];

    assert_eq!(list_of(float()).flows(), value_then_stream);
    assert_eq!(
        bundle_of(vec![float(), PinType::Color {}]).flows(),
        value_then_stream
    );
    assert_eq!(list_of(texture()).flows(), stream_then_value);
    assert_eq!(
        bundle_of(vec![float(), PinType::Tensor {}]).flows(),
        stream_then_value
    );
    assert_eq!(
        bundle_of(vec![texture(), PinType::Audio {}, float()]).flows(),
        stream_only
    );
    assert_eq!(
        list_of(PinType::EncodedVideo { codecs: vec![] }).flows(),
        stream_only
    );
}

#[test]
fn a_stream_input_reads_newest_only_for_a_picture_a_tensor_or_a_bundle_of_them() {
    let newest = [
        texture(),
        PinType::Tensor {},
        bundle_of(vec![texture(), PinType::Tensor {}]),
    ];
    for pin_type in newest {
        assert_eq!(
            pin_type.default_stream_delivery_profile(),
            "newest",
            "{pin_type:?}"
        );
    }
    let ordered = [
        float(),
        PinType::Boolean {},
        PinType::Audio {},
        PinType::EncodedAudio { codecs: vec![] },
        PinType::Named {
            named_pin_type: NamedPinTypeImportPath::new("module:Detections").unwrap(),
        },
        PinType::Trigger {},
        bundle_of(vec![float(), PinType::String {}]),
        bundle_of(vec![texture(), PinType::Audio {}]),
    ];
    for pin_type in ordered {
        assert_eq!(
            pin_type.default_stream_delivery_profile(),
            "ordered",
            "{pin_type:?}"
        );
    }
}

#[test]
fn an_inputs_default_delivery_follows_its_flow_then_its_type() {
    assert_eq!(
        PinDeclaration::exactly(float()).default_input_delivery_profile(),
        Some("newest")
    );
    assert_eq!(
        declared_with_flow(float(), PinFlow::Stream).default_input_delivery_profile(),
        Some("ordered")
    );
    assert_eq!(
        PinDeclaration::exactly(texture()).default_input_delivery_profile(),
        Some("newest")
    );
    assert_eq!(
        PinDeclaration::exactly(PinType::Audio {}).default_input_delivery_profile(),
        Some("ordered")
    );
    assert_eq!(
        PinDeclaration::exactly(PinType::Trigger {}).default_input_delivery_profile(),
        Some("ordered")
    );
    let one_of_two_streams = PinDeclaration {
        pin_type: DeclaredPinType::OneOf {
            pin_types: vec![texture(), PinType::Tensor {}],
        },
        ..PinDeclaration::exactly(texture())
    };
    assert_eq!(one_of_two_streams.default_input_delivery_profile(), None);
}

#[test]
fn a_flow_its_type_allows_is_taken_and_any_other_is_refused_by_name() {
    assert_eq!(
        declared_with_flow(texture(), PinFlow::Value).refusal_of_its_flow("still"),
        None
    );
    assert_eq!(
        declared_with_flow(float(), PinFlow::Stream).refusal_of_its_flow("level"),
        None
    );
    assert_eq!(
        declared_with_flow(PinType::Audio {}, PinFlow::Value)
            .refusal_of_its_flow("audio")
            .as_deref(),
        Some(
            "port `audio` states the flow `value`, which the Audio type does not allow; it flows \
             as `stream`"
        )
    );
    assert_eq!(
        declared_with_flow(float(), PinFlow::Trigger)
            .refusal_of_its_flow("level")
            .as_deref(),
        Some(
            "port `level` states the flow `trigger`, which the Float type does not allow; it \
             flows as `value` or `stream`"
        )
    );
    let one_of_a_picture_or_sound = PinDeclaration {
        pin_type: DeclaredPinType::OneOf {
            pin_types: vec![texture(), PinType::Audio {}],
        },
        flow: PinFlow::Value,
        ..PinDeclaration::exactly(texture())
    };
    assert!(
        one_of_a_picture_or_sound
            .refusal_of_its_flow("source")
            .is_some_and(|refusal| refusal.contains("the Audio type"))
    );
}

fn a_node_type_with(inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>) -> NodeDescriptor {
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Typed").unwrap(),
        NodeClassImportPath::new("tests.typed:Typed").unwrap(),
        "",
    );
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    descriptor
}

fn a_port(name: &str, family: PortFamily) -> PortDescriptor {
    PortDescriptor::new(name, "", true, PortFamilyDeclaration::of_family(family))
}

#[test]
fn a_typed_input_that_states_no_delivery_reads_by_its_pins_default_and_a_stated_one_wins() {
    let descriptor = a_node_type_with(
        vec![
            a_port("picture", PortFamily::Image),
            a_port("level", PortFamily::Channels)
                .with_pin_declaration(PinDeclaration::exactly(float())),
            a_port("levels", PortFamily::Channels)
                .with_pin_declaration(declared_with_flow(float(), PinFlow::Stream)),
            a_port("every_picture", PortFamily::Image).with_delivery_profile("ordered"),
            a_port("records", PortFamily::Records).with_delivery_profile("newest"),
        ],
        vec![a_port("out", PortFamily::Image)],
    )
    .with_input_deliveries_defaulted_by_pin()
    .unwrap();

    let deliveries: Vec<(&str, Option<&str>)> = descriptor
        .inputs
        .iter()
        .map(|port| (port.name.as_str(), port.delivery_profile.as_deref()))
        .collect();
    assert_eq!(
        deliveries,
        [
            ("picture", Some("newest")),
            ("level", Some("newest")),
            ("levels", Some("ordered")),
            ("every_picture", Some("ordered")),
            ("records", Some("newest")),
        ]
    );
    assert_eq!(descriptor.outputs[0].delivery_profile, None);
}

#[test]
fn a_node_type_is_refused_a_flow_its_type_does_not_take_and_keeps_an_untyped_input_as_stated() {
    let audio_read_as_a_value = a_node_type_with(
        vec![],
        vec![
            a_port("audio", PortFamily::Channels)
                .with_pin_declaration(declared_with_flow(PinType::Audio {}, PinFlow::Value)),
        ],
    );
    assert_eq!(
        audio_read_as_a_value
            .with_input_deliveries_defaulted_by_pin()
            .unwrap_err(),
        "`tests.typed:Typed`: port `audio` states the flow `value`, which the Audio type does \
         not allow; it flows as `stream`"
    );

    let untyped_with_no_delivery =
        a_node_type_with(vec![a_port("records", PortFamily::Records)], vec![])
            .with_input_deliveries_defaulted_by_pin()
            .unwrap();
    assert_eq!(untyped_with_no_delivery.inputs[0].delivery_profile, None);
}

#[test]
fn a_windowed_input_whose_pins_default_skips_messages_is_refused_by_name() {
    let windowed_value = a_node_type_with(
        vec![
            a_port("level", PortFamily::Channels)
                .with_pin_declaration(PinDeclaration::exactly(float()))
                .with_audio_window_contract(crate::AudioWindowContract::MatchDevice {}),
        ],
        vec![],
    );
    let refusal = windowed_value
        .with_input_deliveries_defaulted_by_pin()
        .unwrap_err();
    assert!(
        refusal.starts_with(
            "`tests.typed:Typed`: input port `level` reads by its pin's default delivery \
             profile, `newest`: "
        ),
        "{refusal}"
    );
}
