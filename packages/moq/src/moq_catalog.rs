// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ catalog generation.
//!
//! Maps StreamLib schema identifiers to MoQ track names so remote
//! subscribers can discover available data streams. Every catalog field
//! that names a schema or processor is the structured 4-field record
//! (`{ org, package, type, version: { major, minor, patch } }`).
//! Joined strings like `@org/pkg/Type@v` are `Display` form only —
//! they never round-trip through a parser at the structured boundary.

use serde::{Deserialize, Serialize};
use streamlib::sdk::descriptors::SchemaIdent;
use streamlib::sdk::json_schema::SchemaIdentOutput;

/// A catalog entry describing a single MoQ track and its StreamLib schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoqCatalogTrackEntry {
    /// MoQ track name (derived from schema identifier unless overridden).
    pub track_name: String,
    /// Structured StreamLib schema identifier for the data on this track.
    /// `None` when the producer hasn't declared a schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaIdentOutput>,
    /// Structured source processor identity. `None` when no source
    /// processor is known (e.g. raw track names without a producer
    /// attribution — treat as opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_processor_type: Option<SchemaIdentOutput>,
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

    /// Add a track entry. Both `schema` and `source_processor_type`
    /// are structured `SchemaIdent` references — `None` when not
    /// declared.
    pub fn add_track(
        &mut self,
        track_name: &str,
        schema: Option<&SchemaIdent>,
        source_processor_type: Option<&SchemaIdent>,
        source_port_name: &str,
    ) {
        self.tracks.push(MoqCatalogTrackEntry {
            track_name: track_name.to_string(),
            schema: schema.map(SchemaIdentOutput::from),
            source_processor_type: source_processor_type.map(SchemaIdentOutput::from),
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
    schema: Option<&SchemaIdent>,
    processor_type: &SchemaIdent,
    port_name: &str,
) -> MoqCatalogTrackEntry {
    MoqCatalogTrackEntry {
        track_name: processor_port_to_moq_track_name(processor_id, port_name),
        schema: schema.map(SchemaIdentOutput::from),
        source_processor_type: Some(SchemaIdentOutput::from(processor_type)),
        source_port_name: port_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::descriptors::{Org, Package, SemVer, TypeName};

    fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    #[test]
    fn add_track_emits_structured_schema_for_known_wire_type() {
        let mut catalog = MoqBroadcastCatalog::new();
        let schema = ident("tatolab", "core", "EncodedVideoFrame", SemVer::new(1, 0, 0));
        let proc_type = ident("tatolab", "h264", "H264Encoder", SemVer::new(1, 0, 0));
        catalog.add_track(
            "encoder/video_out",
            Some(&schema),
            Some(&proc_type),
            "video_out",
        );
        let entry = &catalog.tracks[0];
        let s = entry.schema.as_ref().expect("known wire type must resolve");
        assert_eq!(s.org, "tatolab");
        assert_eq!(s.package, "core");
        assert_eq!(s.type_name, "EncodedVideoFrame");
        assert_eq!(s.version.major, 1);
        let p = entry
            .source_processor_type
            .as_ref()
            .expect("source processor must resolve");
        assert_eq!(p.type_name, "H264Encoder");
    }

    #[test]
    fn add_track_omits_schema_when_unknown() {
        let mut catalog = MoqBroadcastCatalog::new();
        catalog.add_track("test_track", None, None, "out");
        assert!(catalog.tracks[0].schema.is_none());
        assert!(catalog.tracks[0].source_processor_type.is_none());
    }

    #[test]
    fn catalog_round_trips_through_json() {
        let mut catalog = MoqBroadcastCatalog::new();
        let schema = ident("tatolab", "core", "EncodedVideoFrame", SemVer::new(1, 0, 0));
        let proc_type = ident("tatolab", "h264", "H264Encoder", SemVer::new(1, 0, 0));
        catalog.add_track(
            "encoder/video_out",
            Some(&schema),
            Some(&proc_type),
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
        // Wire-format lock: schema + source_processor_type on the wire
        // are structured objects, never joined strings. A future
        // accidental flatten via custom Serialize would trip this test.
        let mut catalog = MoqBroadcastCatalog::new();
        let schema = ident("tatolab", "core", "VideoFrame", SemVer::new(1, 0, 0));
        let proc_type = ident("tatolab", "streamlib", "CameraProcessor", SemVer::new(1, 0, 0));
        catalog.add_track("track", Some(&schema), Some(&proc_type), "video");
        let json: serde_json::Value =
            serde_json::from_slice(&catalog.to_json_bytes()).unwrap();
        let schema_json = &json["tracks"][0]["schema"];
        assert!(
            schema_json.is_object(),
            "schema must be a structured JSON object, not a string"
        );
        assert_eq!(schema_json["org"], "tatolab");
        assert_eq!(schema_json["package"], "core");
        assert_eq!(schema_json["type"], "VideoFrame");
        assert_eq!(schema_json["version"]["major"], 1);
        let proc_json = &json["tracks"][0]["source_processor_type"];
        assert!(
            proc_json.is_object(),
            "source_processor_type must be a structured JSON object, not a string"
        );
        assert_eq!(proc_json["type"], "CameraProcessor");
    }

    #[test]
    fn catalog_entry_helper_resolves_structured() {
        let schema = ident("tatolab", "core", "AudioFrame", SemVer::new(1, 0, 0));
        let proc_type = ident(
            "tatolab",
            "streamlib",
            "AudioCaptureProcessor",
            SemVer::new(1, 0, 0),
        );
        let entry = catalog_entry_for_output_port(
            "encoder",
            Some(&schema),
            &proc_type,
            "audio_out",
        );
        assert_eq!(entry.track_name, "encoder/audio_out");
        let s = entry.schema.as_ref().unwrap();
        assert_eq!(s.type_name, "AudioFrame");
        let p = entry.source_processor_type.as_ref().unwrap();
        assert_eq!(p.type_name, "AudioCaptureProcessor");
    }
}
