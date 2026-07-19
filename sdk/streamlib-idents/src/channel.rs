// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Channel names — the pub/sub rendezvous identifier a port publishes to /
//! subscribes from.
//!
//! A channel name is the same string in two roles: an iceoryx2 service name
//! intra-node and (phase [L]) a Zenoh key-expression cross-node. Its charset
//! reuses the canonical org/package ident grammar (`[a-z][a-z0-9-]*`) — the
//! one charset that is simultaneously iceoryx2-service-legal and
//! Zenoh-keyexpr-legal — so the authoring model does not change when routing
//! moves cross-node. This module is the single source of truth for that
//! grammar; the SDK and the engine both validate through it rather than
//! forking a parallel copy.
//!
//! The wire carries a channel name through a fixed-width `PortKey`
//! ([`MAX_CHANNEL_NAME_BYTES`] bytes), so an over-length explicit name is a
//! hard error and a generated name is hash-suffixed to stay in bound (never
//! prefix-truncated, which would collide).

use crate::error::{IdentError, IdentResult};
use crate::ident::is_lower_alnum_or_hyphen;
use std::fmt;

/// Maximum channel-name length in UTF-8 bytes.
///
/// Pinned to the fixed `PortKey` wire capacity
/// (`streamlib_ipc_types::PortKey::MAX_NAME_BYTES`). The engine holds a
/// cross-crate assertion that the two constants agree — this crate has no
/// `streamlib-ipc-types` dependency (that crate pulls in iceoryx2), so the
/// bound is duplicated here as a plain constant and reconciled at the engine
/// layer that depends on both.
pub const MAX_CHANNEL_NAME_BYTES: usize = 63;

/// Number of lowercase-hex characters in the deterministic disambiguating
/// suffix appended when a generated channel name would overflow
/// [`MAX_CHANNEL_NAME_BYTES`]. 12 hex chars = 48 bits of the name hash.
const CHANNEL_NAME_HASH_SUFFIX_HEX_LEN: usize = 12;

/// A validated channel name.
///
/// Constructed via [`ChannelName::new`] (validating an explicit user-supplied
/// name) or [`connect_channel_name`] (deterministically generating the name
/// `connect()` assigns to both ends of a link). Both paths guarantee the
/// charset grammar and the [`MAX_CHANNEL_NAME_BYTES`] bound hold.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelName(String);

impl ChannelName {
    /// Validate and wrap an explicit, user-supplied channel name.
    ///
    /// An over-length name is [`IdentError::ChannelNameTooLong`] — never
    /// truncated. Charset violations surface as the matching named variant.
    pub fn new(s: impl Into<String>) -> IdentResult<Self> {
        let s = s.into();
        validate_channel_name(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ChannelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate a channel name against the canonical grammar
/// (`[a-z][a-z0-9-]*`, at most [`MAX_CHANNEL_NAME_BYTES`] UTF-8 bytes).
pub fn validate_channel_name(s: &str) -> IdentResult<()> {
    if s.is_empty() {
        return Err(IdentError::EmptyChannelName);
    }
    if s.len() > MAX_CHANNEL_NAME_BYTES {
        return Err(IdentError::ChannelNameTooLong {
            name: s.to_string(),
            len: s.len(),
            max: MAX_CHANNEL_NAME_BYTES,
        });
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_lowercase() {
        return Err(IdentError::ChannelNameMustStartWithLowercase(s.to_string()));
    }
    for c in chars {
        if !is_lower_alnum_or_hyphen(c) {
            return Err(IdentError::InvalidChannelNameCharacter(s.to_string(), c));
        }
    }
    Ok(())
}

/// Deterministically derive the channel name `connect()` assigns to both ends
/// of a link: `{src_processor}-{src_output}--{dst_processor}-{dst_input}`.
///
/// The `--` double-hyphen separates the source `proc-port` pair from the
/// destination pair; single hyphens join a processor to its port. All four
/// inputs are already lowercase idents, so the joined form is grammar-legal by
/// construction. If it overflows [`MAX_CHANNEL_NAME_BYTES`], the human-readable
/// prefix is shortened and a stable hash of the *full* joined form is appended
/// (`{prefix}-{hash}`) — a pure function of the inputs that stays unique rather
/// than a prefix truncation that would collide two long links onto one channel.
///
/// The result is always a valid [`ChannelName`]; construction cannot fail.
pub fn connect_channel_name(
    src_processor: &str,
    src_output: &str,
    dst_processor: &str,
    dst_input: &str,
) -> ChannelName {
    let joined = format!("{src_processor}-{src_output}--{dst_processor}-{dst_input}");
    if joined.len() <= MAX_CHANNEL_NAME_BYTES {
        return ChannelName(joined);
    }

    let hash = fnv1a_64(joined.as_bytes());
    let suffix = format!("{hash:016x}");
    let suffix = &suffix[suffix.len() - CHANNEL_NAME_HASH_SUFFIX_HEX_LEN..];

    // Reserve room for the `-` joiner and the hex suffix, then keep as much of
    // the human-readable prefix as fits on a UTF-8 char boundary.
    let prefix_budget = MAX_CHANNEL_NAME_BYTES - 1 - CHANNEL_NAME_HASH_SUFFIX_HEX_LEN;
    let mut cut = prefix_budget.min(joined.len());
    while !joined.is_char_boundary(cut) {
        cut -= 1;
    }
    let prefix = &joined[..cut];
    ChannelName(format!("{prefix}-{suffix}"))
}

/// FNV-1a 64-bit — a fixed, platform-stable hash so a regenerated channel name
/// is byte-identical across builds and runtimes (`std`'s `DefaultHasher` is
/// explicitly not stable across versions).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_names() {
        for name in ["a", "camera-out", "cam-frame--sink-in", "a1-b2-c3"] {
            validate_channel_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(ChannelName::new(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            validate_channel_name(""),
            Err(IdentError::EmptyChannelName)
        ));
    }

    #[test]
    fn rejects_uppercase_and_leading_non_alpha() {
        assert!(matches!(
            validate_channel_name("Camera"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
        assert!(matches!(
            validate_channel_name("1cam"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
        assert!(matches!(
            validate_channel_name("-cam"),
            Err(IdentError::ChannelNameMustStartWithLowercase(_))
        ));
    }

    #[test]
    fn rejects_illegal_charset() {
        // Underscore, dot, slash — none are iceoryx2/keyexpr-safe.
        for (name, bad) in [("cam_out", '_'), ("cam.out", '.'), ("cam/out", '/')] {
            assert_eq!(
                validate_channel_name(name),
                Err(IdentError::InvalidChannelNameCharacter(name.to_string(), bad))
            );
        }
    }

    #[test]
    fn double_hyphen_separator_is_grammar_legal() {
        // The connect() separator `--` must pass — the charset allows runs of
        // hyphens, which is what keeps the generated name a single valid ident.
        validate_channel_name("src-out--dst-in").unwrap();
    }

    #[test]
    fn explicit_over_length_name_is_a_hard_error_not_truncated() {
        // Mental-revert guard for the whole grammar decision: an explicit
        // user-supplied name past the wire bound must error, never truncate.
        let long = "a".repeat(MAX_CHANNEL_NAME_BYTES + 1);
        assert_eq!(long.len(), 64);
        assert_eq!(
            ChannelName::new(&long),
            Err(IdentError::ChannelNameTooLong {
                name: long.clone(),
                len: 64,
                max: MAX_CHANNEL_NAME_BYTES,
            })
        );
    }

    #[test]
    fn exact_bound_name_is_accepted() {
        let exact = "a".repeat(MAX_CHANNEL_NAME_BYTES);
        assert_eq!(exact.len(), MAX_CHANNEL_NAME_BYTES);
        assert!(ChannelName::new(&exact).is_ok());
    }

    #[test]
    fn connect_name_is_deterministic() {
        let a = connect_channel_name("cam", "frame", "sink", "in");
        let b = connect_channel_name("cam", "frame", "sink", "in");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "cam-frame--sink-in");
    }

    #[test]
    fn connect_name_is_a_valid_channel_name() {
        let name = connect_channel_name("camera", "output", "encoder", "input");
        validate_channel_name(name.as_str()).unwrap();
    }

    #[test]
    fn connect_name_distinct_endpoints_are_unique() {
        // Distinct links must land on distinct channels — the whole point of a
        // per-link generated name.
        let a = connect_channel_name("cam", "frame", "sink", "in");
        let b = connect_channel_name("cam", "frame", "sink", "aux");
        let c = connect_channel_name("cam", "alt", "sink", "in");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn connect_name_over_bound_is_hash_suffixed_not_truncated() {
        // Two links whose long human prefixes share the first 50 bytes must NOT
        // collapse onto one channel — the hash suffix keeps them distinct while
        // both stay inside the wire bound.
        let long = "verylongprocessorname".repeat(3); // 63 bytes, > budget
        let a = connect_channel_name(&long, "outputport", "downstreamsink", "inputport");
        let b = connect_channel_name(&long, "outputport", "downstreamsink", "otherport");
        assert!(a.as_str().len() <= MAX_CHANNEL_NAME_BYTES);
        assert!(b.as_str().len() <= MAX_CHANNEL_NAME_BYTES);
        validate_channel_name(a.as_str()).unwrap();
        validate_channel_name(b.as_str()).unwrap();
        assert_ne!(a, b, "hash suffix must disambiguate prefix-colliding links");
        // Deterministic even on the hashed path.
        assert_eq!(
            a,
            connect_channel_name(&long, "outputport", "downstreamsink", "inputport")
        );
    }
}
