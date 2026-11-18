// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

use super::{PinType, VectorComponentCount, built_in_pin_type};

/// The type as a refusal names it: its label, then what narrows it, as `Vector of 3`.
impl fmt::Display for PinType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = built_in_pin_type(self.built_in_name()).label;
        match self {
            Self::Integer {
                exact_size: Some(exact_size),
            } => write!(formatter, "{label} ({})", exact_size.variant_name()),
            Self::Float {
                exact_size: Some(exact_size),
            } => write!(formatter, "{label} ({})", exact_size.variant_name()),
            Self::Menu { option_names } => {
                write!(formatter, "{label} ({})", option_names.join(", "))
            }
            Self::Vector { component_count } => {
                let components = match component_count {
                    VectorComponentCount::Two => 2,
                    VectorComponentCount::Three => 3,
                    VectorComponentCount::Four => 4,
                };
                write!(formatter, "{label} of {components}")
            }
            Self::Texture { pixel_formats } if !pixel_formats.is_empty() => {
                let formats: Vec<_> = pixel_formats
                    .iter()
                    .map(|format| format.wire_name())
                    .collect();
                write!(formatter, "{label} ({})", formats.join(", "))
            }
            Self::EncodedVideo { codecs } | Self::EncodedAudio { codecs } if !codecs.is_empty() => {
                write!(formatter, "{label} ({})", codecs.join(", "))
            }
            Self::Asset { media } if !media.is_empty() => {
                let media: Vec<_> = media.iter().map(|media| media.name()).collect();
                write!(formatter, "{label} ({})", media.join(", "))
            }
            Self::ListOf { element_type } => write!(formatter, "{label} of {element_type}"),
            Self::Bundle { members } => {
                let members: Vec<_> = members
                    .iter()
                    .map(|member| format!("{}: {}", member.name, member.member_type))
                    .collect();
                write!(formatter, "{label} ({})", members.join(", "))
            }
            Self::Named { named_pin_type } => {
                write!(formatter, "{label} `{}`", named_pin_type.as_str())
            }
            _ => formatter.write_str(label),
        }
    }
}
