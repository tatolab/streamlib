// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The single per-port delivery knob on the authoring surface.
//!
//! A [`DeliveryProfile`] is the one word an author writes at a port declaration
//! site (`#[processor]` attribute / `@processor` decorator). It resolves to the
//! three transport settings the engine used to expose as four separate knobs
//! (`read_mode`, `overflow`, `buffer_size`, `max_queued_messages`): the
//! consumer-side drain order ([`ReadMode`]), the producer-side overflow policy
//! ([`Overflow`]), and the ring depth. When a port declares no profile, the
//! default derives from the wire type's [`FlowClass`], carried in the schema
//! `metadata` block — so authors mostly never touch it.

use serde::{Deserialize, Serialize};

use super::overflow::Overflow;
use super::read_mode::ReadMode;

/// The one per-port delivery knob. Each profile bundles a fixed
/// (drain order, overflow policy, ring depth) triple; see [`DeliveryProfile::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryProfile {
    /// Newest-wins: drain to the latest sample, evict oldest under pressure,
    /// shallow ring. State snapshots — video frames, control state — where a
    /// stale sample has no value once a fresher one exists.
    Latest,
    /// FIFO with a bounded backlog: read next in order, evict + count the
    /// oldest under sustained overrun, deeper ring. Sample streams — audio,
    /// encoded frames — where order matters but the producer must never block.
    EverySample,
    /// Lossless FIFO: read next in order, the producer blocks rather than
    /// drop, deeper ring. File writers, muxers, loggers where every sample
    /// must be delivered. Explicit-only — no [`FlowClass`] resolves here.
    Lossless,
}

/// The resolved transport triple a [`DeliveryProfile`] expands to at wire time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryResolution {
    /// Consumer-side drain order applied by the destination's mailbox.
    pub drain_order: ReadMode,
    /// Producer-side overflow policy sizing the channel's `enable_safe_overflow`.
    pub overflow: Overflow,
    /// Ring depth — both the iceoryx2 subscriber buffer and the host mailbox
    /// capacity.
    pub depth: usize,
}

impl DeliveryProfile {
    /// Ring depth for [`DeliveryProfile::Latest`].
    pub const LATEST_DEPTH: usize = 4;
    /// Ring depth for [`DeliveryProfile::EverySample`] and [`DeliveryProfile::Lossless`].
    pub const STREAM_DEPTH: usize = 16;

    /// Expand this profile into its fixed (drain order, overflow, depth) triple.
    pub fn resolve(self) -> DeliveryResolution {
        match self {
            DeliveryProfile::Latest => DeliveryResolution {
                drain_order: ReadMode::SkipToLatest,
                overflow: Overflow::DropOldest,
                depth: Self::LATEST_DEPTH,
            },
            DeliveryProfile::EverySample => DeliveryResolution {
                drain_order: ReadMode::ReadNextInOrder,
                overflow: Overflow::DropOldest,
                depth: Self::STREAM_DEPTH,
            },
            DeliveryProfile::Lossless => DeliveryResolution {
                drain_order: ReadMode::ReadNextInOrder,
                overflow: Overflow::Block,
                depth: Self::STREAM_DEPTH,
            },
        }
    }

    /// Parse an author-declared profile string.
    ///
    /// Recognized values: `"latest"`, `"every_sample"`, `"lossless"`. Unknown
    /// values surface as a structured configuration error so a typo at the
    /// declaration site is rejected at wire time, not silently defaulted.
    pub fn from_manifest_str(value: &str) -> Result<Self, String> {
        match value {
            "latest" => Ok(Self::Latest),
            "every_sample" => Ok(Self::EverySample),
            "lossless" => Ok(Self::Lossless),
            other => Err(format!(
                "unknown delivery_profile value '{other}', expected 'latest', \
                 'every_sample', or 'lossless'"
            )),
        }
    }

    /// The canonical manifest string for this profile.
    pub fn as_manifest_str(self) -> &'static str {
        match self {
            DeliveryProfile::Latest => "latest",
            DeliveryProfile::EverySample => "every_sample",
            DeliveryProfile::Lossless => "lossless",
        }
    }
}

/// The per-wire-type data class carried in a schema's `metadata.flow_class`.
/// It sets the *default* [`DeliveryProfile`] for any port carrying that type,
/// which a port-site profile override still wins over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowClass {
    /// Ordered samples where every one matters — audio, encoded frames.
    /// Defaults to [`DeliveryProfile::EverySample`].
    SampleStream,
    /// Latest-state snapshots where a stale sample has no value once a fresher
    /// one exists — video frames, control state. Defaults to
    /// [`DeliveryProfile::Latest`].
    StateStream,
}

impl FlowClass {
    /// Parse a schema-declared `metadata.flow_class` string.
    ///
    /// Recognized values: `"sample_stream"` and `"state_stream"`. `lossless`
    /// is deliberately absent — a lossless profile is an explicit port-site
    /// choice, never a type-level default (a wire type doesn't know whether a
    /// given consumer can afford to backpressure its producer).
    pub fn from_manifest_str(value: &str) -> Result<Self, String> {
        match value {
            "sample_stream" => Ok(Self::SampleStream),
            "state_stream" => Ok(Self::StateStream),
            other => Err(format!(
                "unknown flow_class value '{other}', expected 'sample_stream' or 'state_stream'"
            )),
        }
    }

    /// The default [`DeliveryProfile`] a port carrying this flow class resolves
    /// to when it declares no explicit profile override.
    pub fn default_profile(self) -> DeliveryProfile {
        match self {
            FlowClass::SampleStream => DeliveryProfile::EverySample,
            FlowClass::StateStream => DeliveryProfile::Latest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_resolves_to_skip_drop_shallow() {
        let r = DeliveryProfile::Latest.resolve();
        assert_eq!(r.drain_order, ReadMode::SkipToLatest);
        assert_eq!(r.overflow, Overflow::DropOldest);
        assert_eq!(r.depth, 4);
        assert!(r.overflow.enable_safe_overflow());
    }

    #[test]
    fn every_sample_resolves_to_fifo_drop_deep() {
        let r = DeliveryProfile::EverySample.resolve();
        assert_eq!(r.drain_order, ReadMode::ReadNextInOrder);
        assert_eq!(r.overflow, Overflow::DropOldest);
        assert_eq!(r.depth, 16);
        assert!(r.overflow.enable_safe_overflow());
    }

    #[test]
    fn lossless_resolves_to_fifo_block_deep() {
        let r = DeliveryProfile::Lossless.resolve();
        assert_eq!(r.drain_order, ReadMode::ReadNextInOrder);
        assert_eq!(r.overflow, Overflow::Block);
        assert_eq!(r.depth, 16);
        assert!(
            !r.overflow.enable_safe_overflow(),
            "lossless must NOT enable safe overflow — the producer backpressures"
        );
    }

    #[test]
    fn profile_parses_known_and_rejects_unknown() {
        assert_eq!(
            DeliveryProfile::from_manifest_str("latest").unwrap(),
            DeliveryProfile::Latest
        );
        assert_eq!(
            DeliveryProfile::from_manifest_str("every_sample").unwrap(),
            DeliveryProfile::EverySample
        );
        assert_eq!(
            DeliveryProfile::from_manifest_str("lossless").unwrap(),
            DeliveryProfile::Lossless
        );
        let err = DeliveryProfile::from_manifest_str("Latest").unwrap_err();
        assert!(err.contains("every_sample"));
    }

    #[test]
    fn manifest_str_roundtrips() {
        for p in [
            DeliveryProfile::Latest,
            DeliveryProfile::EverySample,
            DeliveryProfile::Lossless,
        ] {
            assert_eq!(
                DeliveryProfile::from_manifest_str(p.as_manifest_str()).unwrap(),
                p
            );
        }
    }

    #[test]
    fn flow_class_defaults_match_landmines() {
        // Landmine #1: video_frame (skip_to_latest) is state_stream → latest.
        assert_eq!(
            FlowClass::StateStream.default_profile(),
            DeliveryProfile::Latest
        );
        // sample_stream (audio, encoded frames) → every_sample.
        assert_eq!(
            FlowClass::SampleStream.default_profile(),
            DeliveryProfile::EverySample
        );
    }

    #[test]
    fn flow_class_parses_known_and_rejects_lossless() {
        assert_eq!(
            FlowClass::from_manifest_str("sample_stream").unwrap(),
            FlowClass::SampleStream
        );
        assert_eq!(
            FlowClass::from_manifest_str("state_stream").unwrap(),
            FlowClass::StateStream
        );
        // Landmine #3: lossless never resolves from a flow class.
        assert!(FlowClass::from_manifest_str("lossless").is_err());
    }
}
