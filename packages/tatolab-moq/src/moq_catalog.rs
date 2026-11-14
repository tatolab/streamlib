// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A broadcast's catalog in hang's layout, as the gateway merges it from its
//! tracks' entries and a receive node reads one track's entry back out, and the
//! one translation between a port's stream facts and a track's hang entry.

use serde_json::Value;

/// The catalog sections an entry can sit in, in hang's catalog order.
pub const CATALOG_SECTIONS: [&str; 5] = ["video", "audio", "text", "json", "binary"];

/// The map a catalog section lists its tracks under, by track name.
pub fn the_map_a_catalog_section_lists_its_tracks_under(section: &str) -> Option<&'static str> {
    use hang::catalog::Section;

    Some(match section {
        "video" | "audio" => "renditions",
        "text" => hang::catalog::Text::MAP,
        "json" => hang::catalog::Json::MAP,
        "binary" => hang::catalog::Binary::MAP,
        _ => return None,
    })
}

/// A broadcast's catalog: each `{"section", "entry"}` copied under its section,
/// keyed by its track name. An entry naming no section hang knows is left out.
pub fn the_catalog_merged_from_the_entries_of<'entry>(
    track_names_and_entries: impl IntoIterator<Item = (&'entry str, &'entry Value)>,
) -> Value {
    let mut catalog = serde_json::Map::new();
    for (track_name, section_and_entry) in track_names_and_entries {
        let Some(section) = section_and_entry.get("section").and_then(Value::as_str) else {
            continue;
        };
        let Some(map) = the_map_a_catalog_section_lists_its_tracks_under(section) else {
            continue;
        };
        let Some(entry) = section_and_entry.get("entry") else {
            continue;
        };
        let section_value = catalog
            .entry(section.to_string())
            .or_insert_with(|| serde_json::json!({ map: {} }));
        if let Some(tracks) = section_value.get_mut(map).and_then(Value::as_object_mut) {
            tracks.insert(track_name.to_string(), entry.clone());
        }
    }
    Value::Object(catalog)
}

/// The track named `track_name`'s entry in `catalog`, as `{"section", "entry"}`.
pub fn the_catalog_entry_of_the_track_named(catalog: &Value, track_name: &str) -> Option<Value> {
    CATALOG_SECTIONS.into_iter().find_map(|section| {
        let map = the_map_a_catalog_section_lists_its_tracks_under(section)?;
        let entry = catalog.get(section)?.get(map)?.get(track_name)?;
        Some(serde_json::json!({"section": section, "entry": entry}))
    })
}

/// Whether a track whose entry is `section_and_entry` carries its timestamps in
/// hang's legacy container: a varint in microseconds opening every payload.
pub fn the_track_carries_its_timestamp_in_the_hang_legacy_container(
    section_and_entry: Option<&Value>,
) -> bool {
    let Some(section_and_entry) = section_and_entry else {
        return false;
    };
    let entry = section_and_entry
        .get("entry")
        .cloned()
        .unwrap_or(Value::Null);
    let container = match section_and_entry.get("section").and_then(Value::as_str) {
        Some("video") => serde_json::from_value::<hang::catalog::VideoConfig>(entry)
            .ok()
            .map(|video| video.container),
        Some("audio") => serde_json::from_value::<hang::catalog::AudioConfig>(entry)
            .ok()
            .map(|audio| audio.container),
        _ => None,
    };
    matches!(container, Some(hang::catalog::Container::Legacy))
}

/// The hang entry `{"section", "entry"}` a send's upstream stream facts build:
/// Packets facts naming a codec become an `audio` rendition when they state a
/// sample rate or channel count and a `video` one otherwise, the configuration
/// record as hang's `description`. Other facts, or none, build no entry.
pub fn hang_catalog_entry_built_from_stream_facts(stream_facts: &Value) -> Option<Value> {
    if stream_facts.get("family").and_then(Value::as_str) != Some("packets") {
        return None;
    }
    let codec = stream_facts.get("rfc6381_codec").and_then(Value::as_str)?;
    let mut entry = serde_json::Map::new();
    entry.insert("codec".to_string(), Value::from(codec));
    let sample_rate = stream_facts.get("sample_rate").cloned();
    let channel_count = stream_facts.get("channel_count").cloned();
    let section = if sample_rate.is_some() || channel_count.is_some() {
        entry.insert(
            "sampleRate".to_string(),
            sample_rate.unwrap_or(Value::from(0)),
        );
        entry.insert(
            "numberOfChannels".to_string(),
            channel_count.unwrap_or(Value::from(0)),
        );
        "audio"
    } else {
        for (stream_facts_field, hang_field) in [
            ("coded_width", "codedWidth"),
            ("coded_height", "codedHeight"),
        ] {
            if let Some(value) = stream_facts.get(stream_facts_field) {
                entry.insert(hang_field.to_string(), value.clone());
            }
        }
        "video"
    };
    if let Some(codec_configuration_hex) = stream_facts.get("codec_configuration_hex") {
        entry.insert("description".to_string(), codec_configuration_hex.clone());
    }
    Some(serde_json::json!({"section": section, "entry": entry}))
}

/// The Packets stream facts' fields a remote track's hang entry states, for a
/// receive node writing Packets; `None` for an entry naming no codec, or one
/// outside the `video` and `audio` sections.
pub fn packets_stream_facts_read_from_hang_catalog_entry(
    section_and_entry: &Value,
) -> Option<Value> {
    let entry = section_and_entry.get("entry")?;
    let codec = entry.get("codec").and_then(Value::as_str)?;
    let field_pairs: &[(&str, &str)] =
        match section_and_entry.get("section").and_then(Value::as_str) {
            Some("video") => &[
                ("codedWidth", "coded_width"),
                ("codedHeight", "coded_height"),
            ],
            Some("audio") => &[
                ("sampleRate", "sample_rate"),
                ("numberOfChannels", "channel_count"),
            ],
            _ => return None,
        };
    let mut stream_facts = serde_json::Map::new();
    stream_facts.insert("rfc6381_codec".to_string(), Value::from(codec));
    for (hang_field, stream_facts_field) in field_pairs
        .iter()
        .copied()
        .chain([("description", "codec_configuration_hex")])
    {
        if let Some(value) = entry.get(hang_field).filter(|value| !value.is_null()) {
            stream_facts.insert(stream_facts_field.to_string(), value.clone());
        }
    }
    Some(Value::Object(stream_facts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_merge_under_their_sections_and_read_back_by_track_name() {
        let video = serde_json::json!({"section": "video", "entry": {"codec": "avc1.64001f"}});
        let telemetry = serde_json::json!({"section": "json", "entry": {}});
        let unknown = serde_json::json!({"section": "smell", "entry": {}});
        let catalog = the_catalog_merged_from_the_entries_of([
            ("camera", &video),
            ("telemetry", &telemetry),
            ("odour", &unknown),
        ]);
        assert_eq!(
            catalog["video"]["renditions"]["camera"]["codec"],
            "avc1.64001f"
        );
        assert_eq!(
            the_catalog_entry_of_the_track_named(&catalog, "camera"),
            Some(video)
        );
        assert_eq!(
            the_catalog_entry_of_the_track_named(&catalog, "telemetry"),
            Some(telemetry)
        );
        assert_eq!(
            the_catalog_entry_of_the_track_named(&catalog, "odour"),
            None
        );
    }

    #[test]
    fn hang_reads_the_merged_catalog() {
        let video = serde_json::json!({"section": "video", "entry": {
            "codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720,
            "container": {"kind": "legacy"}
        }});
        let catalog = the_catalog_merged_from_the_entries_of([("camera", &video)]);
        hang::Catalog::<()>::from_slice(&serde_json::to_vec(&catalog).unwrap())
            .expect("hang reads the catalog");
        assert!(the_track_carries_its_timestamp_in_the_hang_legacy_container(Some(&video)));
        assert!(!the_track_carries_its_timestamp_in_the_hang_legacy_container(None));
    }

    #[test]
    fn packets_facts_of_an_encoder_build_a_video_entry_hang_reads() {
        let stream_facts = serde_json::json!({
            "family": "packets", "rfc6381_codec": "avc1.64001f",
            "coded_width": 1280, "coded_height": 720,
        });
        let video = hang_catalog_entry_built_from_stream_facts(&stream_facts)
            .expect("an encoder's facts build an entry");
        assert_eq!(
            video,
            serde_json::json!({"section": "video", "entry": {
                "codec": "avc1.64001f", "codedWidth": 1280, "codedHeight": 720
            }})
        );
        let catalog = the_catalog_merged_from_the_entries_of([("camera", &video)]);
        hang::Catalog::<()>::from_slice(&serde_json::to_vec(&catalog).unwrap())
            .expect("hang reads the built entry");
        assert!(the_track_carries_its_timestamp_in_the_hang_legacy_container(Some(&video)));
    }

    #[test]
    fn packets_facts_stating_a_sample_rate_build_an_audio_entry_hang_reads() {
        let stream_facts = serde_json::json!({
            "family": "packets", "rfc6381_codec": "opus", "sample_rate": 48_000,
            "channel_count": 2, "codec_configuration_hex": "4f707573486561640102",
        });
        let audio = hang_catalog_entry_built_from_stream_facts(&stream_facts)
            .expect("an Opus encoder's facts build an entry");
        assert_eq!(audio["section"], "audio");
        assert_eq!(audio["entry"]["description"], "4f707573486561640102");
        let catalog = the_catalog_merged_from_the_entries_of([("microphone", &audio)]);
        hang::Catalog::<()>::from_slice(&serde_json::to_vec(&catalog).unwrap())
            .expect("hang reads the built entry");
        assert_eq!(
            packets_stream_facts_read_from_hang_catalog_entry(&audio),
            Some(serde_json::json!({
                "rfc6381_codec": "opus", "sample_rate": 48_000, "channel_count": 2,
                "codec_configuration_hex": "4f707573486561640102",
            }))
        );
    }

    #[test]
    fn facts_naming_no_codec_or_of_another_family_build_no_entry() {
        assert_eq!(
            hang_catalog_entry_built_from_stream_facts(
                &serde_json::json!({"family": "packets", "coded_width": 640})
            ),
            None
        );
        assert_eq!(
            hang_catalog_entry_built_from_stream_facts(
                &serde_json::json!({"family": "image", "frame_rate": {"numerator": 30, "denominator": 1}})
            ),
            None
        );
        assert_eq!(
            packets_stream_facts_read_from_hang_catalog_entry(
                &serde_json::json!({"section": "json", "entry": {"mode": "stream"}})
            ),
            None
        );
    }
}
