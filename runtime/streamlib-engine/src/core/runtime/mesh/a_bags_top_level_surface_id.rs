// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface a bag names at its top level, and the bag that names another.
//!
//! The one key the engine reads inside a bag on the way across the mesh, and it
//! reads no other: a surface id names a frame in this machine's own pools, so a
//! runtime on another machine cannot resolve it. §Processor model states the
//! carve-out; §Networking states what happens to the frame — the sender copies
//! its pixels out, and the receiver mints a local surface and hands the bag on
//! naming that one instead.
//!
//! The walk decodes the top-level map's keys and nothing else — it steps over
//! every value rather than building it, so a bag carrying megabytes of pixels
//! costs a pointer walk over the lengths and no allocation at all. The rewrite
//! splices over the id's own bytes for the same reason: every other key, every
//! other value and their order are the producer's, carried across untouched
//! rather than decoded and re-encoded into whatever this engine would have
//! written.

use std::ops::Range;

use rmp::Marker;

use crate::core::error::{Error, Result};

/// The one bag key the engine reads.
const SURFACE_ID_KEY: &str = "surface_id";

/// The longest header msgpack puts in front of a string: the `str32` marker
/// plus its four length bytes. What a rewrite's allocation allows for, since
/// the id going in may take a wider header than the one coming out.
const LONGEST_MSGPACK_STRING_HEADER_BYTES: usize = 5;

/// A bag's top-level `surface_id`, and where its value sits in the bag's bytes.
pub struct ATopLevelSurfaceIdInABag<'a> {
    bag_bytes: &'a [u8],
    surface_id: &'a str,
    /// The id's msgpack value, marker included — what a rewrite splices over.
    value_span: Range<usize>,
}

impl<'a> ATopLevelSurfaceIdInABag<'a> {
    /// The surface this bag names.
    pub fn surface_id(&self) -> &'a str {
        self.surface_id
    }

    /// The same bag naming `local_surface_id` instead.
    ///
    /// Errs only for an id msgpack cannot spell, which the engine never mints;
    /// the alternative is an `unwrap` on a path a peer's bytes reach.
    pub fn a_bag_naming_this_surface_instead(&self, local_surface_id: &str) -> Result<Vec<u8>> {
        let mut rewritten = Vec::with_capacity(
            self.bag_bytes.len() + local_surface_id.len() - self.value_span.len()
                + LONGEST_MSGPACK_STRING_HEADER_BYTES,
        );
        rewritten.extend_from_slice(&self.bag_bytes[..self.value_span.start]);
        rmp::encode::write_str(&mut rewritten, local_surface_id).map_err(|cannot_spell_it| {
            Error::BagEncodeFailed(format!(
                "the surface id {local_surface_id} this runtime minted for an arriving frame \
                 cannot be written into the bag that names it: {cannot_spell_it}"
            ))
        })?;
        rewritten.extend_from_slice(&self.bag_bytes[self.value_span.end..]);
        Ok(rewritten)
    }
}

/// The surface `bag_bytes` names at its top level, or `None` when it names one
/// this engine cannot read.
///
/// A payload that is not a map, that ends mid-value, or whose `surface_id` is
/// not a string names none: a bag the walk cannot read crosses verbatim,
/// exactly as one with no surface would, because the engine has no business
/// deciding what a malformed bag means.
pub fn the_top_level_surface_id_of_a_bag(bag_bytes: &[u8]) -> Option<ATopLevelSurfaceIdInABag<'_>> {
    let mut walk = MsgpackWalk::over(bag_bytes);
    let entries = walk.read_a_map_header()?;
    for _ in 0..entries {
        let this_key_is_the_surface_id = match walk.read_a_string() {
            Some(key) => key == SURFACE_ID_KEY,
            None => {
                // A key that is not a string is legal msgpack and is not a key
                // the bag codec writes, so the walk steps over it like any
                // other value rather than refusing the bag.
                if !walk.step_over_one_value() {
                    return None;
                }
                false
            }
        };
        let value_begins_at = walk.at;
        if this_key_is_the_surface_id {
            let surface_id = walk.read_a_string()?;
            return Some(ATopLevelSurfaceIdInABag {
                bag_bytes,
                surface_id,
                value_span: value_begins_at..walk.at,
            });
        }
        if !walk.step_over_one_value() {
            return None;
        }
    }
    None
}

/// A cursor over one msgpack document, reading only the shape it is asked for.
struct MsgpackWalk<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> MsgpackWalk<'a> {
    fn over(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// The number of entries in the map this document begins with, or `None`
    /// when it does not begin with one.
    fn read_a_map_header(&mut self) -> Option<u32> {
        match self.take_marker()? {
            Marker::FixMap(entries) => Some(u32::from(entries)),
            Marker::Map16 => self.take_big_endian_u16().map(u32::from),
            Marker::Map32 => self.take_big_endian_u32(),
            _ => None,
        }
    }

    /// The string at the cursor, leaving the cursor past it — and leaving it
    /// where it was when the cursor is on anything else, so the caller can
    /// step over that value instead.
    fn read_a_string(&mut self) -> Option<&'a str> {
        let before = self.at;
        let length = match self.take_marker()? {
            Marker::FixStr(length) => u32::from(length),
            Marker::Str8 => u32::from(self.take_byte()?),
            Marker::Str16 => u32::from(self.take_big_endian_u16()?),
            Marker::Str32 => self.take_big_endian_u32()?,
            _ => {
                self.at = before;
                return None;
            }
        };
        let key = self.take_bytes(length as usize);
        match key.and_then(|key| std::str::from_utf8(key).ok()) {
            Some(key) => Some(key),
            None => {
                self.at = before;
                None
            }
        }
    }

    /// Step the cursor past exactly one value, whatever its shape, answering
    /// whether the document held one.
    fn step_over_one_value(&mut self) -> bool {
        // Iterative rather than recursive: a bag is user data, and a deeply
        // nested one must cost stack the engine chose rather than stack the
        // producer did.
        let mut values_still_to_step_over: u64 = 1;
        while values_still_to_step_over > 0 {
            values_still_to_step_over -= 1;
            let Some(marker) = self.take_marker() else {
                return false;
            };
            let stepped = match marker {
                Marker::FixPos(_)
                | Marker::FixNeg(_)
                | Marker::Null
                | Marker::True
                | Marker::False
                | Marker::Reserved => Some(0),
                Marker::U8 | Marker::I8 => self.skip(1),
                Marker::U16 | Marker::I16 => self.skip(2),
                Marker::U32 | Marker::I32 | Marker::F32 => self.skip(4),
                Marker::U64 | Marker::I64 | Marker::F64 => self.skip(8),
                Marker::FixStr(length) => self.skip(usize::from(length)),
                Marker::Str8 | Marker::Bin8 => self
                    .take_byte()
                    .and_then(|length| self.skip(usize::from(length))),
                Marker::Str16 | Marker::Bin16 => self
                    .take_big_endian_u16()
                    .and_then(|length| self.skip(usize::from(length))),
                Marker::Str32 | Marker::Bin32 => self
                    .take_big_endian_u32()
                    .and_then(|length| self.skip(length as usize)),
                Marker::FixArray(entries) => {
                    values_still_to_step_over += u64::from(entries);
                    Some(0)
                }
                Marker::Array16 => self.take_big_endian_u16().map(|entries| {
                    values_still_to_step_over += u64::from(entries);
                    0
                }),
                Marker::Array32 => self.take_big_endian_u32().map(|entries| {
                    values_still_to_step_over += u64::from(entries);
                    0
                }),
                Marker::FixMap(entries) => {
                    values_still_to_step_over += u64::from(entries) * 2;
                    Some(0)
                }
                Marker::Map16 => self.take_big_endian_u16().map(|entries| {
                    values_still_to_step_over += u64::from(entries) * 2;
                    0
                }),
                Marker::Map32 => self.take_big_endian_u32().map(|entries| {
                    values_still_to_step_over += u64::from(entries) * 2;
                    0
                }),
                Marker::FixExt1 => self.skip(2),
                Marker::FixExt2 => self.skip(3),
                Marker::FixExt4 => self.skip(5),
                Marker::FixExt8 => self.skip(9),
                Marker::FixExt16 => self.skip(17),
                Marker::Ext8 => self
                    .take_byte()
                    .and_then(|length| self.skip(usize::from(length) + 1)),
                Marker::Ext16 => self
                    .take_big_endian_u16()
                    .and_then(|length| self.skip(usize::from(length) + 1)),
                Marker::Ext32 => self
                    .take_big_endian_u32()
                    .and_then(|length| self.skip(length as usize + 1)),
            };
            if stepped.is_none() {
                return false;
            }
        }
        true
    }

    fn take_marker(&mut self) -> Option<Marker> {
        self.take_byte().map(Marker::from_u8)
    }

    fn take_byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn take_big_endian_u16(&mut self) -> Option<u16> {
        self.take_bytes(2)
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn take_big_endian_u32(&mut self) -> Option<u32> {
        self.take_bytes(4)
            .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn take_bytes(&mut self, length: usize) -> Option<&'a [u8]> {
        let taken = self.bytes.get(self.at..self.at.checked_add(length)?)?;
        self.at += length;
        Some(taken)
    }

    fn skip(&mut self, length: usize) -> Option<usize> {
        self.take_bytes(length).map(|_| length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a_bag(shape: serde_json::Value) -> Vec<u8> {
        rmp_serde::to_vec_named(&shape).expect("a bag encodes")
    }

    fn the_surface_named_by(bag_bytes: &[u8]) -> Option<&str> {
        the_top_level_surface_id_of_a_bag(bag_bytes).map(|named| named.surface_id())
    }

    /// A video frame names its surface at the top level, which is the one key
    /// the engine reads.
    #[test]
    fn a_bag_naming_a_surface_at_its_top_level_is_found() {
        assert_eq!(
            the_surface_named_by(&a_bag(json!({
                "surface_id": "7#3",
                "width": 1920,
                "height": 1080,
                "timestamp_ns": 1_726_000_000_000_000_000i64,
            }))),
            Some("7#3")
        );
    }

    /// The key is read at the top level only: one nested inside another value
    /// names no surface this runtime resolves, so the bag crosses verbatim.
    #[test]
    fn a_surface_id_nested_inside_another_value_is_not_a_top_level_one() {
        assert_eq!(
            the_surface_named_by(&a_bag(json!({
                "thumbnail": { "surface_id": "7#3" },
                "frames": [{ "surface_id": "8#1" }],
            }))),
            None
        );
    }

    /// Every value shape the bag codec writes is stepped over, so a key after
    /// one of them is still reached.
    #[test]
    fn the_walk_steps_over_every_value_shape_and_reaches_the_key_behind_it() {
        let after_everything = json!({
            "a_null": null,
            "a_bool": true,
            "a_small_int": 7,
            "a_big_int": 1_726_000_000_000_000_000i64,
            "a_negative": -9_000_000_000i64,
            "a_float": 1.5,
            "a_string": "x".repeat(40_000),
            "an_array": [1, "two", [3, 4], { "five": 5 }],
            "a_map": { "nested": { "deeper": [1, 2, 3] } },
            "surface_id": "7#3",
        });
        assert_eq!(the_surface_named_by(&a_bag(after_everything)), Some("7#3"));
    }

    /// A bag carrying bytes — an encoded frame, an audio block — is walked past
    /// its payload rather than through it.
    #[test]
    fn a_bag_carrying_a_binary_payload_is_walked_past_it() {
        let mut bag = Vec::new();
        rmp::encode::write_map_len(&mut bag, 2).expect("a map header");
        rmp::encode::write_str(&mut bag, "bitstream").expect("a key");
        rmp::encode::write_bin(&mut bag, &vec![0xAB; 100_000]).expect("a payload");
        rmp::encode::write_str(&mut bag, "surface_id").expect("a key");
        rmp::encode::write_str(&mut bag, "7#3").expect("a value");
        assert_eq!(the_surface_named_by(&bag), Some("7#3"));
    }

    /// A bag with no surface, and a payload that is not a bag at all, both
    /// carry none — a bag the walk cannot read crosses verbatim.
    #[test]
    fn a_bag_with_no_surface_and_a_payload_that_is_not_one_both_carry_none() {
        assert_eq!(
            the_surface_named_by(&a_bag(json!({
                "samples": "abc",
                "sample_rate": 48_000,
            }))),
            None
        );
        assert_eq!(the_surface_named_by(&a_bag(json!([1, 2, 3]))), None);
        assert_eq!(the_surface_named_by(b"not msgpack at all"), None);
        assert_eq!(the_surface_named_by(&[]), None);
    }

    /// A map header claiming more entries than the bytes hold ends the walk
    /// rather than reading past the payload.
    #[test]
    fn a_truncated_bag_ends_the_walk_rather_than_reading_past_it() {
        let mut truncated = Vec::new();
        rmp::encode::write_map_len(&mut truncated, 4).expect("a map header");
        rmp::encode::write_str(&mut truncated, "width").expect("a key");
        rmp::encode::write_uint(&mut truncated, 1920).expect("a value");
        assert_eq!(the_surface_named_by(&truncated), None);
    }

    /// A `surface_id` that is not a string names no surface this engine wrote,
    /// so the bag crosses verbatim rather than being read as one it can carry.
    #[test]
    fn a_surface_id_that_is_not_a_string_names_no_surface() {
        assert_eq!(
            the_surface_named_by(&a_bag(json!({ "surface_id": 7, "width": 64 }))),
            None
        );
        assert_eq!(
            the_surface_named_by(&a_bag(json!({ "surface_id": null }))),
            None
        );
    }

    /// The rewrite is what the ingress hands downstream: the frame's own bag,
    /// naming the surface this runtime minted, with every other key, every
    /// other value and their order exactly as the producer wrote them.
    #[test]
    fn the_rewritten_bag_names_the_local_surface_and_changes_nothing_else() {
        let produced = a_bag(json!({
            "width": 1920,
            "surface_id": "7#3",
            "height": 1080,
            "timestamp_ns": 1_726_000_000_000_000_000i64,
            "texture_layout": "general",
        }));
        let named = the_top_level_surface_id_of_a_bag(&produced).expect("the bag names a surface");

        let rewritten = named
            .a_bag_naming_this_surface_instead("pool-slot-4e1f#12")
            .expect("an engine-minted id is spellable");

        assert_eq!(the_surface_named_by(&rewritten), Some("pool-slot-4e1f#12"));
        let read_back: serde_json::Value =
            rmp_serde::from_slice(&rewritten).expect("the rewrite is still one bag");
        assert_eq!(
            read_back,
            json!({
                "width": 1920,
                "surface_id": "pool-slot-4e1f#12",
                "height": 1080,
                "timestamp_ns": 1_726_000_000_000_000_000i64,
                "texture_layout": "general",
            })
        );
        // Byte-for-byte outside the id: the splice must not re-encode a key or
        // a value into whatever this engine would have written for it.
        let produced_elsewhere = a_bag(json!({
            "width": 1920,
            "surface_id": "pool-slot-4e1f#12",
            "height": 1080,
            "timestamp_ns": 1_726_000_000_000_000_000i64,
            "texture_layout": "general",
        }));
        assert_eq!(rewritten, produced_elsewhere);
    }

    /// An id of any length lands, whichever msgpack string header it takes —
    /// the pool's ids are short today and the splice must not depend on that.
    #[test]
    fn an_id_shorter_or_longer_than_the_one_it_replaces_both_land() {
        for local_surface_id in ["a", "7#3", &"p".repeat(200), &"p".repeat(70_000)] {
            let produced = a_bag(json!({ "surface_id": "7#3", "width": 64 }));
            let named =
                the_top_level_surface_id_of_a_bag(&produced).expect("the bag names a surface");
            let rewritten = named
                .a_bag_naming_this_surface_instead(local_surface_id)
                .expect("an id of any length is spellable");
            assert_eq!(the_surface_named_by(&rewritten), Some(local_surface_id));
            let read_back: serde_json::Value =
                rmp_serde::from_slice(&rewritten).expect("the rewrite is still one bag");
            assert_eq!(read_back["width"], json!(64));
        }
    }

    /// A bag whose surface is the last key, and one whose surface is the
    /// first, both splice — the tail and the head of the splice are the two
    /// sides an off-by-one lands in.
    #[test]
    fn a_surface_at_either_end_of_the_map_splices() {
        for produced in [
            a_bag(json!({ "surface_id": "7#3", "width": 64, "height": 48 })),
            a_bag(json!({ "width": 64, "height": 48, "surface_id": "7#3" })),
        ] {
            let named =
                the_top_level_surface_id_of_a_bag(&produced).expect("the bag names a surface");
            let rewritten = named
                .a_bag_naming_this_surface_instead("11#2")
                .expect("an engine-minted id is spellable");
            let read_back: serde_json::Value =
                rmp_serde::from_slice(&rewritten).expect("the rewrite is still one bag");
            assert_eq!(read_back["surface_id"], json!("11#2"));
            assert_eq!(read_back["width"], json!(64));
            assert_eq!(read_back["height"], json!(48));
        }
    }
}
