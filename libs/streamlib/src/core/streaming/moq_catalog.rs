// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Catalog Generation
//
// Generates MoQ catalog tracks from StreamLib's processor registry.
// Maps JTD schema names to MoQ track names so remote subscribers
// can discover available data streams.

use serde::{Deserialize, Serialize};

/// A catalog entry describing a single MoQ track and its StreamLib schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoqCatalogTrackEntry {
    /// MoQ track name (derived from schema_name unless overridden).
    pub track_name: String,
    /// StreamLib JTD schema name (e.g., "com.tatolab.encodedvideoframe@1.0.0").
    pub schema_name: String,
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

    /// Add a track entry to the catalog.
    pub fn add_track(
        &mut self,
        track_name: &str,
        schema_name: &str,
        source_processor_type: &str,
        source_port_name: &str,
    ) {
        self.tracks.push(MoqCatalogTrackEntry {
            track_name: track_name.to_string(),
            schema_name: schema_name.to_string(),
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

/// Map a StreamLib JTD schema name to a MoQ track name.
///
/// By default, the schema name IS the track name. This provides a simple,
/// deterministic mapping that remote subscribers can use to discover tracks.
///
/// Example: "com.tatolab.encodedvideoframe@1.0.0" → "com.tatolab.encodedvideoframe@1.0.0"
pub fn schema_name_to_moq_track_name(schema_name: &str) -> String {
    schema_name.to_string()
}

/// Generate a catalog entry for a single output port.
pub fn catalog_entry_for_output_port(
    schema_name: &str,
    processor_type: &str,
    port_name: &str,
) -> MoqCatalogTrackEntry {
    MoqCatalogTrackEntry {
        track_name: schema_name_to_moq_track_name(schema_name),
        schema_name: schema_name.to_string(),
        source_processor_type: processor_type.to_string(),
        source_port_name: port_name.to_string(),
    }
}
