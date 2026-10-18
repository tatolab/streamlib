// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An output port's catalog entry, in the layout the MoQ players read.
//!
//! The field names are hang's catalog spelling (`codedWidth`, `sampleRate`,
//! `numberOfChannels`, `mode`, `mime`) — a wire contract with every player that
//! reads a broadcast's catalog, never the runtime's own words.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The catalog section an entry belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum CatalogSection {
    /// Video renditions a player picks one of.
    Video,
    /// Audio renditions a player picks one of.
    Audio,
    /// Timed-text tracks.
    Text,
    /// JSON data tracks.
    Json,
    /// Opaque binary data tracks.
    Binary,
}

impl CatalogSection {
    /// Every section, in hang's catalog order.
    pub const ALL: [CatalogSection; 5] = [
        CatalogSection::Video,
        CatalogSection::Audio,
        CatalogSection::Text,
        CatalogSection::Json,
        CatalogSection::Binary,
    ];

    /// The section's name as the catalog spells it.
    pub fn as_catalog_key(self) -> &'static str {
        match self {
            CatalogSection::Video => "video",
            CatalogSection::Audio => "audio",
            CatalogSection::Text => "text",
            CatalogSection::Json => "json",
            CatalogSection::Binary => "binary",
        }
    }

    /// The name of the section's entry type, as an author writes it.
    pub fn entry_type_name(self) -> &'static str {
        match self {
            CatalogSection::Video => "VideoEntry",
            CatalogSection::Audio => "AudioEntry",
            CatalogSection::Text => "TextEntry",
            CatalogSection::Json => "JsonEntry",
            CatalogSection::Binary => "BinaryEntry",
        }
    }

    /// The section a catalog key names, or `None`.
    pub fn from_catalog_key(catalog_key: &str) -> Option<CatalogSection> {
        CatalogSection::ALL
            .into_iter()
            .find(|section| section.as_catalog_key() == catalog_key)
    }
}

impl std::fmt::Display for CatalogSection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_catalog_key())
    }
}

/// An output port's catalog entry: its section and that section's own fields,
/// in hang's layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    /// The section the entry belongs in.
    pub section: CatalogSection,
    /// The section's fields, spelled as hang spells them.
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub entry: Value,
}

/// How a `json` or `binary` track's groups carry its payloads. hang gives it no
/// default, so every data-track entry states one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogDataTrackMode {
    /// Each group carries one complete payload superseding the last.
    Snapshot,
    /// One group, never rolled, every payload appended in order.
    Stream,
}

impl CatalogDataTrackMode {
    fn as_catalog_value(self) -> &'static str {
        match self {
            CatalogDataTrackMode::Snapshot => "snapshot",
            CatalogDataTrackMode::Stream => "stream",
        }
    }
}

/// A `video` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoEntry {
    /// The WebCodecs codec string (`avc1.64001f`, `hvc1.1.6.L93.B0`).
    pub codec: String,
    /// Width of the coded picture, in pixels.
    pub coded_width: Option<u32>,
    /// Height of the coded picture, in pixels.
    pub coded_height: Option<u32>,
    /// A decoder's out-of-band setup, hex-encoded.
    pub description: Option<String>,
}

/// An `audio` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioEntry {
    /// The WebCodecs codec string (`opus`, `mp4a.40.2`).
    pub codec: String,
    /// Samples per second, per channel.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub number_of_channels: u32,
    /// A decoder's out-of-band setup, hex-encoded.
    pub description: Option<String>,
}

/// A `text` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextEntry {
    /// The cue format (`vtt`, `ttml`).
    pub format: String,
}

/// A `json` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonEntry {
    /// How the track's groups carry its payloads.
    pub mode: CatalogDataTrackMode,
}

/// A `binary` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryEntry {
    /// How the track's groups carry its payloads.
    pub mode: CatalogDataTrackMode,
    /// The payload's media type (`application/msgpack`).
    pub mime: Option<String>,
}

/// The fields of one entry, those left unstated absent rather than `null`.
fn catalog_entry_fields<const FIELD_COUNT: usize>(
    named_fields: [(&str, Option<Value>); FIELD_COUNT],
) -> Value {
    Value::Object(
        named_fields
            .into_iter()
            .filter_map(|(catalog_field_name, value)| {
                value.map(|value| (catalog_field_name.to_string(), value))
            })
            .collect::<Map<String, Value>>(),
    )
}

impl From<VideoEntry> for CatalogEntry {
    fn from(video: VideoEntry) -> Self {
        CatalogEntry {
            section: CatalogSection::Video,
            entry: catalog_entry_fields([
                ("codec", Some(Value::from(video.codec))),
                ("codedWidth", video.coded_width.map(Value::from)),
                ("codedHeight", video.coded_height.map(Value::from)),
                ("description", video.description.map(Value::from)),
            ]),
        }
    }
}

impl From<AudioEntry> for CatalogEntry {
    fn from(audio: AudioEntry) -> Self {
        CatalogEntry {
            section: CatalogSection::Audio,
            entry: catalog_entry_fields([
                ("codec", Some(Value::from(audio.codec))),
                ("sampleRate", Some(Value::from(audio.sample_rate))),
                (
                    "numberOfChannels",
                    Some(Value::from(audio.number_of_channels)),
                ),
                ("description", audio.description.map(Value::from)),
            ]),
        }
    }
}

/// Why a catalog entry does not read as an [`AudioEntry`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogEntryIsNotAnAudioEntry {
    /// The entry belongs in another section.
    #[error("the catalog entry is a `{section}` entry, not an `audio` one")]
    OfAnotherSection { section: CatalogSection },
    /// The fields are not the ones an `audio` entry carries.
    #[error("the `audio` entry's fields do not read: {reason}")]
    FieldsDoNotRead { reason: String },
}

impl TryFrom<&CatalogEntry> for AudioEntry {
    type Error = CatalogEntryIsNotAnAudioEntry;

    fn try_from(catalog_entry: &CatalogEntry) -> Result<Self, Self::Error> {
        if catalog_entry.section != CatalogSection::Audio {
            return Err(CatalogEntryIsNotAnAudioEntry::OfAnotherSection {
                section: catalog_entry.section,
            });
        }
        AudioEntry::deserialize(&catalog_entry.entry).map_err(|failure| {
            CatalogEntryIsNotAnAudioEntry::FieldsDoNotRead {
                reason: failure.to_string(),
            }
        })
    }
}

impl From<TextEntry> for CatalogEntry {
    fn from(text: TextEntry) -> Self {
        CatalogEntry {
            section: CatalogSection::Text,
            entry: catalog_entry_fields([("format", Some(Value::from(text.format)))]),
        }
    }
}

impl From<JsonEntry> for CatalogEntry {
    fn from(json: JsonEntry) -> Self {
        CatalogEntry {
            section: CatalogSection::Json,
            entry: catalog_entry_fields([(
                "mode",
                Some(Value::from(json.mode.as_catalog_value())),
            )]),
        }
    }
}

impl From<BinaryEntry> for CatalogEntry {
    fn from(binary: BinaryEntry) -> Self {
        CatalogEntry {
            section: CatalogSection::Binary,
            entry: catalog_entry_fields([
                ("mode", Some(Value::from(binary.mode.as_catalog_value()))),
                ("mime", binary.mime.map(Value::from)),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_entry_spells_hangs_field_names_and_omits_what_it_leaves_unstated() {
        assert_eq!(
            CatalogEntry::from(VideoEntry {
                codec: "avc1.64001f".into(),
                coded_width: Some(1280),
                coded_height: Some(720),
                description: None,
            }),
            CatalogEntry {
                section: CatalogSection::Video,
                entry: json!({"codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720}),
            }
        );
        assert_eq!(
            CatalogEntry::from(AudioEntry {
                codec: "opus".into(),
                sample_rate: 48_000,
                number_of_channels: 2,
                description: Some("4f707573".into()),
            })
            .entry,
            json!({"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2, "description": "4f707573"})
        );
        assert_eq!(
            CatalogEntry::from(TextEntry {
                format: "vtt".into()
            })
            .entry,
            json!({"format": "vtt"})
        );
        assert_eq!(
            CatalogEntry::from(JsonEntry {
                mode: CatalogDataTrackMode::Stream
            }),
            CatalogEntry {
                section: CatalogSection::Json,
                entry: json!({"mode": "stream"}),
            }
        );
        assert_eq!(
            CatalogEntry::from(BinaryEntry {
                mode: CatalogDataTrackMode::Snapshot,
                mime: Some("application/msgpack".into()),
            })
            .entry,
            json!({"mode": "snapshot", "mime": "application/msgpack"})
        );
    }

    #[test]
    fn an_audio_entry_reads_back_from_its_catalog_entry() {
        let audio = AudioEntry {
            codec: "opus".into(),
            sample_rate: 48_000,
            number_of_channels: 6,
            description: Some("4f707573".into()),
        };
        assert_eq!(
            AudioEntry::try_from(&CatalogEntry::from(audio.clone())),
            Ok(audio)
        );
        assert_eq!(
            AudioEntry::try_from(&CatalogEntry::from(BinaryEntry {
                mode: CatalogDataTrackMode::Snapshot,
                mime: None,
            })),
            Err(CatalogEntryIsNotAnAudioEntry::OfAnotherSection {
                section: CatalogSection::Binary
            })
        );
        assert!(matches!(
            AudioEntry::try_from(&CatalogEntry {
                section: CatalogSection::Audio,
                entry: json!({"codec": "opus"}),
            }),
            Err(CatalogEntryIsNotAnAudioEntry::FieldsDoNotRead { .. })
        ));
    }

    #[test]
    fn an_entry_round_trips_through_its_wire_form_with_its_section_lowercased() {
        let entry = CatalogEntry {
            section: CatalogSection::Binary,
            entry: json!({"mode": "snapshot"}),
        };
        let wire = serde_json::to_value(&entry).expect("an entry serializes");
        assert_eq!(
            wire,
            json!({"section": "binary", "entry": {"mode": "snapshot"}})
        );
        assert_eq!(
            serde_json::from_value::<CatalogEntry>(wire).expect("the wire form reads back"),
            entry
        );
    }

    #[test]
    fn a_section_reads_back_from_its_catalog_key_and_no_other() {
        for section in CatalogSection::ALL {
            assert_eq!(
                CatalogSection::from_catalog_key(section.as_catalog_key()),
                Some(section)
            );
        }
        assert_eq!(CatalogSection::from_catalog_key("Video"), None);
    }
}
