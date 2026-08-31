// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The region of a coded picture a decoder actually publishes.
//!
//! Both codecs pad the coded picture up to a block size — H.264 to the
//! 16-sample macroblock, H.265 to the CTU, which is 64 samples wide on every
//! encoder this engine mints — and then carry a window in the SPS naming the
//! sub-region that is the real picture. 1080 is a multiple of neither, so a
//! 1920x1080 source is coded at 1920x1088 by both and only the window brings
//! it back.
//!
//! The window is the decoder's business, never a consumer's: a consumer
//! handed the coded extent has no way to know which of the two numbers it
//! holds, and the padding rows are edge-replicated garbage.

/// The sub-region of a coded picture that a decoded frame is published as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodedPictureDisplayWindow {
    /// Left edge of the window within the coded picture, in luma samples.
    pub(crate) origin_x: u32,
    /// Top edge of the window within the coded picture, in luma samples.
    pub(crate) origin_y: u32,
    /// Published width in luma samples.
    pub(crate) width: u32,
    /// Published height in luma samples.
    pub(crate) height: u32,
}

impl DecodedPictureDisplayWindow {
    /// The whole coded picture — what a stream carrying no window publishes,
    /// and the fallback for one whose window does not describe a region
    /// inside it.
    pub(crate) fn covering_the_whole_coded_picture(coded_width: u32, coded_height: u32) -> Self {
        Self {
            origin_x: 0,
            origin_y: 0,
            width: coded_width,
            height: coded_height,
        }
    }

    /// The extent an RGBA readback must cover to reach this window's far
    /// edge — the origin is inside the coded picture, so a converter sized
    /// for the window alone would have nothing to read at a non-zero origin.
    pub(crate) fn extent_a_readback_must_span(&self) -> (u32, u32) {
        (self.origin_x + self.width, self.origin_y + self.height)
    }

    /// Whether the window is the whole coded picture, which is the case for
    /// every extent already aligned to the codec's block size.
    pub(crate) fn crops_nothing(&self, coded_width: u32, coded_height: u32) -> bool {
        *self == Self::covering_the_whole_coded_picture(coded_width, coded_height)
    }

    /// A window is usable only when it names a non-empty region that fits
    /// inside the coded picture. Anything else is a malformed SPS, and
    /// publishing an underflowed or zero extent from one would hand a
    /// consumer a frame whose buffer cannot hold what its header claims.
    fn fits_inside(&self, coded_width: u32, coded_height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && self.origin_x + self.width <= coded_width
            && self.origin_y + self.height <= coded_height
    }
}

/// `SubWidthC` / `SubHeightC` for a chroma format (Rec. ITU-T H.265 table
/// 6-1, and H.264 table 6-1, which agree). Both codecs state their crop
/// offsets in chroma samples, so these are the multipliers that bring an
/// offset back to luma samples. A `separate_colour_plane_flag` stream is
/// treated as monochrome by both specs, and monochrome and 4:4:4 share the
/// (1, 1) pair, so that case needs no branch of its own.
fn chroma_subsampling_factors(chroma_format_idc: u8) -> (u32, u32) {
    match chroma_format_idc {
        0 => (1, 1), // monochrome
        2 => (2, 1), // 4:2:2
        3 => (1, 1), // 4:4:4
        _ => (2, 2), // 4:2:0
    }
}

/// The window an H.265 SPS's conformance window names (Rec. ITU-T H.265
/// §7.4.3.2, equations 7-19 through 7-22).
///
/// `None` when the offsets do not describe a non-empty region inside the
/// coded picture; the caller falls back to the whole picture rather than
/// publishing an extent no buffer matches.
pub(crate) fn h265_conformance_window(
    coded_width: u32,
    coded_height: u32,
    chroma_format_idc: u8,
    conformance_window_flag: bool,
    conf_win_left_offset: u32,
    conf_win_right_offset: u32,
    conf_win_top_offset: u32,
    conf_win_bottom_offset: u32,
) -> Option<DecodedPictureDisplayWindow> {
    if !conformance_window_flag {
        return Some(
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(
                coded_width,
                coded_height,
            ),
        );
    }
    let (sub_width_c, sub_height_c) = chroma_subsampling_factors(chroma_format_idc);
    let cropped_from_the_sides =
        sub_width_c.checked_mul(conf_win_left_offset + conf_win_right_offset)?;
    let cropped_from_the_ends =
        sub_height_c.checked_mul(conf_win_top_offset + conf_win_bottom_offset)?;

    let window = DecodedPictureDisplayWindow {
        origin_x: sub_width_c * conf_win_left_offset,
        origin_y: sub_height_c * conf_win_top_offset,
        width: coded_width.checked_sub(cropped_from_the_sides)?,
        height: coded_height.checked_sub(cropped_from_the_ends)?,
    };
    window
        .fits_inside(coded_width, coded_height)
        .then_some(window)
}

/// The window an H.264 SPS's frame cropping names (Rec. ITU-T H.264
/// §7.4.2.1.1, equations 7-19 through 7-22).
///
/// `CropUnitY` carries the field factor `2 - frame_mbs_only_flag`, which is
/// why a field-coded stream crops in units twice as tall as a frame-coded
/// one. `None` on a malformed window, as for H.265.
pub(crate) fn h264_frame_cropping_window(
    coded_width: u32,
    coded_height: u32,
    chroma_format_idc: u8,
    separate_colour_plane_flag: bool,
    frame_mbs_only_flag: bool,
    frame_cropping_flag: bool,
    frame_crop_left_offset: u32,
    frame_crop_right_offset: u32,
    frame_crop_top_offset: u32,
    frame_crop_bottom_offset: u32,
) -> Option<DecodedPictureDisplayWindow> {
    if !frame_cropping_flag {
        return Some(
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(
                coded_width,
                coded_height,
            ),
        );
    }
    // A separate-colour-plane stream codes each plane as monochrome, so its
    // crop units are the monochrome ones whatever `chroma_format_idc` says.
    let effective_chroma_format_idc = if separate_colour_plane_flag {
        0
    } else {
        chroma_format_idc
    };
    let (crop_unit_x, sub_height_c) = chroma_subsampling_factors(effective_chroma_format_idc);
    let crop_unit_y = sub_height_c * (2 - u32::from(frame_mbs_only_flag));

    let cropped_from_the_sides =
        crop_unit_x.checked_mul(frame_crop_left_offset + frame_crop_right_offset)?;
    let cropped_from_the_ends =
        crop_unit_y.checked_mul(frame_crop_top_offset + frame_crop_bottom_offset)?;

    let window = DecodedPictureDisplayWindow {
        origin_x: crop_unit_x * frame_crop_left_offset,
        origin_y: crop_unit_y * frame_crop_top_offset,
        width: coded_width.checked_sub(cropped_from_the_sides)?,
        height: coded_height.checked_sub(cropped_from_the_ends)?,
    };
    window
        .fits_inside(coded_width, coded_height)
        .then_some(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 4:2:0 CTU pad this engine's own encoder emits: 1080 is not a
    /// multiple of 64, so `aligned_h` is 1088 and the encoder writes
    /// `conf_win_bottom_offset = (1088 - 1080) / 2 = 4`. Four chroma rows
    /// times `SubHeightC` = 2 is the eight luma rows that come back off.
    #[test]
    fn the_ctu_pad_this_engines_h265_encoder_emits_crops_back_to_1080() {
        let window = h265_conformance_window(1920, 1088, 1, true, 0, 0, 0, 4)
            .expect("the encoder's own window must be usable");
        assert_eq!(
            window,
            DecodedPictureDisplayWindow {
                origin_x: 0,
                origin_y: 0,
                width: 1920,
                height: 1080,
            }
        );
        assert!(!window.crops_nothing(1920, 1088));
    }

    /// 4:2:2 halves `SubHeightC`, so the same offset takes half as many rows.
    /// A decoder that hardcoded the 4:2:0 pair would publish 1080 here too,
    /// which is eight rows of padding short of the picture.
    #[test]
    fn the_chroma_format_decides_how_many_luma_rows_an_offset_takes() {
        let window_420 =
            h265_conformance_window(1920, 1088, 1, true, 0, 0, 0, 4).expect("4:2:0 window");
        let window_422 =
            h265_conformance_window(1920, 1088, 2, true, 0, 0, 0, 4).expect("4:2:2 window");
        let window_444 =
            h265_conformance_window(1920, 1088, 3, true, 0, 0, 0, 4).expect("4:4:4 window");
        assert_eq!(window_420.height, 1080);
        assert_eq!(window_422.height, 1084);
        assert_eq!(window_444.height, 1084);
        // Monochrome shares 4:4:4's (1, 1) pair.
        assert_eq!(
            h265_conformance_window(1920, 1088, 0, true, 0, 0, 0, 4)
                .expect("monochrome window")
                .height,
            1084
        );
    }

    /// A stream whose extent is already CTU-aligned carries no window, and
    /// the decoder must publish every coded row rather than assuming a pad.
    #[test]
    fn an_aligned_extent_carries_no_window_and_crops_nothing() {
        let window = h265_conformance_window(1280, 704, 1, false, 0, 0, 0, 0)
            .expect("an absent window is not a malformed one");
        assert!(window.crops_nothing(1280, 704));
        assert_eq!(window.extent_a_readback_must_span(), (1280, 704));
    }

    /// Left and top offsets move the origin rather than shrinking from the
    /// far edge, which is what a readback has to honour to copy the picture
    /// and not the padding.
    #[test]
    fn a_left_top_offset_moves_the_origin_and_the_readback_spans_past_it() {
        let window = h265_conformance_window(1920, 1088, 1, true, 8, 8, 2, 2)
            .expect("an offset window is still a usable one");
        assert_eq!(
            window,
            DecodedPictureDisplayWindow {
                origin_x: 16,
                origin_y: 4,
                width: 1888,
                height: 1080,
            }
        );
        assert_eq!(window.extent_a_readback_must_span(), (1904, 1084));
    }

    /// A window that would crop past the coded picture is malformed. The
    /// caller falls back to the whole picture; publishing a wrapped or
    /// zero extent would describe a buffer that does not exist.
    #[test]
    fn a_window_cropping_past_the_coded_picture_is_refused_rather_than_wrapped() {
        assert_eq!(
            h265_conformance_window(1920, 1088, 1, true, 0, 0, 0, 600),
            None
        );
        assert_eq!(
            h265_conformance_window(1920, 1088, 1, true, 2000, 0, 0, 0),
            None
        );
        // Exactly consumed is empty, not a picture.
        assert_eq!(h265_conformance_window(64, 64, 1, true, 0, 32, 0, 0), None);
        // And the offsets are not allowed to overflow their own sum.
        assert_eq!(
            h265_conformance_window(1920, 1088, 1, true, u32::MAX, 0, 0, 0),
            None
        );
    }

    /// H.264 states the same window in macroblock-padded terms: 1080 is not
    /// a multiple of 16 either, so a 1088-tall coded picture crops back the
    /// same way. `CropUnitY` is `SubHeightC * (2 - frame_mbs_only_flag)`, so
    /// a frame-coded 4:2:0 stream takes two luma rows per offset.
    #[test]
    fn h264_frame_cropping_takes_two_luma_rows_per_offset_when_frame_coded() {
        let window = h264_frame_cropping_window(1920, 1088, 1, false, true, true, 0, 0, 0, 4)
            .expect("the 1088 -> 1080 crop must be usable");
        assert_eq!(window.width, 1920);
        assert_eq!(window.height, 1080);
    }

    /// A field-coded stream doubles `CropUnitY`, so the same offset takes
    /// twice as many rows. This is the one place the two codecs' crop math
    /// genuinely differs.
    #[test]
    fn h264_field_coding_doubles_the_rows_an_offset_takes() {
        let frame_coded = h264_frame_cropping_window(1920, 1088, 1, false, true, true, 0, 0, 0, 2)
            .expect("frame-coded window");
        let field_coded = h264_frame_cropping_window(1920, 1088, 1, false, false, true, 0, 0, 0, 2)
            .expect("field-coded window");
        assert_eq!(frame_coded.height, 1084);
        assert_eq!(field_coded.height, 1080);
    }

    /// A separate-colour-plane stream codes each plane as monochrome, so its
    /// crop unit is 1 horizontally however `chroma_format_idc` reads.
    #[test]
    fn h264_separate_colour_planes_crop_in_monochrome_units() {
        let planar = h264_frame_cropping_window(1920, 1088, 3, true, true, true, 0, 4, 0, 0)
            .expect("separate-plane window");
        let subsampled = h264_frame_cropping_window(1920, 1088, 1, false, true, true, 0, 4, 0, 0)
            .expect("4:2:0 window");
        assert_eq!(planar.width, 1916);
        assert_eq!(subsampled.width, 1912);
    }

    #[test]
    fn an_h264_window_cropping_past_the_coded_picture_is_refused_too() {
        assert_eq!(
            h264_frame_cropping_window(1920, 1088, 1, false, true, true, 0, 0, 0, 600),
            None
        );
        assert_eq!(
            h264_frame_cropping_window(320, 240, 1, false, true, false, 0, 0, 0, 600),
            Some(DecodedPictureDisplayWindow::covering_the_whole_coded_picture(320, 240)),
            "an absent cropping flag ignores the offsets entirely"
        );
    }
}
