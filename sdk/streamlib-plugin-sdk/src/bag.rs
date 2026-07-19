// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`Bag`] — a dynamic, self-describing msgpack named-map payload.
//!
//! Every frame on the StreamLib wire is already a msgpack named map (the
//! `to_vec_named` convention: struct fields become string map keys). `Bag`
//! makes that map a first-class SDK value so a processor can read the fields
//! it needs and write a map without codegen, a generated struct, or a schema
//! package. It is the engine-free, schema-free counterpart to the typed
//! `write::<T>` / `read::<T>` paths on
//! [`crate::iceoryx2::OutputWriter`] / [`crate::iceoryx2::InputMailboxes`].
//!
//! # Wire contract
//!
//! A `Bag` encodes as a msgpack **map with string keys** — byte-for-byte the
//! shape `rmp_serde::to_vec_named` emits for a struct, so a Python processor's
//! `msgpack.packb(dict, use_bin_type=True)` and a Deno processor's
//! `msgpack.encode(obj)` interoperate with it directly. Insertion order is
//! preserved. `bin`-typed values (msgpack tag `0xc4..0xc6`) round-trip as
//! [`Bag::get_bin`] / [`Bag::set_bin`] rather than an array of integers.
//!
//! Reads are **tolerant**: a missing key and a wrong-typed key are distinct
//! named [`Error`] variants ([`Error::BagKeyMissing`],
//! [`Error::BagTypeMismatch`]), never a panic and never an untyped `()`
//! error.

use rmpv::Value;
use serde::Serialize;
use serde::de::DeserializeOwned;
use streamlib_error::{Error, Result};

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serializer};

/// An owned, eagerly-decoded msgpack named map with string keys.
///
/// Construct with [`Bag::new`], the [`crate::bag`] literal macro, or
/// [`Bag::from_msgpack`]; read typed fields with [`Bag::get`] /
/// [`Bag::get_opt`] / [`Bag::get_bin`]; write with [`Bag::set`] /
/// [`Bag::set_bin`]; and encode with [`Bag::to_msgpack`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bag {
    /// Insertion-ordered `(key, value)` entries. A linear scan is used for
    /// lookup: a wire bag is a handful of fields, so an ordered `Vec` beats a
    /// hash map on both allocation and cache behavior at this size.
    entries: Vec<(String, Value)>,
}

impl Bag {
    /// Create an empty bag.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Decode a msgpack named map (string-keyed map) into a bag.
    ///
    /// Returns [`Error::BagDecodeFailed`] if the bytes are not valid msgpack,
    /// if the top-level value is not a map, or if any key is not a string
    /// (StreamLib payloads are named maps by convention, never
    /// integer-keyed).
    pub fn from_msgpack(bytes: &[u8]) -> Result<Self> {
        let mut cursor = bytes;
        let value = rmpv::decode::read_value(&mut cursor)
            .map_err(|e| Error::BagDecodeFailed(e.to_string()))?;
        let pairs = match value {
            Value::Map(pairs) => pairs,
            other => {
                return Err(Error::BagDecodeFailed(format!(
                    "top-level msgpack value is a {}, expected a named map",
                    value_kind(&other)
                )));
            }
        };
        let mut entries = Vec::with_capacity(pairs.len());
        for (key, val) in pairs {
            let key = key.as_str().map(str::to_owned).ok_or_else(|| {
                Error::BagDecodeFailed(format!(
                    "map key {:?} is not a string — bags are named maps",
                    key
                ))
            })?;
            entries.push((key, val));
        }
        Ok(Self { entries })
    }

    /// Encode this bag as a msgpack named map (string-keyed map), the exact
    /// shape `rmp_serde::to_vec_named` emits for a struct.
    pub fn to_msgpack(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        rmp::encode::write_map_len(&mut out, self.entries.len() as u32)
            .map_err(|e| Error::BagEncodeFailed(e.to_string()))?;
        for (key, value) in &self.entries {
            rmp::encode::write_str(&mut out, key)
                .map_err(|e| Error::BagEncodeFailed(e.to_string()))?;
            rmpv::encode::write_value(&mut out, value)
                .map_err(|e| Error::BagEncodeFailed(e.to_string()))?;
        }
        Ok(out)
    }

    /// Read the value at `key` as `T`.
    ///
    /// Returns [`Error::BagKeyMissing`] when the key is absent and
    /// [`Error::BagTypeMismatch`] when the stored value cannot deserialize
    /// into `T` — the two failure modes stay distinguishable so a processor
    /// can tell "field not sent" from "field sent with the wrong shape".
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let value = self
            .lookup(key)
            .ok_or_else(|| Error::BagKeyMissing { key: key.to_owned() })?;
        self.decode_value(key, value)
    }

    /// Read the value at `key` as `T`, tolerating absence.
    ///
    /// Returns `Ok(None)` when the key is absent, `Ok(Some(T))` on a
    /// successful decode, and [`Error::BagTypeMismatch`] when the key is
    /// present but the stored value does not fit `T`.
    pub fn get_opt<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.lookup(key) {
            None => Ok(None),
            Some(value) => self.decode_value(key, value).map(Some),
        }
    }

    /// Deserialize a looked-up value into `T`, mapping any decode failure to
    /// the [`Error::BagTypeMismatch`] that names `key` and the expected type.
    fn decode_value<T: DeserializeOwned>(&self, key: &str, value: &Value) -> Result<T> {
        rmpv::ext::from_value(value.clone()).map_err(|e| Error::BagTypeMismatch {
            key: key.to_owned(),
            expected_type: std::any::type_name::<T>().to_owned(),
            detail: e.to_string(),
        })
    }

    /// Read the value at `key` as a msgpack `bin` byte string.
    ///
    /// Returns [`Error::BagKeyMissing`] when absent and
    /// [`Error::BagTypeMismatch`] when the value is present but not a `bin`
    /// (e.g. it was written as a msgpack array or string).
    pub fn get_bin(&self, key: &str) -> Result<Vec<u8>> {
        let value = self
            .lookup(key)
            .ok_or_else(|| Error::BagKeyMissing { key: key.to_owned() })?;
        match value {
            Value::Binary(bytes) => Ok(bytes.clone()),
            other => Err(Error::BagTypeMismatch {
                key: key.to_owned(),
                expected_type: "bin".to_owned(),
                detail: format!("value is a {}, not a msgpack bin", value_kind(other)),
            }),
        }
    }

    /// Set `key` to a serializable value, replacing any existing entry.
    ///
    /// Returns [`Error::BagEncodeFailed`] if `value` cannot be represented as
    /// a msgpack value (e.g. a map with non-string keys). A `Vec<u8>` set
    /// this way encodes as a msgpack **array**; use [`Bag::set_bin`] for a
    /// `bin` byte string.
    pub fn set<T: Serialize>(&mut self, key: &str, value: T) -> Result<()> {
        let value =
            rmpv::ext::to_value(value).map_err(|e| Error::BagEncodeFailed(e.to_string()))?;
        self.insert_value(key, value);
        Ok(())
    }

    /// Set `key` to a msgpack `bin` byte string, replacing any existing
    /// entry. The single-copy wire footprint counterpart to a `Vec<u8>`
    /// [`Bag::set`] (which would emit an integer array).
    pub fn set_bin(&mut self, key: &str, bytes: impl Into<Vec<u8>>) {
        self.insert_value(key, Value::Binary(bytes.into()));
    }

    /// True iff `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.lookup(key).is_some()
    }

    /// Remove `key`, returning true iff it was present.
    pub fn remove(&mut self, key: &str) -> bool {
        if let Some(index) = self.entries.iter().position(|(k, _)| k == key) {
            self.entries.remove(index);
            true
        } else {
            false
        }
    }

    /// Iterate the bag's keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True iff the bag has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn lookup(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    fn insert_value(&mut self, key: &str, value: Value) {
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| k == key) {
            entry.1 = value;
        } else {
            self.entries.push((key.to_owned(), value));
        }
    }
}

/// A [`Bag`] serializes as a named map — byte-for-byte the shape
/// [`Bag::to_msgpack`] emits under `rmp_serde` and a plain JSON object under
/// `serde_json`. This is what makes `Bag` a valid processor
/// [`Config`](crate::processors::Config): a processor can declare
/// `type Config = Bag` to receive its entire configuration dynamically,
/// with no codegen struct and no schema.
impl Serialize for Bag {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Bag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct BagVisitor;

        impl<'de> Visitor<'de> for BagVisitor {
            type Value = Bag;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a named map with string keys")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<Bag, A::Error> {
                let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((key, value)) = access.next_entry::<String, Value>()? {
                    entries.push((key, value));
                }
                Ok(Bag { entries })
            }
        }

        deserializer.deserialize_map(BagVisitor)
    }
}

/// Human-readable msgpack value class, for error messages only.
fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "bool",
        Value::Integer(_) => "integer",
        Value::F32(_) | Value::F64(_) => "float",
        Value::String(_) => "string",
        Value::Binary(_) => "bin",
        Value::Array(_) => "array",
        Value::Map(_) => "map",
        Value::Ext(..) => "ext",
    }
}

/// Build a [`Bag`] literal from `"key" => value` pairs.
///
/// Each value is serialized with [`Bag::set`]; a non-serializable literal
/// (e.g. a map keyed by a non-string) panics at the construction site, the
/// same failure mode as `serde_json::json!`. Use [`Bag::set_bin`] after
/// construction for msgpack `bin` fields.
#[macro_export]
macro_rules! bag {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut bag = $crate::sdk::bag::Bag::new();
        $(
            ::std::result::Result::expect(
                bag.set($key, $value),
                concat!("bag! literal: value for key ", stringify!($key), " is not msgpack-serializable"),
            );
        )*
        bag
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn value_class_bag() -> Bag {
        let mut bag = Bag::new();
        bag.set("nil", Option::<i64>::None).unwrap();
        bag.set("bool", true).unwrap();
        bag.set("neg_int", -42_i64).unwrap();
        bag.set("uint", 4_000_000_000_u64).unwrap();
        bag.set("float", 3.5_f64).unwrap();
        bag.set("text", "hello").unwrap();
        bag.set("array", vec![1_i64, 2, 3]).unwrap();
        bag.set("nested", {
            let mut inner = Bag::new();
            inner.set("k", "v").unwrap();
            // Nested maps ride as a serde value; encode/decode via the outer.
            rmpv::ext::to_value(inner.to_msgpack().unwrap()).unwrap()
        })
        .unwrap();
        bag.set_bin("blob", vec![0xDE_u8, 0xAD, 0xBE, 0xEF]);
        bag
    }

    #[test]
    fn round_trips_every_value_class() {
        let bag = value_class_bag();
        let bytes = bag.to_msgpack().unwrap();
        let decoded = Bag::from_msgpack(&bytes).unwrap();

        assert_eq!(decoded.get::<Option<i64>>("nil").unwrap(), None);
        assert!(decoded.get::<bool>("bool").unwrap());
        assert_eq!(decoded.get::<i64>("neg_int").unwrap(), -42);
        assert_eq!(decoded.get::<u64>("uint").unwrap(), 4_000_000_000);
        assert_eq!(decoded.get::<f64>("float").unwrap(), 3.5);
        assert_eq!(decoded.get::<String>("text").unwrap(), "hello");
        assert_eq!(decoded.get::<Vec<i64>>("array").unwrap(), vec![1, 2, 3]);
        assert_eq!(decoded.get_bin("blob").unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn missing_key_is_named_error() {
        let bag = Bag::new();
        match bag.get::<i64>("absent") {
            Err(Error::BagKeyMissing { key }) => assert_eq!(key, "absent"),
            other => panic!("expected BagKeyMissing, got {:?}", other.err()),
        }
        assert_eq!(bag.get_opt::<i64>("absent").unwrap(), None);
    }

    #[test]
    fn wrong_type_is_named_error() {
        let mut bag = Bag::new();
        bag.set("text", "not a number").unwrap();
        match bag.get::<i64>("text") {
            Err(Error::BagTypeMismatch { key, .. }) => assert_eq!(key, "text"),
            other => panic!("expected BagTypeMismatch, got {:?}", other.err()),
        }
        // Tolerant read of a present-but-wrong-typed field is still an error,
        // not a silent None.
        assert!(matches!(
            bag.get_opt::<i64>("text"),
            Err(Error::BagTypeMismatch { .. })
        ));
    }

    #[test]
    fn bin_vs_array_are_distinct_on_the_wire() {
        let mut bag = Bag::new();
        bag.set_bin("blob", vec![1_u8, 2, 3]);
        bag.set("arr", vec![1_u8, 2, 3]).unwrap();
        let decoded = Bag::from_msgpack(&bag.to_msgpack().unwrap()).unwrap();
        // A bin cannot be read as an int array and vice-versa.
        assert_eq!(decoded.get_bin("blob").unwrap(), vec![1, 2, 3]);
        assert!(matches!(
            decoded.get_bin("arr"),
            Err(Error::BagTypeMismatch { .. })
        ));
    }

    #[test]
    fn decode_rejects_non_map_and_non_string_keys() {
        // A bare integer is not a named map.
        let bytes = rmp_serde::to_vec(&42_i64).unwrap();
        assert!(matches!(
            Bag::from_msgpack(&bytes),
            Err(Error::BagDecodeFailed(_))
        ));

        // An integer-keyed map is not a *named* map.
        let mut int_keyed = Vec::new();
        rmpv::encode::write_value(
            &mut int_keyed,
            &Value::Map(vec![(Value::from(1_i64), Value::from("x"))]),
        )
        .unwrap();
        assert!(matches!(
            Bag::from_msgpack(&int_keyed),
            Err(Error::BagDecodeFailed(_))
        ));
    }

    #[test]
    fn interops_with_a_codegen_struct_shape() {
        // A bag written field-by-field decodes into the same bytes a
        // `to_vec_named` struct serialize would produce, so a typed consumer
        // can `rmp_serde::from_slice` a bag and a bag can decode a typed
        // producer's frame.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Frame {
            width: u32,
            height: u32,
            label: String,
        }
        let frame = Frame {
            width: 1920,
            height: 1080,
            label: "cam0".to_owned(),
        };
        let typed_bytes = rmp_serde::to_vec_named(&frame).unwrap();

        // Bag reads the typed producer's frame.
        let bag = Bag::from_msgpack(&typed_bytes).unwrap();
        assert_eq!(bag.get::<u32>("width").unwrap(), 1920);
        assert_eq!(bag.get::<String>("label").unwrap(), "cam0");

        // Typed consumer reads the bag's frame.
        let mut authored = Bag::new();
        authored.set("width", 1920_u32).unwrap();
        authored.set("height", 1080_u32).unwrap();
        authored.set("label", "cam0").unwrap();
        let round: Frame = rmp_serde::from_slice(&authored.to_msgpack().unwrap()).unwrap();
        assert_eq!(round, frame);
    }

    #[test]
    fn bag_macro_builds_named_map() {
        let bag = bag! {
            "width" => 1920_u32,
            "label" => "cam0",
            "enabled" => true,
        };
        assert_eq!(bag.len(), 3);
        assert_eq!(bag.get::<u32>("width").unwrap(), 1920);
        assert_eq!(bag.get::<String>("label").unwrap(), "cam0");
        assert!(bag.get::<bool>("enabled").unwrap());
    }

    #[test]
    fn serde_round_trips_through_msgpack_named_map() {
        // The serde `Serialize`/`Deserialize` impls must agree byte-for-byte
        // with the hand-rolled `to_msgpack`/`from_msgpack` wire contract, so a
        // `Bag`-typed config crosses the ABI exactly like a codegen struct.
        let bag = bag! {
            "width" => 1920_u32,
            "label" => "cam0",
            "enabled" => true,
        };
        let via_serde = rmp_serde::to_vec_named(&bag).unwrap();
        let via_wire = bag.to_msgpack().unwrap();
        assert_eq!(via_serde, via_wire);

        let decoded: Bag = rmp_serde::from_slice(&via_serde).unwrap();
        assert_eq!(decoded, bag);
    }

    #[test]
    fn serde_round_trips_through_json_object() {
        let bag = bag! { "gain" => 3_i64, "name" => "mixer" };
        let json = serde_json::to_value(&bag).unwrap();
        assert!(json.is_object());
        assert_eq!(json["gain"], serde_json::json!(3));
        let decoded: Bag = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.get::<i64>("gain").unwrap(), 3);
        assert_eq!(decoded.get::<String>("name").unwrap(), "mixer");
    }

    /// A `Bag` config is inherently tolerant: it keeps every wire field, so an
    /// "extra" key a typed struct would have to ignore is simply readable.
    #[test]
    fn bag_config_captures_every_field_including_unexpected() {
        #[derive(Serialize)]
        struct WireConfig {
            known: u32,
            future_knob: bool,
        }
        let bytes = rmp_serde::to_vec_named(&WireConfig {
            known: 7,
            future_knob: true,
        })
        .unwrap();
        let bag: Bag = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(bag.get::<u32>("known").unwrap(), 7);
        assert!(bag.get::<bool>("future_knob").unwrap());
    }

    /// Locks the "dynamic-bag config constructs" acceptance path: `Bag`
    /// satisfies the `Config` trait bound (`Default + Serialize +
    /// DeserializeOwned + PartialEq`) so a processor can declare
    /// `type Config = Bag`. Mentally revert the serde impls above and this
    /// stops compiling.
    #[test]
    fn bag_satisfies_config_bound() {
        fn assert_is_config<T: crate::processors::Config>() {}
        assert_is_config::<Bag>();
    }

    #[test]
    fn set_replaces_and_preserves_order() {
        let mut bag = Bag::new();
        bag.set("a", 1_i64).unwrap();
        bag.set("b", 2_i64).unwrap();
        bag.set("a", 10_i64).unwrap();
        assert_eq!(bag.keys().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(bag.get::<i64>("a").unwrap(), 10);
        assert!(bag.remove("a"));
        assert!(!bag.contains_key("a"));
    }
}
