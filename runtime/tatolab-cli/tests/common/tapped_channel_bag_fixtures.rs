// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bags as a channel carries them, and the `tap` tool's result carrying them, for the `exchange`
//! tests. The frame header is written by the transport's own writer and the payload by msgpack's,
//! never by the decoder under test. Reached through `#[path]` by the unit tests and by the
//! `exchange` integration tests, so it names nothing from the other test support files.

#![allow(dead_code)]

use streamlib_ipc_types::{FRAME_HEADER_SIZE, FrameHeader};

/// The channel every fixture bag is tapped from.
pub const FIXTURE_CHANNEL: &str = "cam/frame";

/// The port a fixture bag's frame header names.
const FIXTURE_BAG_PORT_NAME: &str = "cam/frame";

/// The timestamp a fixture bag's frame header stamps.
const FIXTURE_BAG_TIMESTAMP_NS: i64 = 7_000;

/// The slice a published fixture bag rides in: iceoryx2's slices are fixed-capacity, so the bytes
/// past the frame are slack the decode must not read.
pub const FIXTURE_BAG_SLICE_CAPACITY: usize = 1024;

/// A slice exactly as long as the frame, with no slack behind the payload.
pub const SLICE_HOLDS_ONLY_THE_BAG: usize = 0;

/// The whole-bag length the tap result states for a bag whose preview it capped: more than the
/// preview carried.
pub const CAPPED_BAG_STATED_BYTE_LEN: u64 = 9000;

/// A msgpack named map holding `entries` in order.
pub fn msgpack_named_map(entries: &[(&str, rmpv::Value)]) -> Vec<u8> {
    let named_map = rmpv::Value::Map(
        entries
            .iter()
            .map(|(entry_name, entry_value)| (rmpv::Value::from(*entry_name), entry_value.clone()))
            .collect(),
    );
    let mut encoded_named_map = Vec::new();
    rmpv::encode::write_value(&mut encoded_named_map, &named_map).unwrap();
    encoded_named_map
}

/// `bag_payload` framed as the channel carries it, in a slice of at least `slice_capacity` bytes.
pub fn framed_bag(bag_payload: &[u8], slice_capacity: usize) -> Vec<u8> {
    let mut framed_bag_bytes = vec![0u8; slice_capacity.max(FRAME_HEADER_SIZE + bag_payload.len())];
    FrameHeader::new(
        FIXTURE_BAG_PORT_NAME,
        FIXTURE_BAG_TIMESTAMP_NS,
        u32::try_from(bag_payload.len()).unwrap(),
    )
    .unwrap()
    .write_to_slice(&mut framed_bag_bytes);
    framed_bag_bytes[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + bag_payload.len()]
        .copy_from_slice(bag_payload);
    framed_bag_bytes
}

/// A framed bag whose `surface_id_bag_field_name` carries `published_surface_id`, beside a width.
pub fn bag_publishing_surface_id_in_field(
    published_surface_id: &str,
    surface_id_bag_field_name: &str,
) -> Vec<u8> {
    framed_bag(
        &msgpack_named_map(&[
            (surface_id_bag_field_name, published_surface_id.into()),
            ("width", 640.into()),
        ]),
        FIXTURE_BAG_SLICE_CAPACITY,
    )
}

/// A framed bag whose `surface_id` field carries `published_surface_id`.
pub fn bag_publishing_surface_id(published_surface_id: &str) -> Vec<u8> {
    bag_publishing_surface_id_in_field(published_surface_id, "surface_id")
}

/// A framed bag carrying a width and no surface id.
pub fn bag_publishing_no_surface_id() -> Vec<u8> {
    framed_bag(
        &msgpack_named_map(&[("width", 640.into())]),
        FIXTURE_BAG_SLICE_CAPACITY,
    )
}

/// The `tap` tool's result text carrying `framed_bags`. A bag at an index in `capped_bag_indexes`
/// is flagged as capped and stated at [`CAPPED_BAG_STATED_BYTE_LEN`]; `byte_len` is left out of
/// every bag unless `whole_bag_byte_len_stated`, as a runtime that flags a cap without sizing it.
pub fn tap_result_text_capping_bags(
    framed_bags: &[Vec<u8>],
    capped_bag_indexes: &[usize],
    whole_bag_byte_len_stated: bool,
) -> String {
    let tapped_bags: Vec<serde_json::Value> = framed_bags
        .iter()
        .enumerate()
        .map(|(bag_index, framed_bag_bytes)| {
            let preview_was_capped = capped_bag_indexes.contains(&bag_index);
            let mut tapped_bag = serde_json::Map::new();
            if whole_bag_byte_len_stated {
                tapped_bag.insert(
                    "byte_len".to_owned(),
                    if preview_was_capped {
                        CAPPED_BAG_STATED_BYTE_LEN.into()
                    } else {
                        framed_bag_bytes.len().into()
                    },
                );
            }
            tapped_bag.insert(
                "hex_preview".to_owned(),
                lowercase_hex(framed_bag_bytes).into(),
            );
            tapped_bag.insert("hex_truncated".to_owned(), preview_was_capped.into());
            serde_json::Value::Object(tapped_bag)
        })
        .collect();
    serde_json::json!({
        "channel": FIXTURE_CHANNEL,
        "requested": framed_bags.len(),
        "received": framed_bags.len(),
        "window_ms": 500,
        "dropped_bags": 0,
        "bags": tapped_bags,
    })
    .to_string()
}

/// The `tap` tool's result text carrying `framed_bags`, none capped.
pub fn tap_result_text(framed_bags: &[Vec<u8>]) -> String {
    tap_result_text_capping_bags(framed_bags, &[], true)
}

/// The `tap` tool's result text for a round that caught no bag.
pub fn empty_tap_result_text() -> String {
    tap_result_text(&[])
}

/// Stand-in PNG bytes, distinct per label so a test can tell which frame landed in which file.
pub fn png_bytes_for(label: &str) -> Vec<u8> {
    [b"\x89PNG\r\n\x1a\n".as_slice(), label.as_bytes()].concat()
}

fn lowercase_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
