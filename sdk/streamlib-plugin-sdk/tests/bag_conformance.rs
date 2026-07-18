// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-language dynamic-bag wire conformance (issue #1407).
//!
//! The committed fixture `tests/fixtures/bag_conformance.msgpack` is a msgpack
//! named map carrying every value class a [`Bag`] supports. The three SDK
//! runtimes decode the *identical* bytes and assert the same logical values:
//!
//! - Rust: this file.
//! - Python: `sdk/streamlib-python/python/streamlib/tests/test_bag_conformance.py`.
//! - Deno: `sdk/streamlib-deno/bag_conformance_test.ts`.
//!
//! All three read the same file, so a wire disagreement between the runtimes
//! fails a test rather than silently corrupting a payload. Regenerate the
//! fixture (after an intentional value-class change) with:
//!
//! ```text
//! cargo test -p streamlib-plugin-sdk --test bag_conformance -- --ignored regenerate_fixture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use streamlib_plugin_sdk::sdk::bag::Bag;
use streamlib_plugin_sdk::sdk::error::Error;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("bag_conformance.msgpack")
}

/// The canonical value-class-complete bag. Every SDK runtime's conformance
/// test mirrors these exact key/value pairs.
fn canonical_bag() -> Bag {
    let mut bag = Bag::new();
    bag.set("nil", Option::<i64>::None).unwrap();
    bag.set("flag", true).unwrap();
    bag.set("count", -7_i64).unwrap();
    bag.set("big", 4_000_000_000_u64).unwrap();
    bag.set("ratio", 1.5_f64).unwrap();
    bag.set("name", "streamlib").unwrap();
    bag.set("list", vec![1_i64, 2, 3]).unwrap();
    let mut nested: BTreeMap<String, String> = BTreeMap::new();
    nested.insert("inner".to_owned(), "value".to_owned());
    bag.set("nested", nested).unwrap();
    bag.set_bin("blob", vec![0xDE_u8, 0xAD, 0xBE, 0xEF]);
    bag
}

fn assert_canonical_values(bag: &Bag) {
    assert_eq!(bag.get::<Option<i64>>("nil").unwrap(), None);
    assert!(bag.get::<bool>("flag").unwrap());
    assert_eq!(bag.get::<i64>("count").unwrap(), -7);
    assert_eq!(bag.get::<u64>("big").unwrap(), 4_000_000_000);
    assert_eq!(bag.get::<f64>("ratio").unwrap(), 1.5);
    assert_eq!(bag.get::<String>("name").unwrap(), "streamlib");
    assert_eq!(bag.get::<Vec<i64>>("list").unwrap(), vec![1, 2, 3]);
    let nested: BTreeMap<String, String> = bag.get("nested").unwrap();
    assert_eq!(nested.get("inner").map(String::as_str), Some("value"));
    assert_eq!(bag.get_bin("blob").unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn fixture_decodes_to_canonical_values() {
    let bytes = std::fs::read(fixture_path()).expect(
        "bag_conformance.msgpack fixture missing — regenerate with the \
         `--ignored regenerate_fixture` test",
    );
    let bag = Bag::from_msgpack(&bytes).unwrap();
    assert_canonical_values(&bag);
}

#[test]
fn canonical_bag_is_byte_stable_against_fixture() {
    // The Rust encoder is the fixture's source of truth; if this drifts, the
    // fixture (and the Python/Deno expectations) must be regenerated
    // deliberately, not silently.
    let bytes = std::fs::read(fixture_path()).unwrap();
    assert_eq!(
        canonical_bag().to_msgpack().unwrap(),
        bytes,
        "canonical bag no longer matches the committed fixture — regenerate it"
    );
}

#[test]
fn tolerant_reads_of_missing_and_unexpected_fields() {
    let bytes = std::fs::read(fixture_path()).unwrap();
    let bag = Bag::from_msgpack(&bytes).unwrap();

    // A field a consumer doesn't know about is simply present and ignorable.
    assert!(bag.contains_key("nested"));
    // A field the producer never sent reads as a named miss, not a panic.
    assert!(matches!(
        bag.get::<i64>("frame_rate"),
        Err(Error::BagKeyMissing { .. })
    ));
    assert_eq!(bag.get_opt::<i64>("frame_rate").unwrap(), None);
    // A present field read at the wrong type is a distinct named error.
    assert!(matches!(
        bag.get::<i64>("name"),
        Err(Error::BagTypeMismatch { .. })
    ));
}

#[test]
#[ignore = "run explicitly to (re)write the committed cross-language fixture"]
fn regenerate_fixture() {
    let path = fixture_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, canonical_bag().to_msgpack().unwrap()).unwrap();
}
