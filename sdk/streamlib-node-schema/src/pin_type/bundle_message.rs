// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one message a whole bundle travels as on a link, which the runtime's packing of a bundle
//! and every reader of a port's family both follow.

use super::PinType;

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

impl PinType {
    /// The one message a whole value of this bundle travels as; `None` for a type that is no
    /// bundle.
    pub fn the_message_a_bundle_travels_as(&self) -> Option<BundleMessage> {
        let Self::Bundle { members } = self else {
            return None;
        };
        Some(if self.holds_a_heavy_member() {
            BundleMessage::BundleFrame
        } else if !members.is_empty()
            && members
                .iter()
                .all(|member| matches!(member.member_type, Self::Tensor {}))
        {
            BundleMessage::ArrayFrameOfTensors
        } else {
            BundleMessage::RecordsMap
        })
    }
}
