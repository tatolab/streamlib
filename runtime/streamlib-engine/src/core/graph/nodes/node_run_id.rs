// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeRunId(String);

impl NodeRunId {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the inner string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for NodeRunId {
    fn default() -> Self {
        Self(format!("P{}", cuid2::create_id()))
    }
}

impl Deref for NodeRunId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Borrow<str> for NodeRunId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for NodeRunId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeRunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for NodeRunId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&String> for NodeRunId {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

impl From<&str> for NodeRunId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<NodeRunId> for String {
    fn from(id: NodeRunId) -> Self {
        id.0
    }
}

impl From<&NodeRunId> for String {
    fn from(id: &NodeRunId) -> Self {
        id.0.clone()
    }
}

impl From<&NodeRunId> for NodeRunId {
    fn from(id: &NodeRunId) -> Self {
        Self(id.0.clone())
    }
}

impl PartialEq<str> for NodeRunId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<NodeRunId> for str {
    fn eq(&self, other: &NodeRunId) -> bool {
        self == other.0
    }
}

impl PartialEq<&str> for NodeRunId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<NodeRunId> for &str {
    fn eq(&self, other: &NodeRunId) -> bool {
        *self == other.0
    }
}

impl PartialEq<String> for NodeRunId {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<NodeRunId> for String {
    fn eq(&self, other: &NodeRunId) -> bool {
        *self == other.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// msgpack round-trip preserves the bare-string `#[serde(transparent)]`
    /// wire shape. A regression to non-transparent would silently flip the
    /// wire format to a `{0: "..."}` map.
    #[test]
    fn msgpack_round_trip_preserves_transparent_shape() {
        let id = NodeRunId::from("P-test-id");
        let bytes = rmp_serde::to_vec_named(&id).expect("encode");
        let back: NodeRunId = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(id, back);
    }

    /// Wire format is a bare msgpack string, not a `{0: "..."}` map.
    /// Locks the transparent serde representation.
    #[test]
    fn msgpack_wire_is_bare_string_not_map() {
        let id = NodeRunId::from("Pabc");
        let bytes = rmp_serde::to_vec_named(&id).expect("encode");
        // msgpack `fixstr` for "Pabc": tag 0xa4 then bytes "Pabc"
        assert_eq!(bytes, vec![0xa4, b'P', b'a', b'b', b'c']);
    }

    /// Empty-id and unicode-id both survive the wire.
    #[test]
    fn msgpack_round_trip_edge_cases() {
        for s in [
            "",
            "P",
            "P-very-long-id-that-overflows-fixstr-limit-and-needs-str8",
            "P🎥-emoji",
        ] {
            let id = NodeRunId::from(s);
            let bytes = rmp_serde::to_vec_named(&id).expect("encode");
            let back: NodeRunId = rmp_serde::from_slice(&bytes).expect("decode");
            assert_eq!(id, back);
        }
    }
}
