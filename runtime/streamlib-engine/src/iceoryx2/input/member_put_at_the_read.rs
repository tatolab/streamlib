// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The member of a bundle input a link ends at, put into the bundle the input reads as each of
//! its messages arrives. Every link ending at a member of one input is bound to that input's one
//! port, which keeps the newest value each has carried, so the reader takes one message built
//! from all of them, stamped and numbered as the message that arrived last. A bundle holding a
//! Texture, Audio or an encoded member is built as one Bundle frame, each such member the newest
//! message its link carried, a Texture by the pool slot it names.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use streamlib_ipc_types::DataChannelSampleUserHeader;

use super::MemberTakenAtTheRead;
use crate::core::descriptors::{BundleMember, PinType};
use crate::core::pin_value_packing::{
    HeavyMemberOfABundleFrame, PackedPinValue, PackedPinValueRef, pack_pin_value,
    refuse_a_heavy_member_message_of_another_kind, the_bundle_frame_of_a_bundle,
    the_records_message_of_members_of_a_bundle, unpack_pin_value,
};

/// The newest of each member the links into one bundle input have carried: each small member's
/// value, and each Texture, Audio or encoded member's message as it travelled, by member name. A
/// newer message of a member replaces the one held, and a member whose link is removed is
/// forgotten. A Texture held here is its descriptor alone and claims nothing; its reader claims
/// the slot when it reads the bundle, as it would a Texture off a wire of its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NewestOfEachMemberPutAtTheRead {
    /// Each small member's newest value, by name.
    pub(crate) small_members: Map<String, Value>,
    /// Each Texture, Audio or encoded member's newest message, by name.
    pub(crate) heavy_members: BTreeMap<String, PackedPinValue>,
}

impl NewestOfEachMemberPutAtTheRead {
    /// Forget what the link into member `member` carried.
    pub(crate) fn forget_the_member(&mut self, member: &str) {
        self.small_members.remove(member);
        self.heavy_members.remove(member);
    }
}

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
    /// bytes; `None` while the whole bundle still lacks a small member that declares no default
    /// or a Texture, Audio or encoded member, which a default never stands in for. A
    /// whole bundle reads each member no link has carried from `set_on_the_input`, the value set
    /// on its input, when that holds it. Fails saying why the message carries no value of the
    /// member, or what it builds does not pack.
    pub(crate) fn the_message_built_with_the_member(
        &self,
        header: &DataChannelSampleUserHeader,
        payload: Cow<'_, [u8]>,
        newest: &mut NewestOfEachMemberPutAtTheRead,
        set_on_the_input: Option<&Map<String, Value>>,
    ) -> Result<Option<(DataChannelSampleUserHeader, Vec<u8>)>, String> {
        self.hold_the_members_message(header, payload, newest)?;
        let packed = match self.put_into {
            WhatTheMembersArePutInto::TheWholeBundle => {
                let small_members = self.the_small_members_set_with_the_newest_over_them(
                    &newest.small_members,
                    set_on_the_input,
                );
                if self.a_member_the_bundle_needs_has_not_arrived(&small_members, newest) {
                    return Ok(None);
                }
                self.the_whole_bundle_packed(small_members, newest)?
            }
            WhatTheMembersArePutInto::TheMembersTheLinksCarried => {
                the_records_message_of_members_of_a_bundle(&self.bundle, &newest.small_members)?
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

    /// Hold `payload`, the member's message whose header is `header`, in `newest` over the one
    /// held before: a small member's value, or a heavy member's message as it travelled, its
    /// bytes moved in when owned, else copied into the buffer held before. Fails saying why the
    /// message carries no value of the member, holding nothing new.
    fn hold_the_members_message(
        &self,
        header: &DataChannelSampleUserHeader,
        payload: Cow<'_, [u8]>,
        newest: &mut NewestOfEachMemberPutAtTheRead,
    ) -> Result<(), String> {
        let member_type = &self.member.member_type;
        if !member_type.is_a_heavy_type() {
            let value = unpack_pin_value(
                member_type,
                PackedPinValueRef {
                    payload_descriptor: &header.payload_descriptor,
                    payload: &payload,
                },
            )?;
            newest.small_members.insert(self.member.name.clone(), value);
            return Ok(());
        }
        refuse_a_heavy_member_message_of_another_kind(
            &self.member,
            &header.payload_descriptor,
            "the message",
        )?;
        match (newest.heavy_members.get_mut(&self.member.name), payload) {
            (Some(held), Cow::Borrowed(bytes)) => {
                held.payload_descriptor = header.payload_descriptor;
                held.payload.clear();
                held.payload.extend_from_slice(bytes);
            }
            (_, payload) => {
                newest.heavy_members.insert(
                    self.member.name.clone(),
                    PackedPinValue {
                        payload_descriptor: header.payload_descriptor,
                        payload: payload.into_owned(),
                    },
                );
            }
        }
        Ok(())
    }

    /// The whole bundle of `small_members` and the heavy members `newest` holds, packed as the
    /// bundle travels: one Bundle frame for a bundle holding a heavy member, else one message of
    /// its small members.
    fn the_whole_bundle_packed(
        &self,
        small_members: Map<String, Value>,
        newest: &NewestOfEachMemberPutAtTheRead,
    ) -> Result<PackedPinValue, String> {
        if !self.bundle.holds_a_heavy_member() {
            return pack_pin_value(&self.bundle, &Value::Object(small_members), 0);
        }
        let heavy_members: Vec<HeavyMemberOfABundleFrame<'_>> = newest
            .heavy_members
            .iter()
            .map(|(name, held)| HeavyMemberOfABundleFrame {
                name,
                descriptor: held.payload_descriptor,
                payload: &held.payload,
            })
            .collect();
        the_bundle_frame_of_a_bundle(&self.bundle, &Value::Object(small_members), &heavy_members)
    }

    /// Each small member of the bundle `set_on_the_input` holds, with `newest`, what the links
    /// have carried, over them.
    fn the_small_members_set_with_the_newest_over_them(
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
                !value.is_null()
                    && members.iter().any(|member| {
                        &member.name == *name && !member.member_type.is_a_heavy_type()
                    })
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

    fn a_member_the_bundle_needs_has_not_arrived(
        &self,
        small_members: &Map<String, Value>,
        newest: &NewestOfEachMemberPutAtTheRead,
    ) -> bool {
        let PinType::Bundle { members } = &self.bundle else {
            return false;
        };
        members.iter().any(|member| {
            if member.member_type.is_a_heavy_type() {
                return !newest.heavy_members.contains_key(&member.name);
            }
            member.default_value.is_none() && !small_members.contains_key(&member.name)
        })
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
    /// declares no default, or a heavy member. Fails saying why the message gives no member the
    /// link reads.
    pub(super) fn the_message_the_port_reads(
        &self,
        header: &DataChannelSampleUserHeader,
        payload: &[u8],
        newest: &mut NewestOfEachMemberPutAtTheRead,
        set_on_the_input: Option<&Map<String, Value>>,
    ) -> Result<Option<(DataChannelSampleUserHeader, Vec<u8>)>, String> {
        let (header, payload) = match &self.taken_from_its_output {
            Some(taken) => {
                let (header, payload) = taken.the_message_of_the_member(header, payload)?;
                (header, Cow::Owned(payload))
            }
            None => (*header, Cow::Borrowed(payload)),
        };
        match &self.put_into_its_input {
            Some(put) => {
                put.the_message_built_with_the_member(&header, payload, newest, set_on_the_input)
            }
            None => Ok(Some((header, payload.into_owned()))),
        }
    }
}
