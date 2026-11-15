// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a decoded picture is published as: the display window an SPS states and the H.273 colour it
//! signals.

use tracing::info;

use super::SimpleDecoder;
use super::decoded_picture_display_window::DecodedPictureDisplayWindow;

impl SimpleDecoder {
    /// The active SPS's display window, or the whole coded picture when no
    /// SPS has been parsed yet.
    pub(super) fn decoded_picture_display_window_or_whole_picture(
        &self,
    ) -> DecodedPictureDisplayWindow {
        self.decoded_picture_display_window.unwrap_or_else(|| {
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(
                self.coded_picture_width,
                self.coded_picture_height,
            )
        })
    }

    /// Record the coded extent and the display window a freshly parsed SPS
    /// states. A window the SPS's own offsets make unusable is refused by
    /// name and the whole coded picture published instead — a mis-cropped
    /// frame is worse than an uncropped one, because its buffer and its
    /// header agree on a picture that was never coded.
    pub(super) fn adopt_parsed_sps_geometry(
        &mut self,
        coded_width: u32,
        coded_height: u32,
        parsed_window: Option<DecodedPictureDisplayWindow>,
    ) {
        self.coded_picture_width = coded_width;
        self.coded_picture_height = coded_height;
        let window = parsed_window.unwrap_or_else(|| {
            tracing::warn!(
                codec = ?self.config.codec,
                coded_width,
                coded_height,
                "SPS conformance window does not describe a region inside the coded picture; \
                 publishing the whole coded picture"
            );
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(coded_width, coded_height)
        });
        // Said once per SPS, because a run whose decoded extent does not
        // match its coded one is exactly the thing a reader of these logs is
        // trying to confirm.
        if !window.crops_nothing(coded_width, coded_height) {
            info!(
                codec = ?self.config.codec,
                coded_width,
                coded_height,
                published_width = window.width,
                published_height = window.height,
                "SPS conformance window applied — decoded frames publish the cropped extent"
            );
        }
        self.decoded_picture_display_window = Some(window);
    }

    /// The colour a picture in `display_window` decoded under the active SPS
    /// is in: the signalled axes, with matrix and range filled by the decoded
    /// picture defaults.
    pub(super) fn decoded_picture_color_vui(
        &self,
        display_window: DecodedPictureDisplayWindow,
    ) -> streamlib_consumer_rhi::h273_color_vui::H273ColorVui {
        self.signalled_color_vui()
            .unwrap_or_default()
            .with_decoded_picture_matrix_and_range_defaults(
                display_window.width,
                display_window.height,
            )
    }

    /// The H.273 colour the active SPS's VUI signals, or `None` if no SPS has
    /// been parsed yet or it carried no VUI.
    ///
    /// Each axis is `Some(byte)` only if the bitstream actually carried it
    /// AND it isn't the H.273 "Unspecified" enumerant (value `2`).
    fn signalled_color_vui(&self) -> Option<streamlib_consumer_rhi::h273_color_vui::H273ColorVui> {
        match self.config.codec {
            crate::encode::Codec::H264 => {
                let parser = self.h264_parser.as_ref()?;
                let sps = parser.sps.as_ref()?;
                if !sps.flags.vui_parameters_present_flag {
                    return None;
                }
                let vui = &sps.vui;
                let primaries = decoded_byte_to_option(vui.colour_primaries);
                let transfer = decoded_byte_to_option(vui.transfer_characteristics);
                let matrix = decoded_byte_to_option(vui.matrix_coefficients);
                let full_range = if vui.video_signal_type_present_flag {
                    Some(vui.video_full_range_flag)
                } else {
                    None
                };
                build_color_vui(primaries, transfer, matrix, full_range)
            }
            crate::encode::Codec::H265 => {
                let parser = self.h265_parser.as_ref()?;
                let sps = parser.active_sps[0].as_ref()?;
                if !sps.flags.vui_parameters_present_flag {
                    return None;
                }
                let vui = &sps.vui;
                let primaries = decoded_byte_to_option(vui.colour_primaries as i32);
                let transfer = decoded_byte_to_option(vui.transfer_characteristics as i32);
                let matrix = decoded_byte_to_option(vui.matrix_coeffs as i32);
                let full_range = if vui.flags.video_signal_type_present_flag {
                    Some(vui.flags.video_full_range_flag)
                } else {
                    None
                };
                build_color_vui(primaries, transfer, matrix, full_range)
            }
        }
    }
}

/// Map an H.273 byte parsed from the bitstream to `Some(byte)` if it is a
/// real value, or `None` if it is the H.273 "Unspecified" enumerant
/// (value `2`) or out-of-range. Returning `None` for `Unspecified` means
/// `signalled_color_vui()` axes match the on-wire ColorInfo semantics ("absent
/// IS unknown").
fn decoded_byte_to_option(value: i32) -> Option<u8> {
    if value <= 0 || value > 255 {
        return None;
    }
    let byte = value as u8;
    if byte == streamlib_consumer_rhi::h273_color_vui::H273_UNSPECIFIED {
        return None;
    }
    Some(byte)
}

/// Build an [`H273ColorVui`] from per-axis options, returning `None` when
/// every axis is `None` (so callers can disambiguate "no VUI" from "VUI
/// present but every axis was Unspecified").
fn build_color_vui(
    primaries: Option<u8>,
    transfer: Option<u8>,
    matrix: Option<u8>,
    full_range: Option<bool>,
) -> Option<streamlib_consumer_rhi::h273_color_vui::H273ColorVui> {
    if primaries.is_none() && transfer.is_none() && matrix.is_none() && full_range.is_none() {
        return None;
    }
    Some(streamlib_consumer_rhi::h273_color_vui::H273ColorVui {
        primaries,
        transfer,
        matrix,
        full_range,
    })
}

#[cfg(test)]
mod color_vui_helper_tests {
    use super::*;
    use streamlib_consumer_rhi::h273_color_vui;

    #[test]
    fn unspecified_byte_becomes_none() {
        assert_eq!(decoded_byte_to_option(2), None);
    }

    #[test]
    fn zero_or_negative_byte_becomes_none() {
        assert_eq!(decoded_byte_to_option(0), None);
        assert_eq!(decoded_byte_to_option(-1), None);
    }

    #[test]
    fn real_byte_becomes_some() {
        assert_eq!(decoded_byte_to_option(1), Some(1)); // BT.709
        assert_eq!(decoded_byte_to_option(13), Some(13)); // sRGB
        assert_eq!(decoded_byte_to_option(16), Some(16)); // PQ
    }

    #[test]
    fn all_axes_none_returns_none_vui() {
        assert!(build_color_vui(None, None, None, None).is_none());
    }

    #[test]
    fn full_range_alone_returns_some_vui() {
        let vui =
            build_color_vui(None, None, None, Some(true)).expect("non-empty axis yields Some");
        assert_eq!(vui.full_range, Some(true));
        assert!(vui.primaries.is_none());
    }

    #[test]
    fn hdr10_axes_round_trip_to_h273_bytes() {
        let vui = build_color_vui(
            Some(h273_color_vui::primaries::BT2020),
            Some(h273_color_vui::transfer::SMPTE2084),
            Some(h273_color_vui::matrix::BT2020_NCL),
            Some(true),
        )
        .expect("non-empty axes yield Some");
        assert_eq!(vui.primaries, Some(9));
        assert_eq!(vui.transfer, Some(16));
        assert_eq!(vui.matrix, Some(9));
        assert_eq!(vui.full_range, Some(true));
    }
}
