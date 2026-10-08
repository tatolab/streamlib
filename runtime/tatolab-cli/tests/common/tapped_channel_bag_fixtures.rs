// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Bags as a channel carries them, the `tap` tool's result carrying them, and the images exchanged
//! for them, for the `exchange` tests. The frame header is written by the transport's own writer
//! and the payload by msgpack's, never by the decoder under test. Shared by the integration tests
//! and, through `#[path]`, the unit tests; either mounts it beside `stub_local_api_server`.

#![allow(dead_code)]

use std::path::Path;

use streamlib_ipc_types::{FRAME_HEADER_SIZE, FrameHeader};
use streamlib_runtime_client_contract::local_api_wire_contract::{TapToolResult, TapToolResultBag};

use super::stub_local_api_server::StubSurfaceImageAnswer;

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

/// The per-bag preview ceiling a fixture tap result states.
const FIXTURE_TAP_MAX_BAG_BYTES: usize = 1024 * 1024;

/// The sample window a fixture tap result states.
const FIXTURE_TAP_WINDOW_MS: u64 = 500;

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
/// is flagged as capped and stated at [`CAPPED_BAG_STATED_BYTE_LEN`].
pub fn tap_result_text_capping_bags(
    framed_bags: &[Vec<u8>],
    capped_bag_indexes: &[usize],
) -> String {
    let tapped_bags: Vec<TapToolResultBag> = framed_bags
        .iter()
        .enumerate()
        .map(|(bag_index, framed_bag_bytes)| {
            let preview_was_capped = capped_bag_indexes.contains(&bag_index);
            TapToolResultBag {
                byte_len: if preview_was_capped {
                    CAPPED_BAG_STATED_BYTE_LEN
                } else {
                    u64::try_from(framed_bag_bytes.len()).unwrap()
                },
                hex_preview: hex::encode(framed_bag_bytes),
                hex_truncated: preview_was_capped,
            }
        })
        .collect();
    serde_json::to_string(&TapToolResult {
        channel: FIXTURE_CHANNEL.to_owned(),
        requested: framed_bags.len(),
        received: framed_bags.len(),
        window_ms: FIXTURE_TAP_WINDOW_MS,
        dropped_bags: 0,
        max_bag_bytes: FIXTURE_TAP_MAX_BAG_BYTES,
        bags_withheld_at_byte_budget: 0,
        bags: tapped_bags,
    })
    .unwrap()
}

/// The `tap` tool's result text carrying `framed_bags`, none capped.
pub fn tap_result_text(framed_bags: &[Vec<u8>]) -> String {
    tap_result_text_capping_bags(framed_bags, &[])
}

/// The `tap` tool's result text for a round that caught no bag.
pub fn empty_tap_result_text() -> String {
    tap_result_text(&[])
}

/// Stand-in PNG bytes, distinct per label so a test can tell which frame landed in which file.
pub fn png_bytes_for(label: &str) -> Vec<u8> {
    [b"\x89PNG\r\n\x1a\n".as_slice(), label.as_bytes()].concat()
}

/// The image route's `200` for [`png_bytes_for`]`(label)`, stating a 1920x1080 source surface.
pub fn labelled_png_image_answer(label: &str) -> StubSurfaceImageAnswer {
    StubSurfaceImageAnswer::png_image(&png_bytes_for(label), Some(1920), Some(1080))
}

/// How many `.png` files `directory` holds.
pub fn png_files_in(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .filter(|directory_entry| {
            directory_entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "png")
        })
        .count()
}
