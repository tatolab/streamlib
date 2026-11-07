// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Image frames on a `video` port: read by an encoder off a message's
//! descriptor, with the pooled frame it names lent and claimed, and written
//! by a decoder in the slot of the output-pool texture it converted into.

use streamlib_consumer_rhi::H273ColorVui;
use streamlib_consumer_rhi::h273_color_vui::{matrix, primaries, transfer};
use tatolab_module_sdk::{
    MODULE_HOST_H273_UNSPECIFIED, ModuleHostImageFrame, ModuleHostImageFrameH273Colour,
    ModuleHostReadMessage, ModuleHostSurface, ModuleHostTablePayloadDescriptor,
};

/// One Image frame an encoder reads: its pooled frame lent, claimed until
/// this drops, and what its descriptor states.
#[derive(Debug)]
pub struct ImageFrameReadByTheEncoder {
    lent_pooled_frame: ModuleHostSurface,
    pub width: u32,
    pub height: u32,
    /// Monotonic nanoseconds, the stamp the media frame carries.
    pub timestamp_ns: i64,
    pub color_vui: H273ColorVui,
}

impl ImageFrameReadByTheEncoder {
    /// Read one `video` message, refusing by name what is not an Image frame.
    pub fn of_the_video_message(message: &ModuleHostReadMessage) -> Result<Self, String> {
        let payload_descriptor = message
            .payload_descriptor()
            .map_err(|refusal| format!("a `video` message's descriptor: {refusal}"))?;
        let image_frame = ModuleHostImageFrame::of_the_payload_descriptor(&payload_descriptor)
            .map_err(|refusal| format!("a `video` message is not an Image frame: {refusal}"))?;
        let lent_pooled_frame = message
            .lend_pooled_gpu_frame()
            .map_err(|refusal| format!("a `video` message's frame: {refusal}"))?;
        Ok(Self {
            lent_pooled_frame,
            width: image_frame.width,
            height: image_frame.height,
            timestamp_ns: message.timestamp_ns(),
            color_vui: color_vui_of_the_image_frame_colour(image_frame.colour),
        })
    }

    /// The published surface the frame's pixels are checked out from.
    pub fn surface_id(&self) -> &str {
        self.lent_pooled_frame.surface_id()
    }

    /// The frame's surface, lent to convert from.
    pub fn lent_pooled_frame(&self) -> &ModuleHostSurface {
        &self.lent_pooled_frame
    }
}

/// The colour an Image frame's H.273 code points state, an unspecified axis
/// `None`.
pub fn color_vui_of_the_image_frame_colour(colour: ModuleHostImageFrameH273Colour) -> H273ColorVui {
    let stated =
        |code_point: u8| (code_point != MODULE_HOST_H273_UNSPECIFIED).then_some(code_point);
    H273ColorVui {
        primaries: stated(colour.primaries),
        transfer: stated(colour.transfer),
        matrix: stated(colour.matrix),
        full_range: match colour.range {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
    }
}

/// The H.273 code points `color_vui` states, an axis it leaves `None`
/// unspecified.
pub fn image_frame_colour_of_the_color_vui(
    color_vui: &H273ColorVui,
) -> ModuleHostImageFrameH273Colour {
    ModuleHostImageFrameH273Colour {
        primaries: color_vui.primaries.unwrap_or(MODULE_HOST_H273_UNSPECIFIED),
        transfer: color_vui.transfer.unwrap_or(MODULE_HOST_H273_UNSPECIFIED),
        matrix: color_vui.matrix.unwrap_or(MODULE_HOST_H273_UNSPECIFIED),
        range: color_vui
            .full_range
            .map_or(MODULE_HOST_H273_UNSPECIFIED, u8::from),
    }
}

/// The Image frame descriptor of a decoded picture converted into
/// `decoded_picture_texture`, an output-pool texture of
/// `texture_format_wire_name`.
pub fn image_frame_payload_descriptor_of_a_decoded_picture(
    decoded_picture_texture: &ModuleHostSurface,
    texture_format_wire_name: &str,
    width: u32,
    height: u32,
    color_vui: &H273ColorVui,
) -> Result<ModuleHostTablePayloadDescriptor, String> {
    let texture_format =
        ModuleHostImageFrame::texture_format_of_the_wire_name(texture_format_wire_name)
            .ok_or_else(|| format!("{texture_format_wire_name:?} names no texture format"))?;
    let gpu_pool_slot_frame = decoded_picture_texture
        .gpu_pool_slot_frame()
        .map_err(|refusal| refusal.to_string())?;
    ModuleHostImageFrame {
        width,
        height,
        texture_format,
        colour: image_frame_colour_of_the_color_vui(color_vui),
    }
    .payload_descriptor_in_the_slot(&gpu_pool_slot_frame)
    .map_err(|refusal| refusal.to_string())
}

/// `ColorInfo.primaries` wire names and the H.273 enumerants they stand for.
const PRIMARIES_WIRE_NAMES: &[(&str, u8)] = &[
    ("bt709", primaries::BT709),
    ("bt470_m", primaries::BT470_M),
    ("bt470_bg", primaries::BT470_BG),
    ("smpte170m", primaries::SMPTE170M),
    ("smpte240m", primaries::SMPTE240M),
    ("film", primaries::FILM),
    ("bt2020", primaries::BT2020),
    ("smpte428", primaries::SMPTE428),
    ("smpte431", primaries::SMPTE431),
    ("smpte432", primaries::SMPTE432),
    ("ebu3213", primaries::EBU3213),
];

/// `ColorInfo.transfer` wire names and the H.273 enumerants they stand for.
const TRANSFER_WIRE_NAMES: &[(&str, u8)] = &[
    ("bt709", transfer::BT709),
    ("gamma22", transfer::GAMMA22),
    ("gamma28", transfer::GAMMA28),
    ("smpte170m", transfer::SMPTE170M),
    ("smpte240m", transfer::SMPTE240M),
    ("linear", transfer::LINEAR),
    ("log100", transfer::LOG100),
    ("log100_sqrt10", transfer::LOG100_SQRT10),
    ("xvycc", transfer::XVYCC),
    ("bt1361", transfer::BT1361),
    ("srgb", transfer::SRGB),
    ("bt2020_ten_bit", transfer::BT2020_TEN_BIT),
    ("bt2020_twelve_bit", transfer::BT2020_TWELVE_BIT),
    ("smpte2084", transfer::SMPTE2084),
    ("smpte428", transfer::SMPTE428),
    ("arib_std_b67", transfer::ARIB_STD_B67),
];

/// `ColorInfo.matrix` wire names and the H.273 enumerants they stand for.
const MATRIX_WIRE_NAMES: &[(&str, u8)] = &[
    ("identity", matrix::IDENTITY),
    ("bt709", matrix::BT709),
    ("fcc", matrix::FCC),
    ("bt470_bg", matrix::BT470_BG),
    ("smpte170m", matrix::SMPTE170M),
    ("smpte240m", matrix::SMPTE240M),
    ("ycgco", matrix::YCGCO),
    ("bt2020_ncl", matrix::BT2020_NCL),
    ("bt2020_cl", matrix::BT2020_CL),
    ("smpte2085", matrix::SMPTE2085),
    ("chroma_ncl", matrix::CHROMA_NCL),
    ("chroma_cl", matrix::CHROMA_CL),
    ("ictcp", matrix::ICTCP),
];

fn wire_name_of_byte(wire_names: &[(&'static str, u8)], byte: u8) -> Option<&'static str> {
    wire_names
        .iter()
        .find(|(_, named_byte)| *named_byte == byte)
        .map(|&(name, _)| name)
}

/// A colour's `ColorInfo` wire names, which the engine's colour conversion
/// reads; an axis H.273 leaves unspecified or the vocabulary does not name is
/// `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct H273ColorVuiWireNames {
    pub primaries: Option<&'static str>,
    pub transfer: Option<&'static str>,
    pub matrix: Option<&'static str>,
    pub range: Option<&'static str>,
}

impl H273ColorVuiWireNames {
    /// The wire names `color_vui`'s enumerants stand for.
    pub fn of_h273_color_vui(color_vui: &H273ColorVui) -> Self {
        Self {
            primaries: color_vui
                .primaries
                .and_then(|byte| wire_name_of_byte(PRIMARIES_WIRE_NAMES, byte)),
            transfer: color_vui
                .transfer
                .and_then(|byte| wire_name_of_byte(TRANSFER_WIRE_NAMES, byte)),
            matrix: color_vui
                .matrix
                .and_then(|byte| wire_name_of_byte(MATRIX_WIRE_NAMES, byte)),
            range: color_vui
                .full_range
                .map(|full_range| if full_range { "full" } else { "limited" }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_reads_back_through_its_code_points_with_unspecified_axes_kept_absent() {
        let color_vui = H273ColorVui {
            primaries: Some(primaries::BT709),
            transfer: None,
            matrix: Some(matrix::SMPTE170M),
            full_range: Some(false),
        };
        let colour = image_frame_colour_of_the_color_vui(&color_vui);
        assert_eq!(colour.transfer, MODULE_HOST_H273_UNSPECIFIED);
        assert_eq!(color_vui_of_the_image_frame_colour(colour), color_vui);
    }

    #[test]
    fn a_colour_with_no_range_stated_reads_as_unspecified() {
        let colour = image_frame_colour_of_the_color_vui(&H273ColorVui::default());
        assert_eq!(colour.range, MODULE_HOST_H273_UNSPECIFIED);
        assert_eq!(color_vui_of_the_image_frame_colour(colour).full_range, None);
    }

    #[test]
    fn the_module_sdk_names_every_texture_format_as_the_engine_numbers_it() {
        use streamlib_consumer_rhi::TextureFormat;
        for repr_value in 0.. {
            let Some(texture_format) = TextureFormat::from_repr_value(repr_value) else {
                assert_eq!(
                    ModuleHostImageFrame::wire_name_of_the_texture_format(repr_value),
                    None
                );
                break;
            };
            assert_eq!(
                ModuleHostImageFrame::texture_format_of_the_wire_name(texture_format.wire_name()),
                Some(repr_value)
            );
        }
    }

    #[test]
    fn every_wire_name_round_trips_through_its_enumerant() {
        for wire_names in [PRIMARIES_WIRE_NAMES, TRANSFER_WIRE_NAMES, MATRIX_WIRE_NAMES] {
            for &(name, byte) in wire_names {
                assert_eq!(wire_name_of_byte(wire_names, byte), Some(name));
            }
        }
    }

    #[test]
    fn a_colours_wire_names_name_each_stated_axis() {
        let wire_names = H273ColorVuiWireNames::of_h273_color_vui(&H273ColorVui {
            primaries: Some(primaries::BT709),
            transfer: Some(transfer::SRGB),
            matrix: Some(matrix::BT709),
            full_range: Some(true),
        });
        assert_eq!(
            wire_names,
            H273ColorVuiWireNames {
                primaries: Some("bt709"),
                transfer: Some("srgb"),
                matrix: Some("bt709"),
                range: Some("full"),
            }
        );
    }
}
