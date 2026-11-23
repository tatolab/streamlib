// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one message a whole bundle travels as on a link, which the runtime's packing of a bundle
//! and every reader of a port's family both follow.

use super::{BundleMember, PinType};

/// The one message a whole bundle's value travels as on a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleMessage {
    /// A Records message whose map holds each member by name: a bundle of small members, or of
    /// Lists of them.
    RecordsMap,
    /// An Array frame with an attribute named for each member: a bundle of Tensors.
    ArrayFrameOfTensors,
    /// A Bundle frame holding each heavy member by reference: a bundle holding a Texture, Audio
    /// or an encoded unit.
    BundleFrame,
}

impl BundleMessage {
    /// The one message a whole value of a bundle of `members` travels as.
    pub fn of_members(members: &[BundleMember]) -> Self {
        if members
            .iter()
            .any(|member| member.member_type.is_a_heavy_type())
        {
            Self::BundleFrame
        } else if is_a_bundle_of_only_tensors(members) {
            Self::ArrayFrameOfTensors
        } else {
            Self::RecordsMap
        }
    }
}

impl PinType {
    /// The one message a whole value of this bundle travels as; `None` for a type that is no
    /// bundle.
    pub fn the_message_a_bundle_travels_as(&self) -> Option<BundleMessage> {
        match self {
            Self::Bundle { members } => Some(BundleMessage::of_members(members)),
            _ => None,
        }
    }
}

/// Whether `members` are one or more members, every one a Tensor: a bundle that travels as an
/// Array frame and that takes no other kind of member.
pub(super) fn is_a_bundle_of_only_tensors(members: &[BundleMember]) -> bool {
    !members.is_empty()
        && members
            .iter()
            .all(|member| matches!(member.member_type, PinType::Tensor {}))
}
