// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The packing of a bundle holding a Texture, Audio or an encoded unit as one Bundle frame. The
//! small members ride the frame's one Records message row, packed as a bundle of those members
//! alone packs, and each heavy member rides a row of its own under the descriptor it would have
//! on a wire of its own, its bytes beside it. A Texture's row names its pool slot and holds no
//! bytes. A frame is read as the input's own bundle: the small members it asks for, each one the
//! frame leaves out given its default, and a row for each heavy member it declares, so an output
//! holding more connects.

use serde_json::{Map, Value};
use streamlib_ipc_types::{
    BundleFrameDescriptorBody, BundleFrameRow, BundleFrameRowHolds, BundleFrameRowToPack,
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, bundle_frame_of_rows,
    bundle_frame_row_table_of,
};
use streamlib_node_schema::pin_type::BundleMember;

use super::bundle_packing::{
    the_bundle_a_records_message_carries, the_records_message_of_a_bundle,
};
use super::{PackedPinValue, PackedPinValueRef, message_kind_of};
use crate::core::descriptors::PinType;

/// One heavy member of a bundle in a Bundle frame: its name, the descriptor its bytes would
/// travel under on a wire of their own, and those bytes; none for a Texture, whose descriptor
/// names the pool slot it lives in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeavyMemberOfABundleFrame<'data> {
    /// The member's name.
    pub name: &'data str,
    /// The descriptor the member would travel under on a wire of its own.
    pub descriptor: DataChannelPayloadDescriptor,
    /// The member's bytes.
    pub payload: &'data [u8],
}

/// One heavy member of a bundle read off a Bundle frame: its name, the descriptor its row
/// states, and its bytes, borrowed from the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct HeavyMemberReadOffABundleFrame<'message> {
    /// The member's name.
    pub name: String,
    /// The descriptor the member's row states.
    pub descriptor: DataChannelPayloadDescriptor,
    /// The member's bytes.
    pub payload: &'message [u8],
}

/// A Bundle frame read as the bundle an input declares: its small members by name, and each of
/// its heavy members in the order the bundle declares them.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleFrameReadAsItsBundle<'message> {
    /// Each small member the input declares, by name.
    pub small_members: Map<String, Value>,
    /// Each heavy member the input declares, borrowed from the frame.
    pub heavy_members: Vec<HeavyMemberReadOffABundleFrame<'message>>,
}

/// The members of `bundle`, a bundle holding a heavy member, split into its small members, as
/// a bundle of their own, and its heavy members; refused for any other type.
fn small_and_heavy_members_of(bundle: &PinType) -> Result<(PinType, Vec<&BundleMember>), String> {
    let PinType::Bundle { members, .. } = bundle else {
        return Err(format!(
            "a {bundle} is no bundle, and a Bundle frame carries a bundle's members"
        ));
    };
    if !bundle.holds_a_heavy_member() {
        return Err(format!(
            "a {bundle} holds no Texture, Audio or encoded member, so it travels as the one \
             message its small members ride, and never as a Bundle frame"
        ));
    }
    let (heavy, small): (Vec<&BundleMember>, Vec<&BundleMember>) = members
        .iter()
        .partition(|member| member.member_type.is_a_heavy_type());
    Ok((
        PinType::Bundle {
            name: None,
            members: small.into_iter().cloned().collect(),
        },
        heavy,
    ))
}

/// The kind of message a heavy member of `member_type` travels as on a wire of its own.
fn kind_a_heavy_member_travels_as(member_type: &PinType) -> DataChannelPayloadDescriptorKind {
    message_kind_of(member_type).expect("a heavy type travels on a link")
}

/// Refuse `descriptor`, given for the heavy member `member`, when it describes another kind of
/// message than the member travels as on a wire of its own, naming the member and `where_it_is`.
pub(crate) fn refuse_a_heavy_member_message_of_another_kind(
    member: &BundleMember,
    descriptor: &DataChannelPayloadDescriptor,
    where_it_is: &str,
) -> Result<(), String> {
    let kind = kind_a_heavy_member_travels_as(&member.member_type);
    if descriptor.known_kind().ok() == Some(kind) {
        return Ok(());
    }
    Err(format!(
        "member `{}` is {}, which travels as {kind:?}, and {where_it_is} is described as {:?}",
        member.name,
        member.member_type,
        descriptor.known_kind()
    ))
}

/// `small_members`, the stored value of `bundle`'s small members by name, and `heavy_members`,
/// one for each of its heavy members, packed as one Bundle frame: the small members' Records
/// message row first, when the bundle has small members, then each heavy member's row in the
/// order the bundle declares them. Refused naming a heavy member given twice, left out, of the
/// wrong kind of message, or that `bundle` does not declare, and a small member its type does
/// not take.
pub fn the_bundle_frame_of_a_bundle(
    bundle: &PinType,
    small_members: &Value,
    heavy_members: &[HeavyMemberOfABundleFrame<'_>],
) -> Result<PackedPinValue, String> {
    let (small_bundle, declared_heavy_members) = small_and_heavy_members_of(bundle)?;
    if let Some(unknown) = heavy_members.iter().find(|given| {
        !declared_heavy_members
            .iter()
            .any(|member| member.name == given.name)
    }) {
        return Err(format!(
            "a {bundle} has no Texture, Audio or encoded member `{}`",
            unknown.name
        ));
    }
    let small_message = match &small_bundle {
        PinType::Bundle { members, .. } if members.is_empty() => None,
        _ => Some(the_records_message_of_a_bundle(
            &small_bundle,
            small_members,
        )?),
    };
    let member_holds: Vec<BundleFrameRowHolds> = declared_heavy_members
        .iter()
        .map(|member| BundleFrameRowHolds::Member(member.name.clone()))
        .collect();
    let mut rows = Vec::with_capacity(declared_heavy_members.len() + 1);
    if let Some(small_message) = &small_message {
        rows.push(BundleFrameRowToPack {
            holds: &BundleFrameRowHolds::SmallMembers,
            descriptor: small_message.payload_descriptor,
            payload: &small_message.payload,
        });
    }
    for (member, holds) in declared_heavy_members.iter().zip(&member_holds) {
        let mut given = heavy_members
            .iter()
            .filter(|given| given.name == member.name);
        let (Some(heavy_member), None) = (given.next(), given.next()) else {
            return Err(format!(
                "a {bundle} carries member `{}` once, and it was given {} times",
                member.name,
                heavy_members
                    .iter()
                    .filter(|given| given.name == member.name)
                    .count()
            ));
        };
        refuse_a_heavy_member_message_of_another_kind(
            member,
            &heavy_member.descriptor,
            "the message it was given",
        )?;
        rows.push(BundleFrameRowToPack {
            holds,
            descriptor: heavy_member.descriptor,
            payload: heavy_member.payload,
        });
    }
    let (payload_descriptor, payload) =
        bundle_frame_of_rows(&rows).map_err(|refusal| refusal.reason)?;
    Ok(PackedPinValue {
        payload_descriptor,
        payload,
    })
}

/// The rows of `message`, a Bundle frame of `bundle`: refused when it is no Bundle frame or
/// its row table does not read.
fn the_rows_of_a_bundle_frame(
    bundle: &PinType,
    message: PackedPinValueRef<'_>,
) -> Result<Vec<BundleFrameRow>, String> {
    if message.payload_descriptor.known_kind().ok()
        != Some(DataChannelPayloadDescriptorKind::BundleFrame)
    {
        return Err(format!(
            "a {bundle} travels as a Bundle frame, and the message is described as {:?}",
            message.payload_descriptor.known_kind()
        ));
    }
    bundle_frame_row_table_of(
        BundleFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body),
        message.payload,
    )
    .map_err(|refusal| refusal.reason)
}

/// The bytes `row` holds in `message`.
fn the_bytes_of_the_row<'message>(
    row: &BundleFrameRow,
    message: PackedPinValueRef<'message>,
) -> &'message [u8] {
    &message.payload[row.payload_byte_offset as usize
        ..(row.payload_byte_offset + row.payload_byte_len) as usize]
}

/// The small members of `small_bundle` the Records message row of `rows` carries, each one it
/// leaves out given its default, and all of them so when the frame has no such row.
fn the_small_members_a_bundle_frame_carries(
    small_bundle: &PinType,
    rows: &[BundleFrameRow],
    message: PackedPinValueRef<'_>,
) -> Result<Map<String, Value>, String> {
    if matches!(small_bundle, PinType::Bundle { members, .. } if members.is_empty()) {
        return Ok(Map::new());
    }
    let empty_records = rmp_serde::to_vec_named(&Map::new()).expect("an empty map encodes");
    let records_message_descriptor = DataChannelPayloadDescriptor::records_message();
    let records = match rows
        .iter()
        .find(|row| row.holds == BundleFrameRowHolds::SmallMembers)
    {
        Some(row) => PackedPinValueRef {
            payload_descriptor: &row.descriptor,
            payload: the_bytes_of_the_row(row, message),
        },
        None => PackedPinValueRef {
            payload_descriptor: &records_message_descriptor,
            payload: &empty_records,
        },
    };
    match the_bundle_a_records_message_carries(small_bundle, records)? {
        Value::Object(small_members) => Ok(small_members),
        _ => unreachable!("a bundle reads as an object of its members"),
    }
}

/// The heavy member `member` of `bundle` borrowed from its row of `rows`: refused when the frame
/// holds no row for it, or one of another kind of message.
fn the_heavy_member_a_bundle_frame_carries<'message>(
    bundle: &PinType,
    member: &BundleMember,
    rows: &[BundleFrameRow],
    message: PackedPinValueRef<'message>,
) -> Result<HeavyMemberReadOffABundleFrame<'message>, String> {
    let row = rows
        .iter()
        .find(|row| row.holds == BundleFrameRowHolds::Member(member.name.clone()))
        .ok_or_else(|| {
            format!(
                "the {bundle}'s Bundle frame holds no row for member `{}`",
                member.name
            )
        })?;
    refuse_a_heavy_member_message_of_another_kind(
        member,
        &row.descriptor,
        "its row in the Bundle frame",
    )?;
    Ok(HeavyMemberReadOffABundleFrame {
        name: member.name.clone(),
        descriptor: row.descriptor,
        payload: the_bytes_of_the_row(row, message),
    })
}

/// The Bundle frame `message` read as `bundle`, the bundle an input declares: its small members
/// read off the frame's Records message row as a bundle of those members reads them, and each
/// heavy member borrowed from its row. Refused when `message` is no Bundle frame, a heavy member
/// `bundle` declares has no row or one of another kind of message, or a small member is refused.
pub fn the_bundle_a_bundle_frame_carries<'message>(
    bundle: &PinType,
    message: PackedPinValueRef<'message>,
) -> Result<BundleFrameReadAsItsBundle<'message>, String> {
    let (small_bundle, declared_heavy_members) = small_and_heavy_members_of(bundle)?;
    let rows = the_rows_of_a_bundle_frame(bundle, message)?;
    let small_members = the_small_members_a_bundle_frame_carries(&small_bundle, &rows, message)?;
    let heavy_members = declared_heavy_members
        .iter()
        .map(|member| the_heavy_member_a_bundle_frame_carries(bundle, member, &rows, message))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(BundleFrameReadAsItsBundle {
        small_members,
        heavy_members,
    })
}

/// The message member `member` of `bundle` travels as on its own, taken from `message`, a Bundle
/// frame of the whole bundle: a heavy member's row under the descriptor it states, its bytes
/// copied, a Texture's naming its pool slot and holding none; a small member's value, or its
/// default, packed as its own type travels. Only the member's own row is read.
pub(super) fn the_member_a_bundle_frame_carries(
    bundle: &PinType,
    member: &BundleMember,
    message: PackedPinValueRef<'_>,
) -> Result<PackedPinValue, String> {
    let (small_bundle, _) = small_and_heavy_members_of(bundle)?;
    let rows = the_rows_of_a_bundle_frame(bundle, message)?;
    if member.member_type.is_a_heavy_type() {
        let heavy_member = the_heavy_member_a_bundle_frame_carries(bundle, member, &rows, message)?;
        return Ok(PackedPinValue {
            payload_descriptor: heavy_member.descriptor,
            payload: heavy_member.payload.to_vec(),
        });
    }
    let small_members = the_small_members_a_bundle_frame_carries(&small_bundle, &rows, message)?;
    let small_member = small_members.get(&member.name).ok_or_else(|| {
        format!(
            "a {bundle} has no member `{}` its Bundle frame carries",
            member.name
        )
    })?;
    super::pack_pin_value(&member.member_type, small_member, 0)
}
