// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The two ways between a family declaration and a pin type: the pin type a family declaration
//! states exactly, and the family a typed port serves.

use super::{PortFamily, PortFamilyDeclaration};
use crate::pin_type::{BuiltInPinTypeName, PinType, TexturePixelFormat};

impl PortFamilyDeclaration {
    /// The pin type this declaration states exactly: a Texture of the formats it states, for an
    /// Image port whose family follows nothing else. `None` for every other port: no other
    /// family declaration names one type.
    pub fn exact_pin_type(&self) -> Option<PinType> {
        let family_is_fixed = self.also_takes_families.is_empty()
            && self.family_from_config_key.is_none()
            && self.family_follows_input_port.is_none();
        if self.family != PortFamily::Image || !family_is_fixed {
            return None;
        }
        let pixel_formats = self
            .formats
            .iter()
            .map(|format| TexturePixelFormat::from_wire_name(format))
            .collect::<Option<Vec<_>>>()?;
        Some(PinType::Texture { pixel_formats })
    }
}

impl PortFamily {
    /// The family whose messages carry a pin of `pin_type`, which a typed port serves as its
    /// `family`. `None` for a type that takes no wire, a named type, whose bytes belong to no
    /// family, and a List or a bundle, which no family's message carries.
    pub fn carrying(pin_type: &PinType) -> Option<Self> {
        Self::carrying_a_pin_of(pin_type.built_in_name())
    }

    /// The family whose messages carry a pin of the built-in type `name`, as [`Self::carrying`]
    /// finds it.
    pub fn carrying_a_pin_of(name: BuiltInPinTypeName) -> Option<Self> {
        match name {
            BuiltInPinTypeName::Boolean
            | BuiltInPinTypeName::Integer
            | BuiltInPinTypeName::Float
            | BuiltInPinTypeName::Color
            | BuiltInPinTypeName::Vector
            | BuiltInPinTypeName::Rectangle
            | BuiltInPinTypeName::Transform
            | BuiltInPinTypeName::Audio => Some(Self::Channels),
            BuiltInPinTypeName::Texture => Some(Self::Image),
            BuiltInPinTypeName::EncodedVideo | BuiltInPinTypeName::EncodedAudio => {
                Some(Self::Packets)
            }
            BuiltInPinTypeName::Tensor => Some(Self::Array),
            BuiltInPinTypeName::String
            | BuiltInPinTypeName::Menu
            | BuiltInPinTypeName::Asset
            | BuiltInPinTypeName::Trigger => Some(Self::Records),
            BuiltInPinTypeName::ListOf
            | BuiltInPinTypeName::Bundle
            | BuiltInPinTypeName::Named
            | BuiltInPinTypeName::NodeReference
            | BuiltInPinTypeName::PinTypeChoice => None,
        }
    }

    /// Every built-in type this family's messages carry, in the order the contract lists them.
    pub fn the_pin_types_it_carries(self) -> Vec<BuiltInPinTypeName> {
        BuiltInPinTypeName::EVERY_NAME
            .into_iter()
            .filter(|name| Self::carrying_a_pin_of(*name) == Some(self))
            .collect()
    }
}
