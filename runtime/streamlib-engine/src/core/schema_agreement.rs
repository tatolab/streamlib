// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-read wire-tag classification for an inbound frame.
//!
//! Compares the [`SchemaIdentWire`] stamped on an inbound frame against a
//! consumer port's expected tag. An unset tag on either side is the tolerant
//! wildcard and never mismatches; only two set tags with different
//! `(org, package, type)` identity tuples are a mismatch. Comparison is
//! version-blind, matching every other resolution surface in the runtime: a
//! Rust cdylib port carries the `0.0.0` version-free sentinel while a
//! manifest-resolved Python/Deno port carries its schema owner's package
//! version, and those two ends describe the same schema.
//!
//! [`SchemaIdentWire`]: crate::iceoryx2::SchemaIdentWire

use crate::iceoryx2::SchemaIdentWire;

/// Whether a producer schema and a consumer schema agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaAgreement {
    /// The two ends agree — concrete schemas sharing an identity tuple, or a
    /// wildcard (unset) on at least one side.
    Compatible,
    /// Both ends declare a concrete schema and their identity tuples differ.
    Mismatch,
}

/// Classify a stamped inbound-frame tag against a consumer port's expected tag.
///
/// An [unset][SchemaIdentWire::is_unset] tag on either side is the wildcard and
/// never mismatches; two set tags with different identity tuples are a
/// mismatch. Version-blind.
pub fn classify_wire_schema_agreement(
    stamped: &SchemaIdentWire,
    expected: &SchemaIdentWire,
) -> SchemaAgreement {
    if stamped.is_unset() || expected.is_unset() || stamped.matches_schema_tuple(expected) {
        SchemaAgreement::Compatible
    } else {
        SchemaAgreement::Mismatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_agreement_treats_unset_as_wildcard() {
        let set = SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();
        let unset = SchemaIdentWire::default();
        assert_eq!(
            classify_wire_schema_agreement(&unset, &set),
            SchemaAgreement::Compatible,
        );
        assert_eq!(
            classify_wire_schema_agreement(&set, &unset),
            SchemaAgreement::Compatible,
        );
        assert_eq!(
            classify_wire_schema_agreement(&set, &set),
            SchemaAgreement::Compatible,
        );
    }

    /// Revert lock for the runtime read path: two distinct stamped/expected
    /// tags MUST classify as a mismatch. Reverting the per-frame tag read to
    /// "ignore the tag" collapses this to `Compatible` and fails here.
    #[test]
    fn wire_agreement_flags_distinct_set_tags() {
        let stamped =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 0, 0).unwrap();
        let expected =
            SchemaIdentWire::from_segments("tatolab", "core", "AudioFrame", 1, 0, 0).unwrap();
        assert_eq!(
            classify_wire_schema_agreement(&stamped, &expected),
            SchemaAgreement::Mismatch,
        );
    }

    /// Revert lock (#1654): the per-frame read path is version-blind. The
    /// stamped tag and the port's expected tag are resolved from asymmetric
    /// sources, so a version-sensitive comparison here warns once per port on
    /// every cross-language link.
    #[test]
    fn wire_agreement_ignores_the_version_axis() {
        let sentinel =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 0, 0, 0).unwrap();
        let versioned =
            SchemaIdentWire::from_segments("tatolab", "core", "VideoFrame", 1, 2, 3).unwrap();
        assert_eq!(
            classify_wire_schema_agreement(&sentinel, &versioned),
            SchemaAgreement::Compatible,
        );
        assert_eq!(
            classify_wire_schema_agreement(&versioned, &sentinel),
            SchemaAgreement::Compatible,
        );

        // The version axis is ignored; the identity tuple is not.
        let other_package =
            SchemaIdentWire::from_segments("tatolab", "vision", "VideoFrame", 1, 2, 3).unwrap();
        assert_eq!(
            classify_wire_schema_agreement(&versioned, &other_package),
            SchemaAgreement::Mismatch,
        );
    }
}
