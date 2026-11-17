// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::descriptors::PortDescriptor;
use crate::pin_type::{DeclaredPinType, PinDeclaration, PinFlow, PinType, TexturePixelFormat};

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
