// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the spike puts on the wire behind the measurement preamble, and how
//! wide a surface reference is.
//!
//! Owner decision 7 on #1702: Tier A carries reference-sized frames matching a
//! surface-id frame's weight. The early Tier A run that reported a ~27ms floor
//! at 1080p was pushing whole uncompressed pictures through main memory — a
//! choice of this harness, not a property of the engine. A real
//! `@tatolab/core/VideoFrame` references its GPU surface by id and weighs a few
//! hundred bytes (`packages/core/schemas/video_frame.yaml`), so the reference
//! mode is what the protocol cells measure and the full-pixel mode survives
//! only to keep that retracted result reproducible.

use serde::{Deserialize, Serialize};

/// What rides the wire behind the measurement preamble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SyntheticFrameWirePayloadMode {
    /// A surface reference weighing what a real `VideoFrame` weighs. The
    /// protocol default; pixels never cross the link.
    #[default]
    SurfaceReference,
    /// The whole uncompressed picture. Reproduces the payload-size sweep that
    /// retracted the 27ms floor; never the basis of a gated number.
    FullPixelPayload,
}

impl SyntheticFrameWirePayloadMode {
    /// Stable token used in cell directory names and in `cell-spec.json`.
    pub fn as_artifact_token(self) -> &'static str {
        match self {
            SyntheticFrameWirePayloadMode::SurfaceReference => "surface-reference",
            SyntheticFrameWirePayloadMode::FullPixelPayload => "full-pixel-payload",
        }
    }

    /// Parse the `--wire-payload-mode` flag value.
    pub fn parse_from_flag_value(value: &str) -> std::result::Result<Self, String> {
        match value {
            "surface-reference" => Ok(SyntheticFrameWirePayloadMode::SurfaceReference),
            "full-pixel-payload" => Ok(SyntheticFrameWirePayloadMode::FullPixelPayload),
            other => Err(format!(
                "unknown wire payload mode `{other}` — expected `surface-reference` or \
                 `full-pixel-payload`"
            )),
        }
    }
}

/// The fields a `@tatolab/core/VideoFrame` carries on the wire, populated with
/// values a Linux camera would actually produce.
///
/// This exists so the reference body's width is *derived* from the schema
/// rather than asserted as a magic constant: change the schema and this struct
/// diverges visibly, where a hardcoded byte count would silently stay wrong.
/// The optional HDR members (`color_info`, `mastering_display`, `content_light`)
/// are absent, matching an SDR camera stream.
#[derive(Debug, Serialize)]
struct RepresentativeVideoFrameSurfaceReference {
    surface_id: String,
    width: u32,
    height: u32,
    timestamp_ns: String,
    fps: u32,
    texture_layout: i32,
}

impl RepresentativeVideoFrameSurfaceReference {
    fn for_geometry(width_pixels: u32, height_pixels: u32, frames_per_second: u32) -> Self {
        Self {
            // Shaped like the ids the surface-share service mints: a device
            // stem plus a pool slot. Length matters here, content does not.
            surface_id: "camera-vivid-video0-texture-pool-slot-03".to_string(),
            width: width_pixels,
            height: height_pixels,
            // The schema carries this as a string to survive JSON's 53-bit
            // integer limit, so the encoded body pays for 19 digits of text.
            timestamp_ns: "1754081999123456789".to_string(),
            fps: frames_per_second,
            // VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL.
            texture_layout: 5,
        }
    }
}

/// Fixed wire width of a surface reference, in bytes.
///
/// Held constant across geometries on purpose. A bare encoding varies by a
/// byte or two with the decimal digit count of `width`/`height` (640 is three
/// digits, 1920 is four), which would make the resolution leg of the matrix
/// vary transport cost as well as pixel work — a confound in exactly the
/// comparison the leg exists to make. Sized above the widest encoding any
/// geometry produces, and asserted to be so.
pub const SURFACE_REFERENCE_BODY_BYTES: usize = 192;

/// Encode a surface reference for `width_pixels`x`height_pixels` at
/// `frames_per_second`, returning the bytes that ride behind the preamble.
///
/// The body is built once at setup and reused unchanged for every frame: the
/// measured quantity is transport plus callback cost, both of which turn on the
/// body's *width*, and re-encoding per frame would add a cost the real path —
/// which encodes its bag once per frame in the producer, off this measurement's
/// critical path — does not have here.
///
/// The result is zero-padded to [`SURFACE_REFERENCE_BODY_BYTES`]. Padding is
/// invisible to every consumer here because the spike's ports are declared
/// `any`, so no schema is ever consulted and the body is opaque bytes.
pub fn encode_surface_reference_body(
    width_pixels: u32,
    height_pixels: u32,
    frames_per_second: u32,
) -> Vec<u8> {
    let reference = RepresentativeVideoFrameSurfaceReference::for_geometry(
        width_pixels,
        height_pixels,
        frames_per_second,
    );
    // Infallible for this struct — every member is a plain scalar or String.
    let mut body = serde_json::to_vec(&reference).unwrap_or_default();
    debug_assert!(
        body.len() <= SURFACE_REFERENCE_BODY_BYTES,
        "a {}x{} surface reference encodes to {} bytes, over the fixed wire width of {}",
        width_pixels,
        height_pixels,
        body.len(),
        SURFACE_REFERENCE_BODY_BYTES
    );
    body.resize(SURFACE_REFERENCE_BODY_BYTES, 0u8);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The protocol default must be the reference mode. A silent flip back to
    /// full-pixel would reproduce the retracted 27ms artifact and report it as a
    /// gated number.
    #[test]
    fn the_default_wire_payload_mode_is_surface_reference() {
        assert_eq!(
            SyntheticFrameWirePayloadMode::default(),
            SyntheticFrameWirePayloadMode::SurfaceReference
        );
    }

    /// A surface reference has to stay in the hundreds of bytes to be worth the
    /// name. If the schema grows a large member this fails rather than quietly
    /// turning the reference cell into a payload cell.
    #[test]
    fn the_surface_reference_body_weighs_a_few_hundred_bytes() {
        let body = encode_surface_reference_body(1920, 1080, 60);
        assert!(
            (64..1024).contains(&body.len()),
            "surface reference body is {} bytes, which is no longer reference-sized",
            body.len()
        );
    }

    /// The reference body's width must not track the geometry it describes —
    /// that independence is the entire point, and it is what makes a 1080p60
    /// cell measurable where a full-pixel one saturates. It must hold across
    /// digit-count boundaries, which is where the unpadded encoding differed.
    #[test]
    fn the_surface_reference_body_width_is_independent_of_frame_geometry() {
        let widths: Vec<usize> = [(640, 480), (1280, 720), (1920, 1080), (3840, 2160)]
            .into_iter()
            .map(|(width, height)| encode_surface_reference_body(width, height, 30).len())
            .collect();
        assert!(
            widths.iter().all(|width| *width == SURFACE_REFERENCE_BODY_BYTES),
            "surface reference widths varied across geometries: {widths:?}"
        );
    }

    /// The padding has to leave room for the widest geometry the matrix runs,
    /// or a 4K reference would be truncated into a shorter body and quietly
    /// measure a narrower wire than a 1080p one.
    #[test]
    fn the_fixed_wire_width_exceeds_the_widest_encoding_any_geometry_produces() {
        let widest = RepresentativeVideoFrameSurfaceReference::for_geometry(3840, 2160, 240);
        let encoded = serde_json::to_vec(&widest).expect("serializes");
        assert!(
            encoded.len() < SURFACE_REFERENCE_BODY_BYTES,
            "widest encoding is {} bytes against a fixed width of {}",
            encoded.len(),
            SURFACE_REFERENCE_BODY_BYTES
        );
    }

    /// 1080p BGRA is 8.29 MB; a surface reference must be smaller by orders of
    /// magnitude or the mode buys nothing.
    #[test]
    fn a_surface_reference_is_orders_of_magnitude_below_a_full_picture() {
        let body = encode_surface_reference_body(1920, 1080, 60);
        let full_picture_bytes = 1920usize * 1080 * 4;
        assert!(full_picture_bytes / body.len() > 10_000);
    }

    /// The token is part of the cell directory name, so two cells differing only
    /// by wire mode must not collide.
    #[test]
    fn wire_payload_modes_have_distinct_artifact_tokens() {
        assert_ne!(
            SyntheticFrameWirePayloadMode::SurfaceReference.as_artifact_token(),
            SyntheticFrameWirePayloadMode::FullPixelPayload.as_artifact_token()
        );
    }

    /// Every token the harness prints must parse back, so a cell can be replayed
    /// from its own `cell-spec.json`.
    #[test]
    fn artifact_tokens_round_trip_through_the_flag_parser() {
        for mode in [
            SyntheticFrameWirePayloadMode::SurfaceReference,
            SyntheticFrameWirePayloadMode::FullPixelPayload,
        ] {
            assert_eq!(
                SyntheticFrameWirePayloadMode::parse_from_flag_value(mode.as_artifact_token()),
                Ok(mode)
            );
        }
        assert!(SyntheticFrameWirePayloadMode::parse_from_flag_value("latest").is_err());
    }

    /// The mode rides `cell-spec.json`, so it must survive serde in the same
    /// kebab-case form the flag uses.
    #[test]
    fn wire_payload_mode_round_trips_through_serde_in_flag_form() {
        let encoded = serde_json::to_value(SyntheticFrameWirePayloadMode::FullPixelPayload)
            .expect("serializes");
        assert_eq!(encoded, serde_json::json!("full-pixel-payload"));
        let decoded: SyntheticFrameWirePayloadMode =
            serde_json::from_value(encoded).expect("deserializes");
        assert_eq!(decoded, SyntheticFrameWirePayloadMode::FullPixelPayload);
    }
}
