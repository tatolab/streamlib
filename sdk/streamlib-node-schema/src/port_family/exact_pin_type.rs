// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The two ways between a family declaration and a pin type: the pin type a family declaration
//! states exactly, and the family a typed port serves.

use super::{PortFamily, PortFamilyDeclaration};
use crate::pin_type::{PinType, TexturePixelFormat};

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
        match pin_type {
            PinType::Boolean {}
            | PinType::Integer { .. }
            | PinType::Float { .. }
            | PinType::Color {}
            | PinType::Vector { .. }
            | PinType::Rectangle {}
            | PinType::Transform {}
            | PinType::Audio {} => Some(Self::Channels),
            PinType::Texture { .. } => Some(Self::Image),
            PinType::EncodedVideo { .. } | PinType::EncodedAudio { .. } => Some(Self::Packets),
            PinType::Tensor {} => Some(Self::Array),
            PinType::String {}
            | PinType::Menu { .. }
            | PinType::Asset { .. }
            | PinType::Trigger {} => Some(Self::Records),
            PinType::ListOf { .. }
            | PinType::Bundle { .. }
            | PinType::Named { .. }
            | PinType::NodeReference { .. }
            | PinType::PinTypeChoice { .. } => None,
        }
    }
}
