// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The member of a bundle input a link ends at, put into the bundle the input reads as each of
//! its messages arrives. Every link ending at a member of one input is bound to that input's one
//! port, which keeps the newest value each has carried, so the reader takes one message built
//! from all of them, stamped and numbered as the message that arrived last.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use streamlib_ipc_types::DataChannelSampleUserHeader;

use super::MemberTakenAtTheRead;
use crate::core::descriptors::{BundleMember, PinType};
use crate::core::pin_value_packing::{
    PackedPinValueRef, pack_pin_value, the_records_message_of_members_of_a_bundle, unpack_pin_value,
};

/// What the members a port's links carry are built into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhatTheMembersArePutInto {
    /// The whole bundle, each member no link has carried yet read at its default. While a member
    /// declaring no default has not arrived, the port has no bundle to hand the reader.
    TheWholeBundle,
    /// The members the links have carried alone, which the island reading them puts over the
    /// input's own value, so each member no link carries reads what its value holds.
    TheMembersTheLinksCarried,
}

/// The member of the bundle `bundle` a link ends at, put into what the port reads as each of
/// its messages arrives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberPutAtTheRead {
    /// The bundle the link's input carries.
    pub bundle: PinType,
    /// The member the link ends at.
    pub member: BundleMember,
    /// What the members are built into.
    pub put_into: WhatTheMembersArePutInto,
}

impl MemberPutAtTheRead {
    /// The message the port hands its reader once `payload`, a message of the member whose
    /// header is `header`, has arrived, `newest` holding the newest value of each member its
    /// links have carried and taking this one: its header, stating the built payload, and its
    /// bytes; `None` while the whole bundle still lacks a member that declares no default. A
    /// whole bundle reads each member no link has carried from `set_on_the_input`, the value set
    /// on its input, when that holds it. Fails saying why the message carries no value of the
    /// member, or what it builds does not pack.
    pub fn the_message_built_with_the_member(
        &self,
        header: &DataChannelSampleUserHeader,
        payload: &[u8],
        newest: &mut Map<String, Value>,
        set_on_the_input: Option<&Map<String, Value>>,
    ) -> Result<Option<(DataChannelSampleUserHeader, Vec<u8>)>, String> {
        let value = unpack_pin_value(
            &self.member.member_type,
            PackedPinValueRef {
                payload_descriptor: &header.payload_descriptor,
                payload,
            },
        )?;
        newest.insert(self.member.name.clone(), value);
        let packed = match self.put_into {
            WhatTheMembersArePutInto::TheWholeBundle => {
                let whole =
                    self.the_members_set_with_the_newest_over_them(newest, set_on_the_input);
                if self.a_member_without_a_default_has_not_arrived(&whole) {
                    return Ok(None);
                }
                pack_pin_value(&self.bundle, &Value::Object(whole), 0)?
            }
            WhatTheMembersArePutInto::TheMembersTheLinksCarried => {
                the_records_message_of_members_of_a_bundle(&self.bundle, newest)?
            }
        };
        let payload_byte_len = u32::try_from(packed.payload.len()).map_err(|_| {
            format!(
                "the bundle built with member `{}` packs to {} bytes, past what one message \
                 states",
                self.member.name,
                packed.payload.len()
            )
        })?;
        Ok(Some((
            DataChannelSampleUserHeader {
                payload_byte_len,
                payload_descriptor: packed.payload_descriptor,
                ..*header
            },
            packed.payload,
        )))
    }

    /// Each member of the bundle `set_on_the_input` holds, with `newest`, what the links have
    /// carried, over them.
    fn the_members_set_with_the_newest_over_them(
        &self,
        newest: &Map<String, Value>,
        set_on_the_input: Option<&Map<String, Value>>,
    ) -> Map<String, Value> {
        let PinType::Bundle { members } = &self.bundle else {
            return newest.clone();
        };
        let mut whole: Map<String, Value> = set_on_the_input
            .into_iter()
            .flatten()
            .filter(|(name, value)| {
                !value.is_null() && members.iter().any(|member| &member.name == *name)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        whole.extend(
            newest
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        whole
    }

    fn a_member_without_a_default_has_not_arrived(&self, newest: &Map<String, Value>) -> bool {
        let PinType::Bundle { members } = &self.bundle else {
            return false;
        };
        members
            .iter()
            .any(|member| member.default_value.is_none() && !newest.contains_key(&member.name))
    }
}

/// The bundle members a link reads at its destination's port: the member of its output's bundle
/// it takes from each message, and the member of its input's bundle it puts that value into;
/// neither for a link between two whole ports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BundleMembersAtTheRead {
    /// The member of the output's bundle the link starts at.
    pub taken_from_its_output: Option<MemberTakenAtTheRead>,
    /// The member of the input's bundle the link ends at.
    pub put_into_its_input: Option<MemberPutAtTheRead>,
}

impl BundleMembersAtTheRead {
    /// Whether the link reads a member at either end.
    pub(super) fn reads_a_member(&self) -> bool {
        self.taken_from_its_output.is_some() || self.put_into_its_input.is_some()
    }

    /// The message the port hands its reader once `payload`, a message of the link's output
    /// whose header is `header`, has arrived: the member taken from it, put into what the port
    /// reads with `newest`, each member's newest value, taking it, over `set_on_the_input`, the
    /// value set on its input. `None` while that is a whole bundle still lacking a member that
    /// declares no default. Fails saying why the message gives no member the link reads.
    pub(super) fn the_message_the_port_reads(
        &self,
        header: &DataChannelSampleUserHeader,
        payload: &[u8],
        newest: &mut Map<String, Value>,
        set_on_the_input: Option<&Map<String, Value>>,
    ) -> Result<Option<(DataChannelSampleUserHeader, Vec<u8>)>, String> {
        let (header, payload) = match &self.taken_from_its_output {
            Some(taken) => taken.the_message_of_the_member(header, payload)?,
            None => (*header, payload.to_vec()),
        };
        match &self.put_into_its_input {
            Some(put) => {
                put.the_message_built_with_the_member(&header, &payload, newest, set_on_the_input)
            }
            None => Ok(Some((header, payload))),
        }
    }
}
