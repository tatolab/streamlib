// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The two ways between a family declaration and a pin type: the pin type a family declaration
//! states exactly, and the family a typed port serves.

use super::{PortFamily, PortFamilyDeclaration};
use crate::pin_type::{BuiltInPinTypeName, BundleMessage, PinType, TexturePixelFormat};

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
    /// The family whose messages carry a pin of the built-in type `pin_type` names, which a
    /// typed port serves as its `family`. `None` for a type that takes no wire, a named type,
    /// whose bytes belong to no family, and a List or a bundle, whose family follows what it
    /// holds: [`Self::carrying_a_whole_value_of`] finds the family of a whole bundle.
    pub fn carrying(pin_type: &PinType) -> Option<Self> {
        Self::carrying_a_pin_of(pin_type.built_in_name())
    }

    /// The family one message of which carries a whole value of `pin_type`: for a bundle, the
    /// family of the one message it travels as ([`PinType::the_message_a_bundle_travels_as`],
    /// [`BundleMessage::its_family`]); for any other type, the one [`Self::carrying`] finds.
    pub fn carrying_a_whole_value_of(pin_type: &PinType) -> Option<Self> {
        match pin_type.the_message_a_bundle_travels_as() {
            Some(travels_as) => travels_as.its_family(),
            None => Self::carrying(pin_type),
        }
    }

    /// The family a port of the type a pin type choice chooses travels on: the family one
    /// message of which carries a whole value of it, or Records for a named type, whose port is
    /// declared on Records and carries the bytes its class packs. `None` for a List and a type
    /// that takes no wire.
    pub fn a_chosen_pin_type_travels_on(pin_type: &PinType) -> Option<Self> {
        match pin_type {
            PinType::Named { .. } => Some(Self::Records),
            other => Self::carrying_a_whole_value_of(other),
        }
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
            | BuiltInPinTypeName::Connection
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

impl BundleMessage {
    /// The family this message of a whole bundle belongs to: Records for a map of members, Array
    /// for a frame of Tensors, and none for a Bundle frame, which only an input typed as the
    /// bundle takes apart.
    pub fn its_family(self) -> Option<PortFamily> {
        match self {
            Self::RecordsMap => Some(PortFamily::Records),
            Self::ArrayFrameOfTensors => Some(PortFamily::Array),
            Self::BundleFrame => None,
        }
    }
}
