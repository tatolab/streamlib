// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The id one runtime mints for one link request it sends.
//!
//! Minted by the requester rather than the applying runtime, because it is what
//! makes a resend idempotent: the request carries the same id however many
//! times it is sent, and the runtime that applied it once answers with the link
//! it already made. It is also the only handle a requester has on a request no
//! runtime has answered yet — `graph` renders it, and a cancel names it.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One runtime's id for one link request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkRequestUniqueId(String);

impl LinkRequestUniqueId {
    /// Mint an id for a request about to be sent.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for LinkRequestUniqueId {
    fn default() -> Self {
        Self(format!("LR{}", cuid2::create_id()))
    }
}

impl From<String> for LinkRequestUniqueId {
    fn from(minted: String) -> Self {
        Self(minted)
    }
}

impl From<&str> for LinkRequestUniqueId {
    fn from(minted: &str) -> Self {
        Self(minted.to_string())
    }
}

impl fmt::Display for LinkRequestUniqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two mints never collide, and the prefix tells a request id apart from a
    /// link id at a glance — both appear in one MCP answer.
    #[test]
    fn every_mint_is_its_own_id_and_says_what_kind_of_id_it_is() {
        let one = LinkRequestUniqueId::new();
        let another = LinkRequestUniqueId::new();
        assert_ne!(one, another);
        assert!(one.as_str().starts_with("LR"), "{one}");
    }

    /// It rides the wire as the bare string, so a peer reads the id the
    /// requester minted rather than a map wrapping it.
    #[test]
    fn it_rides_the_wire_as_the_string_it_is() {
        let id = LinkRequestUniqueId::from("LRabc123");
        let bytes = rmp_serde::to_vec_named(&id).expect("encode");
        assert_eq!(
            rmp_serde::from_slice::<serde_json::Value>(&bytes).expect("a msgpack value"),
            serde_json::json!("LRabc123")
        );
        assert_eq!(
            rmp_serde::from_slice::<LinkRequestUniqueId>(&bytes).expect("decode"),
            id
        );
    }
}
