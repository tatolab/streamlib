// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What kind of content an output port carries, as the MoQ gateway lists it.
//!
//! Guessed from the port's names before anything is read, and corrected by the
//! first bag the gateway actually reads — the names are a hint a processor
//! author chose, the bag is what crosses.

use crate::core::json_schema::MoqGatewayTrackKind;
use crate::core::runtime::mesh::a_bags_top_level_surface_id::TopLevelBagKeysTheMoqGatewayReads;

impl MoqGatewayTrackKind {
    /// The kind a port's names suggest, before any of its bags has been read.
    pub(crate) fn inferred_from_the_port_names(
        processor_display_name: &str,
        port_name: &str,
    ) -> Self {
        let port = port_name.to_ascii_lowercase();
        let processor = processor_display_name.to_ascii_lowercase();
        if port.contains("encoded_video") {
            return Self::Video;
        }
        if port.contains("encoded_audio") {
            return Self::Audio;
        }
        if port.contains("detect") || processor.contains("detect") {
            return Self::Detections;
        }
        let a_known_surface_producer = ["camera", "test pattern", "testpattern", "decoder"]
            .iter()
            .any(|producer| processor.contains(producer));
        if port == "video" && a_known_surface_producer {
            return Self::Surface;
        }
        Self::Bags
    }

    /// The kind one bag this port published shows it to be.
    pub(crate) fn shown_by_a_bag(bag_keys: &TopLevelBagKeysTheMoqGatewayReads) -> Self {
        if bag_keys.carries_a_bitstream {
            return match bag_keys.codec.as_deref() {
                Some("opus" | "aac" | "pcm" | "flac") => Self::Audio,
                Some("h264" | "h265" | "hevc" | "av1" | "vp8" | "vp9") => Self::Video,
                _ => Self::Unknown,
            };
        }
        if bag_keys.names_a_surface {
            return Self::Surface;
        }
        if bag_keys.carries_a_detections_array {
            return Self::Detections;
        }
        Self::Bags
    }

    /// Whether a sync point on this kind of track opens a new MoQ group.
    ///
    /// Video only: every Opus packet is a sync point, and a subscriber keeps
    /// only its newest group, so a group per packet loses packets whenever two
    /// groups' streams overlap on the relay path.
    pub(crate) fn is_cut_at_sync_points(self) -> bool {
        self == Self::Video
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_names_suggest_a_kind_before_any_bag_is_read() {
        let inferred = MoqGatewayTrackKind::inferred_from_the_port_names;
        assert_eq!(
            inferred("H264 Encoder 1", "encoded_video"),
            MoqGatewayTrackKind::Video
        );
        assert_eq!(
            inferred("Opus Encoder 1", "encoded_audio"),
            MoqGatewayTrackKind::Audio
        );
        assert_eq!(
            inferred("Yolo 1", "detections"),
            MoqGatewayTrackKind::Detections
        );
        assert_eq!(
            inferred("Object Detector 2", "out"),
            MoqGatewayTrackKind::Detections
        );
        assert_eq!(
            inferred("Camera Source 1", "video"),
            MoqGatewayTrackKind::Surface
        );
        assert_eq!(
            inferred("Test Pattern Source 1", "video"),
            MoqGatewayTrackKind::Surface
        );
        assert_eq!(inferred("Blur 1", "video"), MoqGatewayTrackKind::Bags);
        assert_eq!(inferred("Ticker 1", "out"), MoqGatewayTrackKind::Bags);
    }

    fn keys(
        configure: impl FnOnce(&mut TopLevelBagKeysTheMoqGatewayReads),
    ) -> TopLevelBagKeysTheMoqGatewayReads {
        let mut bag_keys = TopLevelBagKeysTheMoqGatewayReads::default();
        configure(&mut bag_keys);
        bag_keys
    }

    #[test]
    fn the_first_bag_corrects_what_the_names_suggested() {
        let shown = MoqGatewayTrackKind::shown_by_a_bag;
        assert_eq!(
            shown(&keys(|k| {
                k.carries_a_bitstream = true;
                k.codec = Some("h264".into());
            })),
            MoqGatewayTrackKind::Video
        );
        assert_eq!(
            shown(&keys(|k| {
                k.carries_a_bitstream = true;
                k.codec = Some("opus".into());
            })),
            MoqGatewayTrackKind::Audio
        );
        assert_eq!(
            shown(&keys(|k| k.carries_a_bitstream = true)),
            MoqGatewayTrackKind::Unknown
        );
        assert_eq!(
            shown(&keys(|k| k.names_a_surface = true)),
            MoqGatewayTrackKind::Surface
        );
        assert_eq!(
            shown(&keys(|k| k.carries_a_detections_array = true)),
            MoqGatewayTrackKind::Detections
        );
        assert_eq!(shown(&keys(|_| {})), MoqGatewayTrackKind::Bags);
    }

    #[test]
    fn only_a_video_track_opens_a_group_at_each_sync_point() {
        assert!(MoqGatewayTrackKind::Video.is_cut_at_sync_points());
        for kind in [
            MoqGatewayTrackKind::Audio,
            MoqGatewayTrackKind::Bags,
            MoqGatewayTrackKind::Detections,
            MoqGatewayTrackKind::Surface,
            MoqGatewayTrackKind::Unknown,
        ] {
            assert!(
                !kind.is_cut_at_sync_points(),
                "{kind:?} is cut at sync points"
            );
        }
    }

    #[test]
    fn it_renders_as_the_listings_lowercase_words() {
        assert_eq!(
            serde_json::to_value(MoqGatewayTrackKind::Detections).unwrap(),
            serde_json::json!("detections")
        );
    }
}
