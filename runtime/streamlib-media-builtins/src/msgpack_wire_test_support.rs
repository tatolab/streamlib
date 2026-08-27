// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading a cast's own msgpack wire bytes back, for the wire-key lock tests.
//!
//! Shared by every bag cast in this crate: each one's contract is the keys and
//! the msgpack types a consumer in another language reads, and each one locks
//! them the same way.

/// Decode msgpack wire bytes as a named map, once for a whole test.
pub(crate) fn decode_msgpack_named_map_entries(
    wire_bytes: &[u8],
) -> Vec<(rmpv::Value, rmpv::Value)> {
    let value: rmpv::Value =
        rmpv::decode::read_value(&mut &wire_bytes[..]).expect("msgpack decode");
    let rmpv::Value::Map(entries) = value else {
        panic!("wire value must be a named map, got {value:?}");
    };
    entries
}

/// One entry of a decoded named map, under the key a consumer reads it by.
pub(crate) fn wire_map_entry_named(
    entries: &[(rmpv::Value, rmpv::Value)],
    key_name: &str,
) -> rmpv::Value {
    entries
        .iter()
        .find(|(key, _)| key.as_str() == Some(key_name))
        .unwrap_or_else(|| panic!("wire map missing key {key_name:?}"))
        .1
        .clone()
}
