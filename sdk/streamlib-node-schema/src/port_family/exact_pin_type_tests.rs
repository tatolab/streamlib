// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::descriptors::PortDescriptor;
use crate::pin_type::{
    BundleMember, DeclaredPinType, PinDeclaration, PinFlow, PinType, TexturePixelFormat,
};

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
        PortFamily::carrying(&PinType::PinTypeChoice { among: vec![] }),
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
    let member = |name: &str, member_type| BundleMember {
        name: name.into(),
        member_type,
        meaning: None,
        default_value: None,
    };
    let yaw_and_burst = PinType::Bundle {
        members: vec![
            member("yaw", PinType::Float { exact_size: None }),
            member("burst", PinType::Integer { exact_size: None }),
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

/// A bundle holding a heavy member travels as a Bundle frame, and one holding a Tensor or a
/// bundle as anything but one Records map, so neither is carried whole on a family.
///
/// Mental-revert: calling any bundle Records names a family for a Texture's Bundle frame,
/// which no Records reader takes.
#[test]
fn a_bundle_with_a_member_that_is_not_small_is_carried_whole_on_no_family() {
    let member = |name: &str, member_type| BundleMember {
        name: name.into(),
        member_type,
        meaning: None,
        default_value: None,
    };
    for not_small in [
        PinType::Texture {
            pixel_formats: vec![],
        },
        PinType::Tensor {},
    ] {
        let bundle = PinType::Bundle {
            members: vec![
                member("label", PinType::String {}),
                member("payload", not_small),
            ],
        };
        assert_eq!(
            PortFamily::carrying_a_whole_value_of(&bundle),
            None,
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
