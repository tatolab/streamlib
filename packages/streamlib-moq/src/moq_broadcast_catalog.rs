// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The broadcast's discovery document and the names its tracks answer to.
//!
//! Every subscriber — `moq-js`, `moq-sub`, this wheel — reads the catalog
//! before it reads a byte of media, so this file is the interop surface: a
//! field spelled wrong here drops the whole stream rather than degrading it.
//! The shape is draft-ietf-moq-catalogformat-01, matched field for field
//! against what `moq-pub` emits.
//!
//! The types are written here rather than taken from the `moq-catalog` crate
//! because that crate's `TrackPackaging` is a closed `cmaf` / `loc` enum with
//! no room for a third value, and this publisher also declares the
//! `streamlib-bag` packaging for its other container. `moq-catalog` stays a
//! dev-dependency, where a test proves a CMAF catalog written from these types
//! parses back as the reference's.

use serde::Serialize;

use crate::error::{MoqExtensionError, Result};

/// The track every subscriber reads first to learn what the broadcast carries.
pub(crate) const CATALOG_TRACK_NAME: &str = ".catalog";

/// The track carrying the CMAF initialisation segment — one object, the `ftyp`
/// bytes immediately followed by the `moov` bytes. The name is the reference
/// publisher's and `moq-sub` matches it by value.
pub(crate) const INIT_TRACK_NAME: &str = "0.mp4";

/// The catalog's name for the fragmented-MP4 container.
pub(crate) const CMAF_PACKAGING: &str = "cmaf";

/// The catalog's name for this wheel's own container. Deliberately outside the
/// two the catalog format standardises, so a player that does not know it skips
/// the track instead of decoding noise. The spelling is pinned to
/// [`crate::streamlib_bag_object::STREAMLIB_BAG_PACKAGING`] by a test.
pub(crate) const STREAMLIB_BAG_PACKAGING: &str = "streamlib-bag";

/// The name of the media track carrying the `trak` with this track id.
pub(crate) fn media_track_name(track_id: u32) -> String {
    format!("{track_id}.m4s")
}

/// The catalog format revision. `moq-sub` hard-fails any other value, so this
/// is not a knob.
const CATALOG_FORMAT_VERSION: u16 = 1;

/// The streaming format this catalog describes, and its revision: 1 is
/// "MoQ streaming format 1" — the LOC/CMAF format the reference publisher and
/// every shipped player agree on.
const MOQ_STREAMING_FORMAT: u16 = 1;
const MOQ_STREAMING_FORMAT_VERSION: &str = "0.2";

/// A subscriber may apply a later catalog as a patch rather than a whole
/// document. This publisher writes the catalog once, but the reference declares
/// support and a player reads the flag before it reads the tracks.
const SUPPORTS_DELTA_UPDATES: bool = true;

/// Every track of one broadcast is rendered together as one presentation, which
/// is what render group 1 means. A second group would be an alternate rendition.
const RENDER_GROUP_RENDERED_TOGETHER: u16 = 1;

/// The parameters a player selects a track on.
///
/// Field order here is the reference struct's declaration order, because that
/// is the order the reference's JSON comes out in and a catalog that diffs
/// clean against a known-good one is worth more than an alphabetised struct.
/// `framerate` is absent on purpose: no shipped reference stream carries it,
/// and a player that sees it is reading a field nothing has exercised.
#[derive(Debug, Clone)]
pub(crate) struct MoqCatalogTrackSelectionParameters {
    /// The RFC 6381 codec string — `avc1.64001f`, `hvc1.1.6.L93.B0`, `opus`.
    pub(crate) codec_string: String,
    pub(crate) bitrate_bits_per_second: Option<u32>,
    /// The coded extent, before the conformance crop.
    pub(crate) coded_width: Option<u32>,
    pub(crate) coded_height: Option<u32>,
    pub(crate) sample_rate_hz: Option<u32>,
    /// Serialised as `channelConfig`, a **string** of the channel count — the
    /// catalog format's field is a string even when the value is a number.
    pub(crate) channel_count: Option<u32>,
}

impl MoqCatalogTrackSelectionParameters {
    /// The parameters a video track selects on.
    pub(crate) fn of_video_track(
        codec_string: impl Into<String>,
        coded_width: u32,
        coded_height: u32,
    ) -> Self {
        Self {
            codec_string: codec_string.into(),
            bitrate_bits_per_second: None,
            coded_width: Some(coded_width),
            coded_height: Some(coded_height),
            sample_rate_hz: None,
            channel_count: None,
        }
    }

    /// The parameters an audio track selects on.
    pub(crate) fn of_audio_track(
        codec_string: impl Into<String>,
        sample_rate_hz: u32,
        channel_count: u32,
    ) -> Self {
        Self {
            codec_string: codec_string.into(),
            bitrate_bits_per_second: None,
            coded_width: None,
            coded_height: None,
            sample_rate_hz: Some(sample_rate_hz),
            channel_count: Some(channel_count),
        }
    }

    /// Declare the track's bitrate, which a player uses to choose between
    /// renditions.
    pub(crate) fn with_bitrate_bits_per_second(mut self, bitrate_bits_per_second: u32) -> Self {
        self.bitrate_bits_per_second = Some(bitrate_bits_per_second);
        self
    }
}

/// One media track as the catalog describes it.
#[derive(Debug, Clone)]
pub(crate) struct MoqCatalogTrackDescription {
    /// The track name a subscriber subscribes to for this track's media.
    pub(crate) media_track_name: String,
    /// The track carrying this track's initialisation object. CMAF requires it
    /// on every track — `moq-sub` unwraps the first track's without checking —
    /// and it is `None` only for a container that publishes no initialisation
    /// object at all, where naming one would point a subscriber at a track
    /// nothing writes.
    pub(crate) initialisation_track_name: Option<String>,
    pub(crate) selection_parameters: MoqCatalogTrackSelectionParameters,
}

impl MoqCatalogTrackDescription {
    /// A CMAF media track, named after the `trak` it carries and pointed at the
    /// one initialisation track the broadcast publishes.
    pub(crate) fn of_cmaf_track_id(
        track_id: u32,
        selection_parameters: MoqCatalogTrackSelectionParameters,
    ) -> Self {
        Self {
            media_track_name: media_track_name(track_id),
            initialisation_track_name: Some(INIT_TRACK_NAME.to_owned()),
            selection_parameters,
        }
    }

    /// A track whose container carries everything a decoder needs in each
    /// object, so the broadcast publishes no initialisation track.
    pub(crate) fn of_self_describing_track(
        media_track_name: impl Into<String>,
        selection_parameters: MoqCatalogTrackSelectionParameters,
    ) -> Self {
        Self {
            media_track_name: media_track_name.into(),
            initialisation_track_name: None,
            selection_parameters,
        }
    }
}

/// The whole discovery document for one broadcast.
///
/// Track order is the caller's and is preserved to the byte: `moq-sub` zips the
/// catalog's tracks against the `trak`s of the initialisation segment
/// positionally, by index, so a reordered catalog hands the audio track's
/// description to the video decoder.
#[derive(Debug, Clone)]
pub(crate) struct MoqBroadcastCatalog {
    broadcast_namespace: String,
    container_packaging: String,
    media_tracks: Vec<MoqCatalogTrackDescription>,
}

impl MoqBroadcastCatalog {
    /// The catalog of a broadcast whose media tracks are CMAF chunks.
    pub(crate) fn of_cmaf_tracks(
        broadcast_namespace: impl Into<String>,
        media_tracks: Vec<MoqCatalogTrackDescription>,
    ) -> Self {
        Self {
            broadcast_namespace: broadcast_namespace.into(),
            container_packaging: CMAF_PACKAGING.to_owned(),
            media_tracks,
        }
    }

    /// The catalog of a broadcast whose media tracks are `streamlib-bag`
    /// objects.
    pub(crate) fn of_streamlib_bag_tracks(
        broadcast_namespace: impl Into<String>,
        media_tracks: Vec<MoqCatalogTrackDescription>,
    ) -> Self {
        Self {
            broadcast_namespace: broadcast_namespace.into(),
            container_packaging: STREAMLIB_BAG_PACKAGING.to_owned(),
            media_tracks,
        }
    }

    /// The catalog object's payload: pretty-printed JSON, two-space indent,
    /// exactly as the reference publisher writes it.
    pub(crate) fn catalog_json_bytes(&self) -> Result<bytes::Bytes> {
        let document = serde_json::to_string_pretty(&self.on_the_wire()).map_err(|failure| {
            MoqExtensionError::MalformedObject {
                container: "catalog",
                what: format!("the broadcast catalog could not be written as JSON: {failure}"),
            }
        })?;
        Ok(bytes::Bytes::from(document))
    }

    /// `namespace`, `packaging` and `renderGroup` are hoisted into
    /// `commonTrackFields` and omitted per track: every track of one broadcast
    /// agrees on all three by construction, and the reference hoists whatever
    /// every track agrees on.
    fn on_the_wire(&self) -> BroadcastCatalogRootOnTheWire<'_> {
        BroadcastCatalogRootOnTheWire {
            version: CATALOG_FORMAT_VERSION,
            streaming_format: MOQ_STREAMING_FORMAT,
            streaming_format_version: MOQ_STREAMING_FORMAT_VERSION,
            supports_delta_updates: SUPPORTS_DELTA_UPDATES,
            common_track_fields: CommonTrackFieldsOnTheWire {
                namespace: &self.broadcast_namespace,
                packaging: &self.container_packaging,
                render_group: RENDER_GROUP_RENDERED_TOGETHER,
            },
            tracks: self
                .media_tracks
                .iter()
                .map(|track| TrackOnTheWire {
                    name: &track.media_track_name,
                    init_track: track.initialisation_track_name.as_deref(),
                    selection_params: SelectionParametersOnTheWire {
                        codec: &track.selection_parameters.codec_string,
                        bitrate: track.selection_parameters.bitrate_bits_per_second,
                        width: track.selection_parameters.coded_width,
                        height: track.selection_parameters.coded_height,
                        samplerate: track.selection_parameters.sample_rate_hz,
                        channel_config: track
                            .selection_parameters
                            .channel_count
                            .map(|channel_count| channel_count.to_string()),
                    },
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct BroadcastCatalogRootOnTheWire<'catalog> {
    version: u16,
    #[serde(rename = "streamingFormat")]
    streaming_format: u16,
    #[serde(rename = "streamingFormatVersion")]
    streaming_format_version: &'static str,
    #[serde(rename = "supportsDeltaUpdates")]
    supports_delta_updates: bool,
    #[serde(rename = "commonTrackFields")]
    common_track_fields: CommonTrackFieldsOnTheWire<'catalog>,
    tracks: Vec<TrackOnTheWire<'catalog>>,
}

#[derive(Debug, Serialize)]
struct CommonTrackFieldsOnTheWire<'catalog> {
    namespace: &'catalog str,
    packaging: &'catalog str,
    #[serde(rename = "renderGroup")]
    render_group: u16,
}

#[derive(Debug, Serialize)]
struct TrackOnTheWire<'catalog> {
    name: &'catalog str,
    #[serde(rename = "initTrack", skip_serializing_if = "Option::is_none")]
    init_track: Option<&'catalog str>,
    #[serde(rename = "selectionParams")]
    selection_params: SelectionParametersOnTheWire<'catalog>,
}

#[derive(Debug, Serialize)]
struct SelectionParametersOnTheWire<'catalog> {
    codec: &'catalog str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bitrate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    samplerate: Option<u32>,
    #[serde(rename = "channelConfig", skip_serializing_if = "Option::is_none")]
    channel_config: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `moq-pub` writes for a two-track broadcast, copied from a captured
    /// reference catalog.
    const REFERENCE_CATALOG_JSON: &str = r#"{
  "version": 1,
  "streamingFormat": 1,
  "streamingFormatVersion": "0.2",
  "supportsDeltaUpdates": true,
  "commonTrackFields": {
    "namespace": "bbb",
    "packaging": "cmaf",
    "renderGroup": 1
  },
  "tracks": [
    {
      "name": "1.m4s",
      "initTrack": "0.mp4",
      "selectionParams": {
        "codec": "avc1.64001f",
        "width": 1920,
        "height": 1080
      }
    },
    {
      "name": "2.m4s",
      "initTrack": "0.mp4",
      "selectionParams": {
        "codec": "mp4a.40.2",
        "bitrate": 128000,
        "samplerate": 48000,
        "channelConfig": "2"
      }
    }
  ]
}"#;

    fn reference_shaped_catalog() -> MoqBroadcastCatalog {
        MoqBroadcastCatalog::of_cmaf_tracks(
            "bbb",
            vec![
                MoqCatalogTrackDescription::of_cmaf_track_id(
                    1,
                    MoqCatalogTrackSelectionParameters::of_video_track("avc1.64001f", 1920, 1080),
                ),
                MoqCatalogTrackDescription::of_cmaf_track_id(
                    2,
                    MoqCatalogTrackSelectionParameters::of_audio_track("mp4a.40.2", 48_000, 2)
                        .with_bitrate_bits_per_second(128_000),
                ),
            ],
        )
    }

    fn catalog_json_string(catalog: &MoqBroadcastCatalog) -> String {
        let payload = catalog
            .catalog_json_bytes()
            .expect("a catalog of owned strings and integers always serialises");
        String::from_utf8(payload.to_vec()).expect("serde_json writes UTF-8")
    }

    #[test]
    fn a_two_track_cmaf_catalog_is_byte_for_byte_what_the_reference_publisher_emits() {
        assert_eq!(
            catalog_json_string(&reference_shaped_catalog()),
            REFERENCE_CATALOG_JSON
        );
    }

    #[test]
    fn a_cmaf_catalog_this_module_writes_parses_back_as_the_reference_catalog_type() {
        let document = catalog_json_string(&reference_shaped_catalog());

        let parsed: moq_catalog::Root =
            serde_json::from_str(&document).expect("the reference type reads this catalog");

        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.streaming_format, 1);
        assert_eq!(parsed.streaming_format_version, "0.2");
        assert!(parsed.streaming_delta_updates);
        assert_eq!(parsed.common_track_fields.namespace.as_deref(), Some("bbb"));
        assert_eq!(
            parsed.common_track_fields.packaging,
            Some(moq_catalog::TrackPackaging::Cmaf)
        );
        assert_eq!(parsed.common_track_fields.render_group, Some(1));

        assert_eq!(parsed.tracks.len(), 2);
        assert_eq!(parsed.tracks[0].name, "1.m4s");
        assert_eq!(parsed.tracks[0].init_track.as_deref(), Some("0.mp4"));
        assert_eq!(
            parsed.tracks[0].selection_params.codec.as_deref(),
            Some("avc1.64001f")
        );
        assert_eq!(parsed.tracks[0].selection_params.width, Some(1920));
        assert_eq!(parsed.tracks[0].selection_params.height, Some(1080));
        assert_eq!(parsed.tracks[0].selection_params.framerate, None);

        assert_eq!(parsed.tracks[1].name, "2.m4s");
        assert_eq!(parsed.tracks[1].init_track.as_deref(), Some("0.mp4"));
        assert_eq!(parsed.tracks[1].selection_params.samplerate, Some(48_000));
        assert_eq!(parsed.tracks[1].selection_params.bitrate, Some(128_000));
        assert_eq!(
            parsed.tracks[1].selection_params.channel_config.as_deref(),
            Some("2")
        );
    }

    #[test]
    fn an_opus_track_names_its_sample_rate_and_its_channel_count_as_a_string() {
        let catalog = MoqBroadcastCatalog::of_cmaf_tracks(
            "streamlib",
            vec![MoqCatalogTrackDescription::of_cmaf_track_id(
                1,
                MoqCatalogTrackSelectionParameters::of_audio_track("opus", 48_000, 2),
            )],
        );

        let parsed: moq_catalog::Root = serde_json::from_str(&catalog_json_string(&catalog))
            .expect("the reference type reads this catalog");

        let selection_params = &parsed.tracks[0].selection_params;
        assert_eq!(selection_params.codec.as_deref(), Some("opus"));
        assert_eq!(selection_params.samplerate, Some(48_000));
        assert_eq!(selection_params.channel_config.as_deref(), Some("2"));
        assert_eq!(selection_params.width, None);
        assert_eq!(selection_params.height, None);
    }

    #[test]
    fn a_streamlib_bag_catalog_carries_a_packaging_the_reference_type_cannot_hold() {
        let catalog = MoqBroadcastCatalog::of_streamlib_bag_tracks(
            "streamlib",
            vec![MoqCatalogTrackDescription::of_self_describing_track(
                "1.bag",
                MoqCatalogTrackSelectionParameters::of_video_track("h264", 1920, 1080),
            )],
        );

        let document = catalog_json_string(&catalog);

        assert!(
            document.contains(r#""packaging": "streamlib-bag""#),
            "the catalog names its own container: {document}"
        );
        assert!(
            !document.contains("initTrack"),
            "a self-describing container publishes no init track to point at: {document}"
        );
        assert!(
            serde_json::from_str::<moq_catalog::Root>(&document).is_err(),
            "the reference packaging enum is closed, which is why this crate writes its own types"
        );
    }

    #[test]
    fn the_catalog_names_the_same_container_the_object_writer_encodes() {
        assert_eq!(
            STREAMLIB_BAG_PACKAGING,
            crate::streamlib_bag_object::STREAMLIB_BAG_PACKAGING
        );
    }

    #[test]
    fn every_cmaf_track_names_the_one_init_track_because_a_subscriber_unwraps_it() {
        let document = catalog_json_string(&reference_shaped_catalog());

        let parsed: moq_catalog::Root =
            serde_json::from_str(&document).expect("the reference type reads this catalog");

        assert!(
            parsed
                .tracks
                .iter()
                .all(|track| track.init_track.as_deref() == Some(INIT_TRACK_NAME))
        );
    }

    #[test]
    fn tracks_keep_the_order_they_were_given_so_a_subscriber_can_zip_them_against_the_moov() {
        let catalog = MoqBroadcastCatalog::of_cmaf_tracks(
            "streamlib",
            vec![
                MoqCatalogTrackDescription::of_cmaf_track_id(
                    7,
                    MoqCatalogTrackSelectionParameters::of_audio_track("opus", 48_000, 1),
                ),
                MoqCatalogTrackDescription::of_cmaf_track_id(
                    3,
                    MoqCatalogTrackSelectionParameters::of_video_track("avc1.64001f", 640, 480),
                ),
            ],
        );

        let parsed: moq_catalog::Root = serde_json::from_str(&catalog_json_string(&catalog))
            .expect("the reference type reads this catalog");

        assert_eq!(parsed.tracks[0].name, "7.m4s");
        assert_eq!(
            parsed.tracks[0].selection_params.codec.as_deref(),
            Some("opus")
        );
        assert_eq!(parsed.tracks[1].name, "3.m4s");
        assert_eq!(parsed.tracks[1].selection_params.width, Some(640));
    }

    #[test]
    fn a_media_track_is_named_after_the_track_id_of_the_trak_it_carries() {
        assert_eq!(media_track_name(1), "1.m4s");
        assert_eq!(media_track_name(42), "42.m4s");
    }

    #[test]
    fn the_catalog_declares_format_version_one_because_a_subscriber_hard_fails_any_other() {
        let parsed: moq_catalog::Root =
            serde_json::from_str(&catalog_json_string(&reference_shaped_catalog()))
                .expect("the reference type reads this catalog");

        assert_eq!(parsed.version, CATALOG_FORMAT_VERSION);
        assert_eq!(CATALOG_FORMAT_VERSION, 1);
    }

    #[test]
    fn the_catalog_track_is_the_dotted_name_every_subscriber_looks_for_first() {
        assert_eq!(CATALOG_TRACK_NAME, ".catalog");
        assert_eq!(INIT_TRACK_NAME, "0.mp4");
        assert_eq!(CMAF_PACKAGING, "cmaf");
    }
}
