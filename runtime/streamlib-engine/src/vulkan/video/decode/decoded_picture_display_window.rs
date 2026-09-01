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
//!
//! Every offset here comes off a bitstream an untrusted producer wrote, and
//! H.264's reach the full `u32` range, so all of the arithmetic is checked.
//! A malformed window is refused, never wrapped into a plausible-looking one.

/// The crop offsets an SPS states, in chroma samples. Both codecs spell the
/// same four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpsCropOffsets {
    pub(crate) left: u32,
    pub(crate) right: u32,
    pub(crate) top: u32,
    pub(crate) bottom: u32,
}

/// The H.264 SPS syntax elements that decide a frame-cropping window.
#[derive(Debug, Clone, Copy)]
pub(crate) struct H264FrameCroppingSyntax {
    pub(crate) chroma_format_idc: u8,
    pub(crate) separate_colour_plane_flag: bool,
    pub(crate) frame_mbs_only_flag: bool,
    pub(crate) frame_cropping_flag: bool,
    pub(crate) offsets: SpsCropOffsets,
}

/// The H.265 SPS syntax elements that decide a conformance window.
#[derive(Debug, Clone, Copy)]
pub(crate) struct H265ConformanceWindowSyntax {
    pub(crate) chroma_format_idc: u8,
    pub(crate) conformance_window_flag: bool,
    pub(crate) offsets: SpsCropOffsets,
}

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

    /// Whether the window is the whole coded picture, which is the case for
    /// every extent already aligned to the codec's block size.
    pub(crate) fn crops_nothing(&self, coded_width: u32, coded_height: u32) -> bool {
        *self == Self::covering_the_whole_coded_picture(coded_width, coded_height)
    }

    /// The window `offsets` name, once the codec has said how many luma
    /// samples one offset is worth. Both codecs' windows are this function;
    /// they differ only in deriving the units.
    ///
    /// `None` for any window that does not name a non-empty region inside the
    /// coded picture, including one whose own offsets overflow.
    fn from_crop_units(
        coded_width: u32,
        coded_height: u32,
        crop_unit_x: u32,
        crop_unit_y: u32,
        offsets: SpsCropOffsets,
    ) -> Option<Self> {
        let cropped_from_the_sides = offsets
            .left
            .checked_add(offsets.right)?
            .checked_mul(crop_unit_x)?;
        let cropped_from_the_ends = offsets
            .top
            .checked_add(offsets.bottom)?
            .checked_mul(crop_unit_y)?;

        let window = Self {
            origin_x: crop_unit_x.checked_mul(offsets.left)?,
            origin_y: crop_unit_y.checked_mul(offsets.top)?,
            width: coded_width.checked_sub(cropped_from_the_sides)?,
            height: coded_height.checked_sub(cropped_from_the_ends)?,
        };
        window
            .fits_inside(coded_width, coded_height)
            .then_some(window)
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
/// offset back to luma samples. Monochrome and 4:4:4 share the (1, 1) pair,
/// so a separate-colour-plane stream — which both specs treat as monochrome —
/// needs no branch of its own.
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
pub(crate) fn h265_conformance_window(
    coded_width: u32,
    coded_height: u32,
    syntax: H265ConformanceWindowSyntax,
) -> Option<DecodedPictureDisplayWindow> {
    if !syntax.conformance_window_flag {
        return Some(
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(
                coded_width,
                coded_height,
            ),
        );
    }
    let (sub_width_c, sub_height_c) = chroma_subsampling_factors(syntax.chroma_format_idc);
    DecodedPictureDisplayWindow::from_crop_units(
        coded_width,
        coded_height,
        sub_width_c,
        sub_height_c,
        syntax.offsets,
    )
}

/// The window an H.264 SPS's frame cropping names (Rec. ITU-T H.264
/// §7.4.2.1.1, equations 7-19 through 7-22).
///
/// The one place the two codecs' crop math genuinely differs: `CropUnitY`
/// carries the field factor `2 - frame_mbs_only_flag`, so a field-coded
/// stream crops in units twice as tall as a frame-coded one.
pub(crate) fn h264_frame_cropping_window(
    coded_width: u32,
    coded_height: u32,
    syntax: H264FrameCroppingSyntax,
) -> Option<DecodedPictureDisplayWindow> {
    if !syntax.frame_cropping_flag {
        return Some(
            DecodedPictureDisplayWindow::covering_the_whole_coded_picture(
                coded_width,
                coded_height,
            ),
        );
    }
    // A separate-colour-plane stream codes each plane as monochrome, so its
    // crop units are the monochrome ones whatever `chroma_format_idc` says.
    let effective_chroma_format_idc = if syntax.separate_colour_plane_flag {
        0
    } else {
        syntax.chroma_format_idc
    };
    let (crop_unit_x, sub_height_c) = chroma_subsampling_factors(effective_chroma_format_idc);
    DecodedPictureDisplayWindow::from_crop_units(
        coded_width,
        coded_height,
        crop_unit_x,
        sub_height_c * (2 - u32::from(syntax.frame_mbs_only_flag)),
        syntax.offsets,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offsets(left: u32, right: u32, top: u32, bottom: u32) -> SpsCropOffsets {
        SpsCropOffsets {
            left,
            right,
            top,
            bottom,
        }
    }

    fn h265(
        chroma_format_idc: u8,
        conformance_window_flag: bool,
        offsets: SpsCropOffsets,
    ) -> H265ConformanceWindowSyntax {
        H265ConformanceWindowSyntax {
            chroma_format_idc,
            conformance_window_flag,
            offsets,
        }
    }

    fn h264(
        chroma_format_idc: u8,
        separate_colour_plane_flag: bool,
        frame_mbs_only_flag: bool,
        frame_cropping_flag: bool,
        offsets: SpsCropOffsets,
    ) -> H264FrameCroppingSyntax {
        H264FrameCroppingSyntax {
            chroma_format_idc,
            separate_colour_plane_flag,
            frame_mbs_only_flag,
            frame_cropping_flag,
            offsets,
        }
    }

    /// The 4:2:0 CTU pad this engine's own encoder emits: 1080 is not a
    /// multiple of 64, so `aligned_h` is 1088 and the encoder writes
    /// `conf_win_bottom_offset = (1088 - 1080) / 2 = 4`. Four chroma rows
    /// times `SubHeightC` = 2 is the eight luma rows that come back off.
    #[test]
    fn the_ctu_pad_this_engines_h265_encoder_emits_crops_back_to_1080() {
        let window = h265_conformance_window(1920, 1088, h265(1, true, offsets(0, 0, 0, 4)))
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
        let height_for = |chroma_format_idc| {
            h265_conformance_window(
                1920,
                1088,
                h265(chroma_format_idc, true, offsets(0, 0, 0, 4)),
            )
            .expect("a usable window")
            .height
        };
        assert_eq!(height_for(1), 1080); // 4:2:0
        assert_eq!(height_for(2), 1084); // 4:2:2
        assert_eq!(height_for(3), 1084); // 4:4:4
        assert_eq!(height_for(0), 1084); // monochrome shares 4:4:4's pair
    }

    /// A stream whose extent is already CTU-aligned carries no window, and
    /// the decoder must publish every coded row rather than assuming a pad.
    #[test]
    fn an_aligned_extent_carries_no_window_and_crops_nothing() {
        let window = h265_conformance_window(1280, 704, h265(1, false, offsets(0, 0, 0, 0)))
            .expect("an absent window is not a malformed one");
        assert!(window.crops_nothing(1280, 704));
    }

    /// Left and top offsets move the origin rather than shrinking from the
    /// far edge, which is what a readback has to honour to copy the picture
    /// and not the padding.
    #[test]
    fn a_left_top_offset_moves_the_origin_rather_than_shrinking_the_far_edge() {
        let window = h265_conformance_window(1920, 1088, h265(1, true, offsets(8, 8, 2, 2)))
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
    }

    /// A window that would crop past the coded picture is malformed. The
    /// caller falls back to the whole picture; publishing a wrapped or
    /// zero extent would describe a buffer that does not exist.
    #[test]
    fn a_window_cropping_past_the_coded_picture_is_refused_rather_than_wrapped() {
        let refused = |o| h265_conformance_window(1920, 1088, h265(1, true, o));
        assert_eq!(refused(offsets(0, 0, 0, 600)), None);
        assert_eq!(refused(offsets(2000, 0, 0, 0)), None);
        // Exactly consumed is empty, not a picture.
        assert_eq!(
            h265_conformance_window(64, 64, h265(1, true, offsets(0, 32, 0, 0))),
            None
        );
    }

    /// The offsets come off a bitstream an untrusted producer wrote, and
    /// H.264's reach the full `u32` range: the parser stores them as `i32`
    /// from an Exp-Golomb read that returns `code_num as i32`, and the SPS
    /// handler casts straight back with `as u32`. Summing a pair must not
    /// overflow — that panics in a debug build, which is what the round-trip
    /// harness runs under, and wraps into a plausible-looking window in a
    /// release one.
    #[test]
    fn offsets_that_overflow_their_own_sum_are_refused_rather_than_wrapped() {
        let all = u32::MAX;
        let frame_coded = |o| h264_frame_cropping_window(1920, 1088, h264(1, false, true, true, o));
        assert_eq!(frame_coded(offsets(all, all, 0, 0)), None);
        assert_eq!(frame_coded(offsets(0, 0, all, all)), None);
        // A single offset large enough to overflow the unit multiply too.
        assert_eq!(frame_coded(offsets(all, 0, 0, 0)), None);

        let windowed = |o| h265_conformance_window(1920, 1088, h265(1, true, o));
        assert_eq!(windowed(offsets(all, all, 0, 0)), None);
        assert_eq!(windowed(offsets(0, 0, all, all)), None);
        assert_eq!(windowed(offsets(all, 0, 0, 0)), None);
    }

    /// H.264 states the same window in macroblock-padded terms: 1080 is not
    /// a multiple of 16 either, so a 1088-tall coded picture crops back the
    /// same way.
    #[test]
    fn h264_frame_cropping_takes_two_luma_rows_per_offset_when_frame_coded() {
        let window =
            h264_frame_cropping_window(1920, 1088, h264(1, false, true, true, offsets(0, 0, 0, 4)))
                .expect("the 1088 -> 1080 crop must be usable");
        assert_eq!((window.width, window.height), (1920, 1080));
    }

    /// A field-coded stream doubles `CropUnitY`, so the same offset takes
    /// twice as many rows. This is the one place the two codecs' crop math
    /// genuinely differs.
    #[test]
    fn h264_field_coding_doubles_the_rows_an_offset_takes() {
        let height_when_frame_mbs_only = |frame_mbs_only_flag| {
            h264_frame_cropping_window(
                1920,
                1088,
                h264(1, false, frame_mbs_only_flag, true, offsets(0, 0, 0, 2)),
            )
            .expect("a usable window")
            .height
        };
        assert_eq!(height_when_frame_mbs_only(true), 1084);
        assert_eq!(height_when_frame_mbs_only(false), 1080);
    }

    /// A separate-colour-plane stream codes each plane as monochrome, so its
    /// crop unit is 1 horizontally however `chroma_format_idc` reads.
    #[test]
    fn h264_separate_colour_planes_crop_in_monochrome_units() {
        let planar =
            h264_frame_cropping_window(1920, 1088, h264(3, true, true, true, offsets(0, 4, 0, 0)))
                .expect("separate-plane window");
        let subsampled =
            h264_frame_cropping_window(1920, 1088, h264(1, false, true, true, offsets(0, 4, 0, 0)))
                .expect("4:2:0 window");
        assert_eq!(planar.width, 1916);
        assert_eq!(subsampled.width, 1912);
    }

    #[test]
    fn an_h264_window_cropping_past_the_coded_picture_is_refused_too() {
        assert_eq!(
            h264_frame_cropping_window(
                1920,
                1088,
                h264(1, false, true, true, offsets(0, 0, 0, 600))
            ),
            None
        );
        assert_eq!(
            h264_frame_cropping_window(
                320,
                240,
                h264(1, false, true, false, offsets(0, 0, 0, 600))
            ),
            Some(DecodedPictureDisplayWindow::covering_the_whole_coded_picture(320, 240)),
            "an absent cropping flag ignores the offsets entirely"
        );
    }
}
