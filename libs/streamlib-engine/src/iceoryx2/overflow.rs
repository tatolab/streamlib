// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Producer-side overflow policy for an iceoryx2 service.
//!
//! Declared on the destination's input port; the engine derives the
//! iceoryx2 service-level `enable_safe_overflow` setting at wire time.
//! See `docs/learnings/iceoryx2-overflow-vs-readmode.md` (companion
//! to [`ReadMode`](crate::iceoryx2::ReadMode)).

use serde::{Deserialize, Serialize};

/// How an iceoryx2 service behaves when the subscriber's shared-memory
/// buffer is full and the producer tries to publish another sample.
///
/// Pairs with [`ReadMode`](crate::iceoryx2::ReadMode) which controls the
/// consumer-side drain order. The two are orthogonal: `read_mode`
/// decides how the consumer pops samples; `overflow` decides whether
/// the producer ever blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overflow {
    /// Buffer evicts oldest sample to make room; publisher never blocks.
    /// The realtime-media default — producer represents real-world
    /// time advancing and must not be made to wait. Glitches on the
    /// consumer side beat freezing the whole pipeline.
    #[default]
    DropOldest,
    /// Producer blocks until the consumer drains a slot. Use only when
    /// every sample must be delivered in order — file writers, muxers,
    /// loggers — and the consumer's mailbox `buffer_size` is sized for
    /// the expected hiccup envelope.
    Block,
}

impl Overflow {
    /// Parse a manifest-declared overflow string.
    ///
    /// Recognized values: `"drop_oldest"` and `"block"`. Unknown values
    /// surface as a structured configuration error so a typo at the
    /// manifest level (`drop-oldest`, `Block`) is rejected at wire
    /// time, not silently treated as default.
    pub fn from_manifest_str(value: &str) -> Result<Self, String> {
        match value {
            "drop_oldest" => Ok(Self::DropOldest),
            "block" => Ok(Self::Block),
            other => Err(format!(
                "unknown overflow value '{other}', expected 'drop_oldest' or 'block'"
            )),
        }
    }

    /// Returns true when the iceoryx2 service-level
    /// `enable_safe_overflow` flag should be set.
    pub fn enable_safe_overflow(self) -> bool {
        matches!(self, Self::DropOldest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_drop_oldest() {
        assert_eq!(Overflow::default(), Overflow::DropOldest);
    }

    #[test]
    fn parses_known_strings() {
        assert_eq!(
            Overflow::from_manifest_str("drop_oldest").unwrap(),
            Overflow::DropOldest
        );
        assert_eq!(Overflow::from_manifest_str("block").unwrap(), Overflow::Block);
    }

    #[test]
    fn rejects_unknown_strings() {
        let err = Overflow::from_manifest_str("drop-oldest").unwrap_err();
        assert!(err.contains("drop_oldest"));
        assert!(err.contains("block"));
    }

    #[test]
    fn enable_safe_overflow_maps_to_drop_oldest() {
        assert!(Overflow::DropOldest.enable_safe_overflow());
        assert!(!Overflow::Block.enable_safe_overflow());
    }
}
