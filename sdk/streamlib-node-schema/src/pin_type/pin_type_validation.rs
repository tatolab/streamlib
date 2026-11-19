// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Which pin types a port may declare. A List element and a bundle member are a small type,
//! the values that ride a message that exists; a List element may also be a bundle of small
//! types, a bundle member a List of them, and a bundle may be all Tensors. A single pin holds a whole number exact to 2^53
//! at most, so a 64-bit Integer types only an element.

use super::{BundleMember, IntegerSize, PinType};

impl PinType {
    /// Refuse a type no port may declare, saying why: a List of a List, a List element or a
    /// bundle member that is no small type in a bundle that is not all Tensors, a bundle inside
    /// a bundle, a menu with no option,
    /// and an `Int64` or `UInt64` Integer that is no element.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Integer {
                exact_size: Some(exact_size @ (IntegerSize::Int64 | IntegerSize::UInt64)),
            } => Err(format!(
                "an {} Integer types only a List or Tensor element, as a single pin holds a \
                 whole number exact to 2^53 at most",
                exact_size.variant_name()
            )),
            Self::ListOf { element_type } => validate_a_list_element(element_type),
            Self::Bundle { members }
                if !members.is_empty()
                    && members
                        .iter()
                        .all(|member| matches!(member.member_type, PinType::Tensor {})) =>
            {
                Ok(())
            }
            Self::Bundle { members } => members.iter().try_for_each(validate_a_bundle_member),
            Self::PinTypeChoice { among } => among.iter().try_for_each(PinType::validate),
            Self::Menu { option_names } if option_names.is_empty() => {
                Err("a Menu lists at least one option".to_owned())
            }
            _ => Ok(()),
        }
    }

    /// Whether this is a small type, a value that rides a message that exists: Boolean,
    /// Integer, Float, String, Menu, Color, Vector, Rectangle, Transform or Asset.
    pub fn is_a_small_type(&self) -> bool {
        matches!(
            self,
            Self::Boolean {}
                | Self::Integer { .. }
                | Self::Float { .. }
                | Self::String {}
                | Self::Menu { .. }
                | Self::Color {}
                | Self::Vector { .. }
                | Self::Rectangle {}
                | Self::Transform {}
                | Self::Asset { .. }
        )
    }
}

fn validate_a_list_element(element_type: &PinType) -> Result<(), String> {
    match element_type {
        PinType::ListOf { .. } => Err(format!(
            "a List of a List is not a pin type; its element is {element_type}"
        )),
        PinType::Integer { .. } => Ok(()),
        PinType::Bundle { members }
            if members
                .iter()
                .all(|member| member.member_type.is_a_small_type()) =>
        {
            members.iter().try_for_each(validate_a_bundle_member)
        }
        element_type if element_type.is_a_small_type() => element_type.validate(),
        element_type => Err(format!(
            "a List element is a small type, or a bundle of them, and {element_type} is neither"
        )),
    }
}

fn validate_a_bundle_member(member: &BundleMember) -> Result<(), String> {
    let member_type = &member.member_type;
    let taken = match member_type {
        PinType::ListOf { element_type } => element_type.is_a_small_type(),
        member_type => member_type.is_a_small_type(),
    };
    if !taken {
        return Err(format!(
            "bundle member `{}` is {member_type}, and a member is a small type or a List of \
             them, unless every member is a Tensor",
            member.name
        ));
    }
    match member_type {
        PinType::ListOf { element_type } => validate_a_list_element(element_type),
        member_type => member_type.validate(),
    }
}
