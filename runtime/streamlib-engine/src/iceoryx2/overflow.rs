// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Producer-side overflow policy for an iceoryx2 service.
//!
//! No longer an authoring knob — it is the producer-side half a
//! [`DeliveryProfile`](crate::iceoryx2::DeliveryProfile) resolves to. The
//! engine derives the iceoryx2 service-level `enable_safe_overflow` setting
//! from it at wire time.

use serde::{Deserialize, Serialize};

/// How an iceoryx2 service behaves when the subscriber's shared-memory
/// buffer is full and the producer tries to publish another sample.
///
/// Pairs with [`ReadMode`](crate::iceoryx2::ReadMode), the consumer-side drain
/// order; a [`DeliveryProfile`](crate::iceoryx2::DeliveryProfile) resolves to
/// both. The two are orthogonal: the drain order decides how the consumer pops
/// samples; overflow decides whether the producer ever blocks.
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
    fn enable_safe_overflow_maps_to_drop_oldest() {
        assert!(Overflow::DropOldest.enable_safe_overflow());
        assert!(!Overflow::Block.enable_safe_overflow());
    }
}
