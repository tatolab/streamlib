// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Catalog Generation
//
// Generates MoQ catalog tracks from StreamLib's processor registry.
// Maps JTD schema identifiers to MoQ track names so remote subscribers
// can discover available data streams.
//
// Wire shape (#401 phase 2): the per-track `schema` field is structured —
// `{ org, package, type, version: { major, minor, patch } }` — never a
// joined string. Legacy reverse-DNS schemas resolve to `null`.

use serde::{Deserialize, Serialize};

use crate::core::json_schema::SchemaIdentOutput;

/// A catalog entry describing a single MoQ track and its StreamLib schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoqCatalogTrackEntry {
    /// MoQ track name (derived from schema identifier unless overridden).
    pub track_name: String,
    /// Structured StreamLib schema identifier for the data on this track.
    /// `None` for legacy reverse-DNS schemas that have no structured-segment
    /// representation, or when the producer hasn't declared a schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaIdentOutput>,
    /// Source processor type that produces this track.
    pub source_processor_type: String,
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
    /// Create an empty catalog.
    pub fn new() -> Self {
        Self {
            version: 1,
            tracks: Vec::new(),
        }
    }

    /// Add a track entry to the catalog. The `schema_joined` parameter is
    /// the build-time-known joined-versioned identifier (e.g.
    /// `"@tatolab/core/EncodedVideoFrame@1.0.0"`); it's resolved to
    /// structured form via the embedded segment table. Pass an empty
    /// string when the schema is unknown — the entry's `schema` field
    /// will serialize as omitted.
    pub fn add_track(
        &mut self,
        track_name: &str,
        schema_joined: &str,
        source_processor_type: &str,
        source_port_name: &str,
    ) {
        self.tracks.push(MoqCatalogTrackEntry {
            track_name: track_name.to_string(),
            schema: SchemaIdentOutput::try_from_joined(schema_joined),
            source_processor_type: source_processor_type.to_string(),
            source_port_name: source_port_name.to_string(),
        });
    }

    /// Serialize the catalog to JSON bytes for publishing as a MoQ track.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("catalog serialization should not fail")
    }

    /// Deserialize a catalog from JSON bytes received from a MoQ track.
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
    schema_joined: &str,
    processor_type: &str,
    port_name: &str,
) -> MoqCatalogTrackEntry {
    MoqCatalogTrackEntry {
        track_name: processor_port_to_moq_track_name(processor_id, port_name),
        schema: SchemaIdentOutput::try_from_joined(schema_joined),
        source_processor_type: processor_type.to_string(),
        source_port_name: port_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_track_emits_structured_schema_for_known_wire_type() {
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track(
            "encoder/video_out",
            "@tatolab/core/EncodedVideoFrame@1.0.0",
            "H264Encoder",
            "video_out",
        );
        let entry = &catalog.tracks[0];
        let s = entry.schema.as_ref().expect("known wire type must resolve");
        assert_eq!(s.org, "tatolab");
        assert_eq!(s.package, "core");
        assert_eq!(s.type_name, "EncodedVideoFrame");
        assert_eq!(s.version.major, 1);
    }

    #[test]
    fn add_track_omits_schema_for_legacy_or_empty() {
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track("test_track", "", "", "out");
        catalog.add_track(
            "legacy_track",
            "com.streamlib.h264_encoder.config@1.0.0",
            "",
            "out",
        );
        assert!(catalog.tracks[0].schema.is_none());
        assert!(catalog.tracks[1].schema.is_none());
    }

    #[test]
    fn catalog_round_trips_through_json() {
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track(
            "encoder/video_out",
            "@tatolab/core/EncodedVideoFrame@1.0.0",
            "H264Encoder",
            "video_out",
        );
        let bytes = catalog.to_json_bytes();
        let back = MoqBroadcastCatalog::from_json_bytes(&bytes).unwrap();
        assert_eq!(back.tracks.len(), 1);
        let s = back.tracks[0]
            .schema
            .as_ref()
            .expect("schema must round-trip");
        assert_eq!(s.org, "tatolab");
        assert_eq!(s.type_name, "EncodedVideoFrame");
    }

    #[test]
    fn catalog_json_shape_is_structured_not_joined_string() {
        // Wire-format lock: the schema field on the wire is a structured
        // object, never a joined string. If a future change accidentally
        // adds a custom Serialize impl that flattens to the joined form,
        // this test catches it.
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track(
            "track",
            "@tatolab/core/VideoFrame@1.0.0",
            "Camera",
            "video",
        );
        let json: serde_json::Value =
            serde_json::from_slice(&catalog.to_json_bytes()).unwrap();
        let schema = &json["tracks"][0]["schema"];
        assert!(
            schema.is_object(),
            "schema must be a structured JSON object, not a string"
        );
        assert_eq!(schema["org"], "tatolab");
        assert_eq!(schema["package"], "core");
        assert_eq!(schema["type"], "VideoFrame");
        assert_eq!(schema["version"]["major"], 1);
    }

    #[test]
    fn catalog_entry_helper_resolves_structured() {
        let entry = catalog_entry_for_output_port(
            "encoder",
            "@tatolab/core/AudioFrame@1.0.0",
            "AudioCapture",
            "audio_out",
        );
        assert_eq!(entry.track_name, "encoder/audio_out");
        let s = entry.schema.as_ref().unwrap();
        assert_eq!(s.type_name, "AudioFrame");
    }
}
