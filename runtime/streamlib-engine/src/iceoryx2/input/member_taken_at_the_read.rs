// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The member of a bundle a link starts at, taken from each message of the bundle as it reaches
//! the input. A member link subscribes to the bundle's one channel, as any link from the port
//! does, so no second channel and no hidden node exist; its reader sees the member's own message.

use serde::{Deserialize, Serialize};
use streamlib_ipc_types::DataChannelSampleUserHeader;

use crate::core::descriptors::{BundleMember, PinType};
use crate::core::pin_value_packing::{PackedPinValueRef, the_member_a_bundle_message_carries};

/// The member of the bundle `bundle` a link reads, taken from each message as it arrives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberTakenAtTheRead {
    /// The bundle the link's output carries.
    pub bundle: PinType,
    /// The member the link reads.
    pub member: BundleMember,
}

impl MemberTakenAtTheRead {
    /// The member `member` of `bundle` taken at the read; `None` when `bundle` is no bundle
    /// holding it.
    pub fn of(bundle: &PinType, member: &str) -> Option<Self> {
        let PinType::Bundle { members, .. } = bundle else {
            return None;
        };
        let member = members.iter().find(|held| held.name == member)?.clone();
        Some(Self {
            bundle: bundle.clone(),
            member,
        })
    }

    /// The member's own message, taken from `payload`, a message of the whole bundle whose header
    /// is `header`: its header, stating the member's payload, and its bytes. The message keeps
    /// the bundle's stamp and numbering.
    pub fn the_message_of_the_member(
        &self,
        header: &DataChannelSampleUserHeader,
        payload: &[u8],
    ) -> Result<(DataChannelSampleUserHeader, Vec<u8>), String> {
        let packed = the_member_a_bundle_message_carries(
            &self.bundle,
            &self.member,
            PackedPinValueRef {
                payload_descriptor: &header.payload_descriptor,
                payload,
            },
        )?;
        let payload_byte_len = u32::try_from(packed.payload.len()).map_err(|_| {
            format!(
                "member `{}` packs to {} bytes, past what one message states",
                self.member.name,
                packed.payload.len()
            )
        })?;
        Ok((
            DataChannelSampleUserHeader {
                payload_byte_len,
                payload_descriptor: packed.payload_descriptor,
                ..*header
            },
            packed.payload,
        ))
    }
}
