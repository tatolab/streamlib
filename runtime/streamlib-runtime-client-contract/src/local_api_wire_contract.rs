// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The names a runtime's local API speaks on its socket, and the shape of the `tap` tool's
//! result, written once for the runtime that serves them and every client that calls them.

use serde::{Deserialize, Serialize};

/// The route MCP's Streamable HTTP transport is served on.
pub const MCP_STREAMABLE_HTTP_ROUTE_PATH: &str = "/mcp";

/// The request target a client upgrades to MCP's stdio framing on.
pub const MCP_STDIO_UPGRADE_REQUEST_TARGET: &str = "/mcp/stdio";

/// The `Upgrade` protocol token [`MCP_STDIO_UPGRADE_REQUEST_TARGET`] switches to, matched
/// case-insensitively.
pub const MCP_STDIO_UPGRADE_PROTOCOL_TOKEN: &str = "mcp-stdio";

/// The surface-image exchange as an OpenAPI path template: a published surface id in, that
/// frame's exact pixels as a PNG out.
pub const SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE: &str = "/api/surfaces/{surface_id}/image";

/// The placeholder in [`SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE`] the percent-encoded id fills.
const SURFACE_IMAGE_EXCHANGE_ROUTE_SURFACE_ID_PLACEHOLDER: &str = "{surface_id}";

/// RFC 3986's unreserved set: everything outside it is percent-encoded into the route's
/// `{surface_id}` segment. A pooled frame id is `<slot>#<generation>`, and a bare `#` would make
/// the generation a URL fragment the runtime never sees.
const SURFACE_ID_PATH_SEGMENT_PERCENT_ENCODE_ASCII_SET: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');

/// The header carrying the width the exchanged surface's own backing holds. A downscaled image's
/// PNG header states only what was encoded, so this is how a caller learns the true extent.
pub const SURFACE_PIXEL_WIDTH_HEADER_NAME: &str = "x-streamlib-surface-pixel-width";

/// Height counterpart of [`SURFACE_PIXEL_WIDTH_HEADER_NAME`].
pub const SURFACE_PIXEL_HEIGHT_HEADER_NAME: &str = "x-streamlib-surface-pixel-height";

/// The status the exchange answers for an id whose frame's pool slot has since been recycled:
/// the id was real and the frame is gone, so a newer bag answers it where a `404` never will.
pub const RECYCLED_FRAME_HTTP_STATUS_CODE: u16 = 410;

/// The exchange route's path for `published_surface_id`, ready to put on the wire.
pub fn surface_image_exchange_route_path_for_surface_id(published_surface_id: &str) -> String {
    let percent_encoded_surface_id = percent_encoding::utf8_percent_encode(
        published_surface_id,
        SURFACE_ID_PATH_SEGMENT_PERCENT_ENCODE_ASCII_SET,
    )
    .to_string();
    SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE.replace(
        SURFACE_IMAGE_EXCHANGE_ROUTE_SURFACE_ID_PLACEHOLDER,
        &percent_encoded_surface_id,
    )
}

/// The `tap` tool's result: a bounded sample of the bags one channel carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapToolResult {
    /// The tapped output port's address, `<runtime_name>/<node>/<port>`.
    pub channel: String,
    /// Bags the sample asked for, after the runtime bounded the caller's count.
    pub requested: usize,
    /// Bags the sample carries.
    pub received: usize,
    /// The longest the runtime waited to fill the sample.
    pub window_ms: u64,
    /// Bags the tap dropped because the sample fell behind the channel; a tap drops rather than
    /// back-pressure the source.
    pub dropped_bags: u64,
    /// The per-bag ceiling on the bytes hex-encoded into [`TapToolResultBag::hex_preview`].
    pub max_bag_bytes: usize,
    /// Bags received and discarded because they would have overrun the whole result's byte
    /// budget: 0 or 1, since the sample stops at the first.
    pub bags_withheld_at_byte_budget: usize,
    /// The sampled bags, in the order the channel carried them.
    pub bags: Vec<TapToolResultBag>,
}

/// One bag of a [`TapToolResult`]: the frame header and payload exactly as the channel carried
/// them, hex-encoded up to [`TapToolResult::max_bag_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapToolResultBag {
    /// The whole bag's length, frame header included, whether or not the preview holds all of it.
    pub byte_len: u64,
    /// Lowercase hex, two digits per byte with nothing between them.
    pub hex_preview: String,
    /// Whether the preview holds less than the whole bag; a capped bag cannot be decoded.
    pub hex_truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare `#` would make the generation a URL fragment the runtime never sees.
    #[test]
    fn a_pooled_frame_id_is_percent_encoded_into_the_exchange_route() {
        assert_eq!(
            surface_image_exchange_route_path_for_surface_id("cam/frame#7"),
            "/api/surfaces/cam%2Fframe%237/image"
        );
    }

    #[test]
    fn a_surface_id_is_encoded_down_to_the_unreserved_set() {
        assert_eq!(
            surface_image_exchange_route_path_for_surface_id("Az09-._~ é#/?%"),
            "/api/surfaces/Az09-._~%20%C3%A9%23%2F%3F%25/image"
        );
    }

    #[test]
    fn an_empty_surface_id_fills_an_empty_segment() {
        assert_eq!(
            surface_image_exchange_route_path_for_surface_id(""),
            "/api/surfaces//image"
        );
    }

    fn tap_tool_result_carrying(bags: Vec<TapToolResultBag>) -> TapToolResult {
        TapToolResult {
            channel: "studio/camera/video".to_owned(),
            requested: 8,
            received: bags.len(),
            window_ms: 500,
            dropped_bags: 2,
            max_bag_bytes: 1024,
            bags_withheld_at_byte_budget: 0,
            bags,
        }
    }

    /// The keys and JSON types a caller parses, spelled as the wire spells them.
    #[test]
    fn a_tap_tool_result_serializes_every_key_with_its_wire_type() {
        let serialized = serde_json::to_value(tap_tool_result_carrying(vec![TapToolResultBag {
            byte_len: 9000,
            hex_preview: "0aff".to_owned(),
            hex_truncated: true,
        }]))
        .unwrap();

        assert_eq!(
            serialized,
            serde_json::json!({
                "channel": "studio/camera/video",
                "requested": 8,
                "received": 1,
                "window_ms": 500,
                "dropped_bags": 2,
                "max_bag_bytes": 1024,
                "bags_withheld_at_byte_budget": 0,
                "bags": [{"byte_len": 9000, "hex_preview": "0aff", "hex_truncated": true}],
            })
        );
    }

    #[test]
    fn a_tap_tool_result_round_trips_through_its_json() {
        let tap_tool_result = tap_tool_result_carrying(vec![
            TapToolResultBag {
                byte_len: 2,
                hex_preview: "0aff".to_owned(),
                hex_truncated: false,
            },
            TapToolResultBag {
                byte_len: u64::MAX,
                hex_preview: String::new(),
                hex_truncated: true,
            },
        ]);

        let serialized = serde_json::to_string(&tap_tool_result).unwrap();

        assert_eq!(
            serde_json::from_str::<TapToolResult>(&serialized).unwrap(),
            tap_tool_result
        );
    }

    /// A flag is a bool and a length a whole number: a result spelling either another way is not
    /// the tap tool's.
    #[test]
    fn a_bag_whose_flag_or_length_has_another_json_type_is_refused() {
        for misspelled_bag in [
            serde_json::json!({"byte_len": 5, "hex_preview": "", "hex_truncated": 1}),
            serde_json::json!({"byte_len": 5, "hex_preview": "", "hex_truncated": "yes"}),
            serde_json::json!({"byte_len": 5.0, "hex_preview": "", "hex_truncated": false}),
            serde_json::json!({"byte_len": "5", "hex_preview": "", "hex_truncated": false}),
            serde_json::json!({"byte_len": -5, "hex_preview": "", "hex_truncated": false}),
            serde_json::json!({"hex_preview": "", "hex_truncated": false}),
        ] {
            assert!(
                serde_json::from_value::<TapToolResultBag>(misspelled_bag.clone()).is_err(),
                "{misspelled_bag}"
            );
        }
    }
}
