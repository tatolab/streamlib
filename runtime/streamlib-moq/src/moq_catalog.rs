// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ catalog generation.
//!
//! Attributes each MoQ track to the processor that produces it, so a remote
//! subscriber discovering a broadcast can tell what is on each track. The
//! attribution is the producing processor's class import path — the same
//! string the control plane's `type` field carries.
//!
//! Tracks carry no schema. A link is pure plumbing and a bag is
//! self-describing, so there is no declared type for a catalog to publish;
//! a subscriber casts at read time like any other consumer.

use serde::{Deserialize, Serialize};
use streamlib_processor_schema::ProcessorClassImportPath;

/// A catalog entry describing a single MoQ track.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoqCatalogTrackEntry {
    /// MoQ track name (derived from the producing processor and port).
    pub track_name: String,
    /// The class import path of the processor producing this track. `None`
    /// when no source processor is known (e.g. raw track names without a
    /// producer attribution — treat as opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_processor_type: Option<ProcessorClassImportPath>,
    /// Source output port name.
    pub source_port_name: String,
}

/// A full MoQ catalog describing all tracks in a broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoqBroadcastCatalog {
    /// Catalog format version.
    pub version: u32,
    /// List of tracks available in this broadcast.
    pub tracks: Vec<MoqCatalogTrackEntry>,
}

impl MoqBroadcastCatalog {
    pub fn new() -> Self {
        Self {
            version: 1,
            tracks: Vec::new(),
        }
    }

    /// Add a track entry. `source_processor_type` is `None` when the producing
    /// processor is not known.
    pub fn add_track(
        &mut self,
        track_name: &str,
        source_processor_type: Option<&ProcessorClassImportPath>,
        source_port_name: &str,
    ) {
        self.tracks.push(MoqCatalogTrackEntry {
            track_name: track_name.to_string(),
            source_processor_type: source_processor_type.cloned(),
            source_port_name: source_port_name.to_string(),
        });
    }

    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("catalog serialization should not fail")
    }

    pub fn from_json_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

impl Default for MoqBroadcastCatalog {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a processor ID and port name to a MoQ track name.
///
/// Uses `{processor_id}/{port_name}` format to avoid collisions when
/// multiple processors output the same schema type.
pub fn processor_port_to_moq_track_name(processor_id: &str, port_name: &str) -> String {
    format!("{}/{}", processor_id, port_name)
}

/// Generate a catalog entry for a single output port.
pub fn catalog_entry_for_output_port(
    processor_id: &str,
    processor_type: &ProcessorClassImportPath,
    port_name: &str,
) -> MoqCatalogTrackEntry {
    MoqCatalogTrackEntry {
        track_name: processor_port_to_moq_track_name(processor_id, port_name),
        source_processor_type: Some(processor_type.clone()),
        source_port_name: port_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_path(path: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(path).expect("the fixture path names a class")
    }

    #[test]
    fn add_track_attributes_the_producing_class() {
        let mut catalog = MoqBroadcastCatalog::new();
        let encoder = import_path("my_app.codecs:H264Encoder");
        catalog.add_track("encoder/video_out", Some(&encoder), "video_out");

        let entry = &catalog.tracks[0];
        assert_eq!(entry.track_name, "encoder/video_out");
        assert_eq!(entry.source_processor_type.as_ref(), Some(&encoder));
        assert_eq!(entry.source_port_name, "video_out");
    }

    #[test]
    fn add_track_omits_the_source_when_no_producer_is_known() {
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track("test_track", None, "out");
        assert!(catalog.tracks[0].source_processor_type.is_none());
    }

    /// Wire-format lock: the producing processor is a plain string on the
    /// catalog wire, and the catalog carries no `schema` key at all — a link
    /// declares no type for one to name.
    #[test]
    fn catalog_json_names_the_class_as_a_string_and_carries_no_schema() {
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track(
            "track",
            Some(&import_path("my_app.codecs:H264Encoder")),
            "video",
        );
        let json: serde_json::Value = serde_json::from_slice(&catalog.to_json_bytes()).unwrap();
        let track = &json["tracks"][0];

        assert_eq!(
            track["source_processor_type"],
            serde_json::Value::String("my_app.codecs:H264Encoder".to_string()),
            "the producing class is the bare path, not an object wrapping one"
        );
        assert!(
            track.get("schema").is_none(),
            "a track carries no schema — there is no type layer to publish: {track}"
        );
    }

    #[test]
    fn catalog_round_trips_through_json() {
        let mut catalog = MoqBroadcastCatalog::new();
        let encoder = import_path("my_app.codecs:H264Encoder");
        catalog.add_track("encoder/video_out", Some(&encoder), "video_out");

        let back = MoqBroadcastCatalog::from_json_bytes(&catalog.to_json_bytes()).unwrap();
        assert_eq!(back.tracks.len(), 1);
        assert_eq!(back.tracks[0].source_processor_type.as_ref(), Some(&encoder));
        assert_eq!(back.version, 1);
    }

    #[test]
    fn the_entry_helper_names_the_track_after_the_processor_and_port() {
        let entry = catalog_entry_for_output_port(
            "encoder",
            &import_path("my_app.audio:AudioCapture"),
            "audio_out",
        );
        assert_eq!(entry.track_name, "encoder/audio_out");
        assert_eq!(
            entry.source_processor_type.as_ref(),
            Some(&import_path("my_app.audio:AudioCapture"))
        );
    }
}
