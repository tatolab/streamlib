// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a setting choosing a pin type takes, and which bundle a choice among a stream's bundles
//! takes as one it offers.

use super::PinType;

/// The pin types a [`PinType::PinTypeChoice`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinTypesAChoiceTakes<'choice> {
    /// Any pin type.
    AnyPinType,
    /// Exactly one of these.
    OneOf(&'choice [PinType]),
    /// A bundle its stream's node types declare, as
    /// [`PinType::is_a_bundle_chosen_as_the_one_offered`] matches it to one of these; none
    /// while they are empty.
    OneOfTheBundlesItsStreamDeclares(&'choice [PinType]),
}

impl PinTypesAChoiceTakes<'_> {
    /// Whether the choice takes `chosen`.
    pub fn takes(&self, chosen: &PinType) -> bool {
        match self {
            Self::AnyPinType => true,
            Self::OneOf(pin_types) => pin_types.contains(chosen),
            Self::OneOfTheBundlesItsStreamDeclares(bundles) => bundles
                .iter()
                .any(|offered| chosen.is_a_bundle_chosen_as_the_one_offered(offered)),
        }
    }
}

impl PinType {
    /// A setting choosing any pin type.
    pub fn a_choice_of_any() -> Self {
        Self::PinTypeChoice {
            among: Vec::new(),
            among_the_bundles_its_stream_declares: false,
        }
    }

    /// A setting choosing one of the bundles its stream's node types declare, listing none
    /// until the runtime lists them for a placed node.
    pub fn a_choice_among_the_bundles_its_stream_declares() -> Self {
        Self::PinTypeChoice {
            among: Vec::new(),
            among_the_bundles_its_stream_declares: true,
        }
    }

    /// The pin types this takes when it is a [`PinType::PinTypeChoice`]; `None` for any other
    /// type.
    pub fn the_pin_types_a_choice_takes(&self) -> Option<PinTypesAChoiceTakes<'_>> {
        let Self::PinTypeChoice {
            among,
            among_the_bundles_its_stream_declares,
        } = self
        else {
            return None;
        };
        Some(
            match (among.as_slice(), among_the_bundles_its_stream_declares) {
                (bundles, true) => PinTypesAChoiceTakes::OneOfTheBundlesItsStreamDeclares(bundles),
                ([], false) => PinTypesAChoiceTakes::AnyPinType,
                (pin_types, false) => PinTypesAChoiceTakes::OneOf(pin_types),
            },
        )
    }

    /// Whether this and `other` are bundles of the same name whose members have the same names
    /// and pin types, in order; a member's default and meaning do not count.
    pub fn is_the_same_bundle_as(&self, other: &PinType) -> bool {
        matches!((self, other), (Self::Bundle { name, .. }, Self::Bundle { name: other_name, .. })
            if name == other_name && self.has_the_members_of_the_bundle(other))
    }

    /// Whether this, a chosen bundle, is `offered`: a bundle whose members have the same names
    /// and pin types, in order, and of the same name unless this names none, as a value saved
    /// before bundles were named holds.
    pub fn is_a_bundle_chosen_as_the_one_offered(&self, offered: &PinType) -> bool {
        matches!((self, offered), (Self::Bundle { name, .. }, Self::Bundle { name: offered_name, .. })
            if (name.is_none() || name == offered_name) && self.has_the_members_of_the_bundle(offered))
    }

    fn has_the_members_of_the_bundle(&self, other: &PinType) -> bool {
        let (
            Self::Bundle { members, .. },
            Self::Bundle {
                members: other_members,
                ..
            },
        ) = (self, other)
        else {
            return false;
        };
        members.len() == other_members.len()
            && members
                .iter()
                .zip(other_members)
                .all(|(member, other_member)| {
                    member.name == other_member.name
                        && member.member_type == other_member.member_type
                })
    }
}
