// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::descriptors::PortDescriptor;
use crate::pin_type::{
    BundleMember, DeclaredPinType, PinDeclaration, PinFlow, PinType, TexturePixelFormat,
};

/// A member of a bundle named `name`, of `member_type`, with no meaning and no default.
fn a_member(name: &str, member_type: PinType) -> BundleMember {
    BundleMember {
        name: name.into(),
        member_type,
        meaning: None,
        default_value: None,
    }
}

fn image() -> PortFamilyDeclaration {
    PortFamilyDeclaration::of_family(PortFamily::Image)
}

#[test]
fn an_image_declaration_is_a_texture_of_the_formats_it_states() {
    assert_eq!(
        image().exact_pin_type(),
        Some(PinType::Texture {
            pixel_formats: vec![]
        })
    );
    let stating_formats = PortFamilyDeclaration {
        formats: vec!["bgra8_unorm".into(), "nv12".into()],
        ..image()
    };
    assert_eq!(
        stating_formats.exact_pin_type(),
        Some(PinType::Texture {
            pixel_formats: vec![TexturePixelFormat::Bgra8Unorm, TexturePixelFormat::Nv12]
        })
    );
}

#[test]
fn a_declaration_whose_family_is_not_fixed_or_not_image_states_no_pin_type() {
    let not_exact = [
        PortFamilyDeclaration {
            also_takes_families: vec![PortFamily::Array],
            ..image()
        },
        PortFamilyDeclaration {
            family_from_config_key: Some("family".into()),
            ..image()
        },
        PortFamilyDeclaration {
            family_follows_input_port: Some("value".into()),
            ..image()
        },
        PortFamilyDeclaration {
            formats: vec!["not_a_format".into()],
            ..image()
        },
        PortFamilyDeclaration::of_family(PortFamily::Channels),
        PortFamilyDeclaration::of_family(PortFamily::Records),
    ];
    for declaration in not_exact {
        assert_eq!(declaration.exact_pin_type(), None, "{declaration:?}");
    }
}

#[test]
fn a_typed_port_serves_the_family_its_messages_belong_to() {
    assert_eq!(
        PortFamily::carrying(&PinType::Float { exact_size: None }),
        Some(PortFamily::Channels)
    );
    assert_eq!(
        PortFamily::carrying(&PinType::Boolean {}),
        Some(PortFamily::Channels)
    );
    assert_eq!(
        PortFamily::carrying(&PinType::Texture {
            pixel_formats: vec![]
        }),
        Some(PortFamily::Image)
    );
    assert_eq!(
        PortFamily::carrying(&PinType::Tensor {}),
        Some(PortFamily::Array)
    );
    assert_eq!(
        PortFamily::carrying(&PinType::PinTypeChoice {
            among_the_bundles_its_stream_declares: false,
            among: vec![]
        }),
        None
    );
}

/// A bundle of numbers is carried whole on Records, as one map of its members, and a type
/// `carrying` gives a family keeps it.
///
/// Mental-revert: answering `carrying` alone gives a bundle of numbers no family, so a view's
/// port of a record of named numbers does not open.
#[test]
fn a_bundle_of_small_members_is_carried_whole_on_records() {
    let yaw_and_burst = PinType::Bundle {
        name: None,
        members: vec![
            a_member("yaw", PinType::Float { exact_size: None }),
            a_member("burst", PinType::Integer { exact_size: None }),
        ],
    };
    assert_eq!(PortFamily::carrying(&yaw_and_burst), None);
    assert_eq!(
        PortFamily::carrying_a_whole_value_of(&yaw_and_burst),
        Some(PortFamily::Records)
    );
    assert_eq!(
        PortFamily::carrying_a_whole_value_of(&PinType::Float { exact_size: None }),
        Some(PortFamily::Channels)
    );
}

/// A whole bundle is carried on the family of the one message it travels as: a bundle with a
/// List of numbers among its members is one Records map, a bundle of Tensors one Array frame,
/// and a bundle holding a Texture a Bundle frame, carried on no family.
///
/// Mental-revert: asking that every member be a small type finds no family for the bundle
/// with a List member, which the runtime packs as one Records map.
#[test]
fn a_whole_bundle_is_carried_on_the_family_of_the_message_it_travels_as() {
    let bundle = |members| PinType::Bundle {
        name: None,
        members,
    };
    let with_a_list = bundle(vec![
        a_member("label", PinType::String {}),
        a_member(
            "levels",
            PinType::ListOf {
                element_type: Box::new(PinType::Float { exact_size: None }),
            },
        ),
    ]);
    let of_tensors = bundle(vec![
        a_member("boxes", PinType::Tensor {}),
        a_member("scores", PinType::Tensor {}),
    ]);
    let with_a_texture = bundle(vec![
        a_member("label", PinType::String {}),
        a_member(
            "picture",
            PinType::Texture {
                pixel_formats: vec![],
            },
        ),
    ]);
    for (bundle, family) in [
        (&with_a_list, Some(PortFamily::Records)),
        (&of_tensors, Some(PortFamily::Array)),
        (&with_a_texture, None),
    ] {
        bundle.validate().expect("a bundle pin type");
        assert_eq!(
            PortFamily::carrying_a_whole_value_of(bundle),
            family,
            "{bundle}"
        );
    }
}

#[test]
fn a_port_declares_the_pin_its_author_states_else_a_texture_for_a_picture_and_else_none() {
    let picture = PortDescriptor::new("video", "", true, image());
    assert_eq!(
        picture.declared_pin(),
        Some(PinDeclaration {
            pin_type: DeclaredPinType::Is {
                pin_type: PinType::Texture {
                    pixel_formats: vec![]
                }
            },
            flow: PinFlow::Stream,
            annotations: Default::default(),
            former_names: Vec::new(),
            may_be_unset: false,
        })
    );

    let untyped = PortDescriptor::new(
        "audio",
        "",
        true,
        PortFamilyDeclaration::of_family(PortFamily::Channels),
    );
    assert_eq!(untyped.declared_pin(), None);

    let stated = PinDeclaration::exactly(PinType::Float { exact_size: None });
    assert_eq!(
        untyped.with_pin_declaration(stated.clone()).declared_pin(),
        Some(stated)
    );
}

#[test]
fn each_family_lists_the_built_in_types_its_messages_carry() {
    use crate::pin_type::BuiltInPinTypeName::{
        Asset, Audio, Boolean, Color, EncodedAudio, EncodedVideo, Float, Integer, Menu, Rectangle,
        String, Tensor, Texture, Transform, Trigger, Vector,
    };
    assert_eq!(PortFamily::Image.the_pin_types_it_carries(), [Texture]);
    assert_eq!(PortFamily::Array.the_pin_types_it_carries(), [Tensor]);
    assert_eq!(
        PortFamily::Packets.the_pin_types_it_carries(),
        [EncodedVideo, EncodedAudio]
    );
    assert_eq!(
        PortFamily::Channels.the_pin_types_it_carries(),
        [
            Boolean, Integer, Float, Color, Vector, Rectangle, Transform, Audio
        ]
    );
    assert_eq!(
        PortFamily::Records.the_pin_types_it_carries(),
        [String, Menu, Asset, Trigger]
    );
}
