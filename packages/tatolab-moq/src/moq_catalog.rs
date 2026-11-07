// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A broadcast's catalog in hang's layout, as the gateway merges it from its
//! tracks' entries and a receive node reads one track's entry back out.

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
}
