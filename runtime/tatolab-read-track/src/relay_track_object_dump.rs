// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One track's objects written to a directory, one file each, beside an index.
//!
//! A track the broadcast's catalog lists in hang's legacy container is written
//! as its codec payload — the varint timestamp opening every object read off —
//! so the files concatenate into the codec's own stream (Annex-B for H.264 and
//! H.265); its timestamp is the varint. Any other track is written as sent,
//! stamped with the object's transport timestamp.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

/// The index of every object written, one JSON line each.
pub const OBJECT_INDEX_FILE_NAME: &str = "objects.jsonl";

/// The broadcast's catalog, as the publisher served it when the read began.
pub const CATALOG_FILE_NAME: &str = "catalog.json";

/// How a track's objects carry their payload and timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayTrackObjectPayloadForm {
    /// hang's legacy container: a QUIC varint timestamp in microseconds, then
    /// the codec's payload.
    HangLegacyContainer,
    /// Whatever the writer put on the port, stamped by the transport.
    AsSent,
}

impl RelayTrackObjectPayloadForm {
    /// The form of `track_name` in a broadcast whose catalog is `catalog`, or
    /// [`Self::AsSent`] when the catalog lists it in no media section.
    pub fn of_the_track_in_the_catalog(catalog: &hang::Catalog, track_name: &str) -> Self {
        let container = catalog
            .video
            .renditions
            .get(track_name)
            .map(|video| &video.container)
            .or_else(|| {
                catalog
                    .audio
                    .renditions
                    .get(track_name)
                    .map(|audio| &audio.container)
            });
        match container {
            Some(hang::catalog::Container::Legacy) => Self::HangLegacyContainer,
            _ => Self::AsSent,
        }
    }
}

/// One object as the relay handed it over.
#[derive(Debug, Clone, Copy)]
pub struct RelayTrackObjectRead<'payload> {
    /// The sequence number of the group it was read in.
    pub group_sequence: u64,
    /// Whether it opened its group.
    pub starts_group: bool,
    /// The object's transport timestamp in nanoseconds; zero when it carries none.
    pub transport_timestamp_ns: u128,
    /// The object's payload as sent.
    pub payload_as_sent: &'payload [u8],
}

/// One line of [`OBJECT_INDEX_FILE_NAME`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelayTrackObjectIndexEntry {
    /// The object's place among those written, from 0.
    pub index: u64,
    /// The sequence number of the group it was read in.
    pub group_sequence: u64,
    /// Whether it opened its group.
    pub starts_group: bool,
    /// Its timestamp in nanoseconds: the legacy container's varint, else the
    /// transport's; `null` when it carries none.
    pub timestamp_ns: Option<u128>,
    /// Its file, relative to the directory.
    pub file: String,
}

/// Writes one track's objects into a directory.
pub struct RelayTrackObjectDumpDirectory {
    directory: PathBuf,
    payload_form: RelayTrackObjectPayloadForm,
    object_index: File,
    objects_written: u64,
}

impl RelayTrackObjectDumpDirectory {
    /// A dump into `directory`, created when absent, its index begun empty.
    pub fn create(directory: &Path, payload_form: RelayTrackObjectPayloadForm) -> Result<Self> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        let object_index_path = directory.join(OBJECT_INDEX_FILE_NAME);
        let object_index = File::create(&object_index_path)
            .with_context(|| format!("cannot create {}", object_index_path.display()))?;
        Ok(Self {
            directory: directory.to_path_buf(),
            payload_form,
            object_index,
            objects_written: 0,
        })
    }

    /// Write the broadcast's catalog beside the objects.
    pub fn write_the_catalog(&self, catalog_json: &serde_json::Value) -> Result<()> {
        let catalog_path = self.directory.join(CATALOG_FILE_NAME);
        std::fs::write(&catalog_path, serde_json::to_vec_pretty(catalog_json)?)
            .with_context(|| format!("cannot write {}", catalog_path.display()))
    }

    /// Write `object` as the next file and index it.
    pub fn write_the_object(&mut self, object: RelayTrackObjectRead<'_>) -> Result<()> {
        let (payload, timestamp_ns) = match self.payload_form {
            RelayTrackObjectPayloadForm::HangLegacyContainer => {
                let media_frame = hang::container::Frame::decode(object.payload_as_sent)
                    .with_context(|| {
                        format!(
                            "object {} of group {} is not a hang legacy-container media frame",
                            self.objects_written, object.group_sequence
                        )
                    })?;
                (
                    media_frame.payload.to_vec(),
                    Some(media_frame.timestamp.as_nanos()),
                )
            }
            RelayTrackObjectPayloadForm::AsSent => (
                object.payload_as_sent.to_vec(),
                (object.transport_timestamp_ns != 0).then_some(object.transport_timestamp_ns),
            ),
        };
        let file = format!("object-{:06}.bin", self.objects_written);
        let object_path = self.directory.join(&file);
        std::fs::write(&object_path, payload)
            .with_context(|| format!("cannot write {}", object_path.display()))?;
        let index_entry = RelayTrackObjectIndexEntry {
            index: self.objects_written,
            group_sequence: object.group_sequence,
            starts_group: object.starts_group,
            timestamp_ns,
            file,
        };
        writeln!(
            self.object_index,
            "{}",
            serde_json::to_string(&index_entry)?
        )
        .context("cannot append to the object index")?;
        self.objects_written += 1;
        Ok(())
    }

    /// How many objects have been written.
    pub fn objects_written(&self) -> u64 {
        self.objects_written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A media frame in hang's legacy container, written by hang itself.
    fn hang_legacy_container_media_frame(
        timestamp_microseconds: u64,
        codec_payload: &[u8],
    ) -> Vec<u8> {
        let media_frame = hang::container::Frame {
            timestamp: hang::container::Timestamp::from_micros(timestamp_microseconds)
                .expect("a timestamp inside the varint range"),
            payload: codec_payload.to_vec().into(),
        };
        let mut encoded = Vec::new();
        media_frame
            .encode(&mut encoded)
            .expect("hang encodes the frame");
        encoded
    }

    fn index_entries_in(directory: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(directory.join(OBJECT_INDEX_FILE_NAME))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The files of a legacy-container track concatenate into the codec's own
    /// stream: each holds the access unit with the varint read off, stamped
    /// with the varint.
    #[test]
    fn a_legacy_container_track_is_written_as_its_codec_payload_stamped_by_its_varint() {
        let directory = tempfile::tempdir().unwrap();
        let mut dump = RelayTrackObjectDumpDirectory::create(
            directory.path(),
            RelayTrackObjectPayloadForm::HangLegacyContainer,
        )
        .unwrap();
        let key_frame = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
        let delta_frame = [0x00, 0x00, 0x00, 0x01, 0x41, 0x9a];

        for (group_sequence, starts_group, timestamp_microseconds, access_unit) in [
            (4, true, 1_000_000, &key_frame),
            (4, false, 1_033_333, &delta_frame),
        ] {
            dump.write_the_object(RelayTrackObjectRead {
                group_sequence,
                starts_group,
                transport_timestamp_ns: 7,
                payload_as_sent: &hang_legacy_container_media_frame(
                    timestamp_microseconds,
                    access_unit,
                ),
            })
            .unwrap();
        }

        let concatenated: Vec<u8> = ["object-000000.bin", "object-000001.bin"]
            .iter()
            .flat_map(|file| std::fs::read(directory.path().join(file)).unwrap())
            .collect();
        assert_eq!(concatenated, [key_frame, delta_frame].concat());
        assert_eq!(
            index_entries_in(directory.path()),
            [
                serde_json::json!({
                    "index": 0, "group_sequence": 4, "starts_group": true,
                    "timestamp_ns": 1_000_000_000u64, "file": "object-000000.bin",
                }),
                serde_json::json!({
                    "index": 1, "group_sequence": 4, "starts_group": false,
                    "timestamp_ns": 1_033_333_000u64, "file": "object-000001.bin",
                }),
            ]
        );
        assert_eq!(dump.objects_written(), 2);
    }

    /// A track in no media section is written byte for byte, stamped by the
    /// transport; an object the transport did not stamp carries none.
    #[test]
    fn any_other_track_is_written_as_sent_stamped_by_the_transport() {
        let directory = tempfile::tempdir().unwrap();
        let mut dump = RelayTrackObjectDumpDirectory::create(
            directory.path(),
            RelayTrackObjectPayloadForm::AsSent,
        )
        .unwrap();

        for (transport_timestamp_ns, payload_as_sent) in [(5_000, &b"\x81\xa1a\x01"[..]), (0, b"x")]
        {
            dump.write_the_object(RelayTrackObjectRead {
                group_sequence: 0,
                starts_group: true,
                transport_timestamp_ns,
                payload_as_sent,
            })
            .unwrap();
        }

        assert_eq!(
            std::fs::read(directory.path().join("object-000000.bin")).unwrap(),
            b"\x81\xa1a\x01"
        );
        let index_entries = index_entries_in(directory.path());
        assert_eq!(index_entries[0]["timestamp_ns"], 5_000);
        assert_eq!(index_entries[1]["timestamp_ns"], serde_json::Value::Null);
    }

    #[test]
    fn an_object_that_is_not_a_legacy_container_frame_is_refused_naming_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut dump = RelayTrackObjectDumpDirectory::create(
            directory.path(),
            RelayTrackObjectPayloadForm::HangLegacyContainer,
        )
        .unwrap();

        let refusal = dump
            .write_the_object(RelayTrackObjectRead {
                group_sequence: 9,
                starts_group: true,
                transport_timestamp_ns: 0,
                payload_as_sent: &[],
            })
            .unwrap_err();

        assert!(refusal.to_string().contains("group 9"), "{refusal:#}");
    }

    /// The catalog's video and audio renditions name the legacy-container
    /// tracks; a track it lists nowhere is written as sent.
    #[test]
    fn the_catalog_names_which_tracks_carry_the_legacy_container() {
        let catalog: hang::Catalog = serde_json::from_value(serde_json::json!({
            "video": { "renditions": {
                "encoder/encoded_video": { "codec": "avc1.64001f", "container": { "kind": "legacy" } },
            } },
        }))
        .unwrap();

        assert_eq!(
            RelayTrackObjectPayloadForm::of_the_track_in_the_catalog(
                &catalog,
                "encoder/encoded_video"
            ),
            RelayTrackObjectPayloadForm::HangLegacyContainer
        );
        assert_eq!(
            RelayTrackObjectPayloadForm::of_the_track_in_the_catalog(&catalog, "meter/brightness"),
            RelayTrackObjectPayloadForm::AsSent
        );
    }
}
