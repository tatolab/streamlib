// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type's `family` and `codec` settings, which named the family and codec its output
//! carried, that became one `PinTypeChoice` setting choosing the output's type. A saved document
//! opens with the choice holding the type the two named: a module's named type for Records, and
//! encoded audio or video in its codec for Packets.

use serde_json::{Map, Value, json};

use super::NODE_INPUT_VALUES_KEY;

/// The family a node saved with no `family` carried.
const FAMILY_SAVED_BY_DEFAULT: &str = RECORDS_FAMILY;
/// The saved family of an output carrying Records messages.
const RECORDS_FAMILY: &str = "records";
/// The saved family of an output carrying Packets units.
const PACKETS_FAMILY: &str = "packets";

/// A node type's family and codec settings that became one pin type choice.
#[derive(Debug, Clone, Copy)]
pub(super) struct PinTypeChosenFromAFamilyAndCodec {
    /// The node type's path, after any rename.
    pub(super) node_type: &'static str,
    /// The setting that named the output's family, `records` or `packets`.
    pub(super) family_input: &'static str,
    /// The setting that named a Packets output's codec.
    pub(super) codec_input: &'static str,
    /// The pin type choice the two became.
    pub(super) choice_input: &'static str,
    /// The named type a Records output carries as the choice holds it.
    pub(super) records_named_pin_type: &'static str,
    /// The codecs the choice may hold encoded video in.
    pub(super) video_codecs: &'static [&'static str],
    /// The codecs the choice may hold encoded audio in.
    pub(super) audio_codecs: &'static [&'static str],
}

/// Each node type whose family and codec settings became one pin type choice.
pub(super) const PIN_TYPES_CHOSEN_FROM_A_FAMILY_AND_CODEC: &[PinTypeChosenFromAFamilyAndCodec] =
    &[PinTypeChosenFromAFamilyAndCodec {
        node_type: "tatolab.moq:MoqTrackReceive",
        family_input: "family",
        codec_input: "codec",
        choice_input: "content_type",
        records_named_pin_type: "tatolab.moq:MoqRecord",
        video_codecs: &["h264", "h265"],
        audio_codecs: &["opus"],
    }];

/// Turn the family and codec `node` holds, of a type `rows` name, into the pin type choice they
/// became, in place; whether it held either. A node already holding the choice keeps it, and
/// the two are dropped. A Packets family naming no codec, or one the choice may not hold, is
/// left as written, so the node is refused naming the two it no longer takes rather than
/// opening on a type it was never saved with.
pub(super) fn the_family_and_codec_of_a_node_chosen(
    node: &mut Map<String, Value>,
    rows: &[PinTypeChosenFromAFamilyAndCodec],
) -> bool {
    let Some(node_type) = node.get("type").and_then(Value::as_str) else {
        return false;
    };
    let Some(row) = rows.iter().find(|row| row.node_type == node_type) else {
        return false;
    };
    let Some(input_values) = node
        .get_mut(NODE_INPUT_VALUES_KEY)
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    if !input_values.contains_key(row.family_input) && !input_values.contains_key(row.codec_input) {
        return false;
    }
    if !input_values.contains_key(row.choice_input) {
        let family = input_values
            .get(row.family_input)
            .and_then(Value::as_str)
            .unwrap_or(FAMILY_SAVED_BY_DEFAULT);
        let codec = input_values
            .get(row.codec_input)
            .and_then(Value::as_str)
            .filter(|codec| !codec.is_empty());
        let Some(chosen) = the_type_chosen(row, family, codec) else {
            return false;
        };
        input_values.insert(row.choice_input.to_owned(), chosen);
    }
    input_values.remove(row.family_input);
    input_values.remove(row.codec_input);
    true
}

/// The pin type a node that saved `family` and `codec` carried on the output its choice types;
/// `None` for Packets of no codec or of one the choice may not hold.
fn the_type_chosen(
    row: &PinTypeChosenFromAFamilyAndCodec,
    family: &str,
    codec: Option<&str>,
) -> Option<Value> {
    match (family, codec) {
        (PACKETS_FAMILY, Some(codec)) if row.video_codecs.contains(&codec) => {
            Some(json!({"type": "encoded_video", "codecs": [codec]}))
        }
        (PACKETS_FAMILY, Some(codec)) if row.audio_codecs.contains(&codec) => {
            Some(json!({"type": "encoded_audio", "codecs": [codec]}))
        }
        (PACKETS_FAMILY, _) => None,
        _ => Some(json!({"type": "named", "named_pin_type": row.records_named_pin_type})),
    }
}

#[cfg(test)]
mod tests;
