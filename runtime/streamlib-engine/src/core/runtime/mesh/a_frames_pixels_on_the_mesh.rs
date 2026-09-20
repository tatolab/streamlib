// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One mesh message carrying a video frame: what describes its pixels, and how
//! the three parts sit in the payload.
//!
//! The message is `[pixel description][bag][pixel bytes]`. The description is
//! read off the *backing* the sender resolved, never off the bag — a video bag
//! names no pixel format at all, and a receiver guessing one would hand the
//! wrong channel order downstream and never say so.
//!
//! Whether a payload is one of these at all is not guessed from its first
//! bytes: the message's attachment carries the description's length, zero for
//! an ordinary bag, so a bag naming no surface still crosses verbatim and this
//! encoding never has to be told apart from a producer's own.
//!
//! The description is variable-length because the format is spelled by its
//! wire name rather than by a number: `PixelFormat`'s own vocabulary is what
//! every other surface in the engine speaks, and a private numbering here
//! would be a second one to keep in step.

use streamlib_consumer_rhi::PixelFormat;

/// Where each fixed field begins, and how long the fixed part of a description
/// is. The layout is the wire contract, little-endian like the attachment's.
const WIDTH_OFFSET: usize = 0;
const HEIGHT_OFFSET: usize = WIDTH_OFFSET + size_of::<u32>();
const PIXEL_BYTE_LENGTH_OFFSET: usize = HEIGHT_OFFSET + size_of::<u32>();
const FORMAT_WIRE_NAME_LENGTH_OFFSET: usize = PIXEL_BYTE_LENGTH_OFFSET + size_of::<u64>();
const FORMAT_WIRE_NAME_OFFSET: usize = FORMAT_WIRE_NAME_LENGTH_OFFSET + size_of::<u16>();

/// What a receiving runtime needs to rebuild a frame it is handed the pixels
/// of — all of it read from the sending backing, none of it from the bag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AFramesPixelDescriptionOnTheMesh {
    pub pixel_format: PixelFormat,
    pub width: u32,
    pub height: u32,
    /// How many bytes of pixels ride at the end of the message.
    pub pixel_byte_length: u64,
}

/// One mesh message carrying a frame, built and ready to put.
pub struct AMeshMessageCarryingAFramesPixels {
    pub message_bytes: Vec<u8>,
    /// How many of `message_bytes` describe the frame — what rides in the
    /// attachment, so the reading runtime can split the three parts again.
    pub description_bytes: u32,
}

/// One mesh message carrying a frame, as the reading runtime takes it apart.
pub struct AFramesPixelsOffTheMesh<'a> {
    pub description: AFramesPixelDescriptionOnTheMesh,
    pub bag_bytes: &'a [u8],
    pub pixel_bytes: &'a [u8],
}

impl AFramesPixelDescriptionOnTheMesh {
    /// This description on the wire.
    pub fn to_wire_bytes(self) -> Vec<u8> {
        let format_wire_name = self.pixel_format.wire_name().as_bytes();
        let mut wire_bytes = Vec::with_capacity(FORMAT_WIRE_NAME_OFFSET + format_wire_name.len());
        wire_bytes.extend_from_slice(&self.width.to_le_bytes());
        wire_bytes.extend_from_slice(&self.height.to_le_bytes());
        wire_bytes.extend_from_slice(&self.pixel_byte_length.to_le_bytes());
        // The engine's own vocabulary is short enough that this cannot
        // truncate; the cast is bounded by `PixelFormat::wire_name`, not by a
        // peer's bytes.
        wire_bytes.extend_from_slice(&(format_wire_name.len() as u16).to_le_bytes());
        wire_bytes.extend_from_slice(format_wire_name);
        wire_bytes
    }

    /// Read a description off the wire, or `None` when the bytes are not one.
    ///
    /// Every step is fallible rather than an index that cannot fail: these are
    /// peer-controlled bytes, and the read runs where a panic would take the
    /// ingress's writing thread with it.
    pub fn from_wire_bytes(wire_bytes: &[u8]) -> Option<Self> {
        let (width, rest) = wire_bytes.split_first_chunk::<4>()?;
        let (height, rest) = rest.split_first_chunk::<4>()?;
        let (pixel_byte_length, rest) = rest.split_first_chunk::<8>()?;
        let (format_wire_name_length, rest) = rest.split_first_chunk::<2>()?;
        let format_wire_name =
            rest.get(..usize::from(u16::from_le_bytes(*format_wire_name_length)))?;
        let pixel_format =
            PixelFormat::parse_wire_name(std::str::from_utf8(format_wire_name).ok()?).ok()?;
        Some(Self {
            pixel_format,
            width: u32::from_le_bytes(*width),
            height: u32::from_le_bytes(*height),
            pixel_byte_length: u64::from_le_bytes(*pixel_byte_length),
        })
    }
}

/// Build the message one frame crosses as, filling its pixel tail through
/// `read_the_frames_pixels_into`.
///
/// The tail is handed to the caller rather than taken from it, so a frame is
/// copied once — straight out of the sender's staging into the bytes that go
/// on the wire — rather than into an intermediate the message then copies
/// again. At 1080p RGBA that second copy would be 8.3 MB per frame.
pub fn a_mesh_message_carrying_a_frames_pixels(
    description: AFramesPixelDescriptionOnTheMesh,
    bag_bytes: &[u8],
    read_the_frames_pixels_into: impl FnOnce(&mut [u8]),
) -> AMeshMessageCarryingAFramesPixels {
    let described = description.to_wire_bytes();
    let pixel_byte_length = description.pixel_byte_length as usize;
    let mut message_bytes =
        Vec::with_capacity(described.len() + bag_bytes.len() + pixel_byte_length);
    message_bytes.extend_from_slice(&described);
    message_bytes.extend_from_slice(bag_bytes);
    let where_the_pixels_go = message_bytes.len()..message_bytes.len() + pixel_byte_length;
    message_bytes.resize(where_the_pixels_go.end, 0);
    read_the_frames_pixels_into(&mut message_bytes[where_the_pixels_go]);
    AMeshMessageCarryingAFramesPixels {
        message_bytes,
        // Bounded by the format vocabulary above, not by anything a peer sends.
        description_bytes: described.len() as u32,
    }
}

/// Take one arriving message apart, or `None` when its three parts do not fit
/// the bytes that arrived.
pub fn a_frames_pixels_off_the_mesh(
    payload: &[u8],
    description_bytes: u32,
) -> Option<AFramesPixelsOffTheMesh<'_>> {
    let described = payload.get(..description_bytes as usize)?;
    let description = AFramesPixelDescriptionOnTheMesh::from_wire_bytes(described)?;
    let bag_and_pixels = payload.get(description_bytes as usize..)?;
    let pixel_byte_length = usize::try_from(description.pixel_byte_length).ok()?;
    let bag_bytes = bag_and_pixels.get(..bag_and_pixels.len().checked_sub(pixel_byte_length)?)?;
    Some(AFramesPixelsOffTheMesh {
        description,
        bag_bytes,
        pixel_bytes: &bag_and_pixels[bag_bytes.len()..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_description() -> AFramesPixelDescriptionOnTheMesh {
        AFramesPixelDescriptionOnTheMesh {
            pixel_format: PixelFormat::Bgra32,
            width: 1920,
            height: 1080,
            pixel_byte_length: 1920 * 1080 * 4,
        }
    }

    /// The layout is the wire contract, pinned field by field: these are the
    /// bytes a peer reads, so a field that moved would be read as another
    /// field's value rather than as a decode failure.
    #[test]
    fn a_descriptions_bytes_are_every_field_little_endian_at_its_own_offset() {
        let wire_bytes = a_description().to_wire_bytes();
        assert_eq!(
            &wire_bytes[WIDTH_OFFSET..HEIGHT_OFFSET],
            &1920u32.to_le_bytes()
        );
        assert_eq!(
            &wire_bytes[HEIGHT_OFFSET..PIXEL_BYTE_LENGTH_OFFSET],
            &1080u32.to_le_bytes()
        );
        assert_eq!(
            &wire_bytes[PIXEL_BYTE_LENGTH_OFFSET..FORMAT_WIRE_NAME_LENGTH_OFFSET],
            &(1920u64 * 1080 * 4).to_le_bytes()
        );
        assert_eq!(
            &wire_bytes[FORMAT_WIRE_NAME_LENGTH_OFFSET..FORMAT_WIRE_NAME_OFFSET],
            &6u16.to_le_bytes()
        );
        assert_eq!(&wire_bytes[FORMAT_WIRE_NAME_OFFSET..], b"bgra32");
    }

    /// Every format a single-plane backing can carry survives the wire under
    /// its own name — the whole point of carrying the format at all is that
    /// BGRA does not arrive labelled RGBA.
    #[test]
    fn every_format_a_backing_can_carry_survives_the_wire_under_its_own_name() {
        for pixel_format in [
            PixelFormat::Rgba32,
            PixelFormat::Bgra32,
            PixelFormat::Argb32,
            PixelFormat::Rgba64,
            PixelFormat::Rgba16Float,
            PixelFormat::Rgba32Float,
            PixelFormat::Gray8,
            PixelFormat::Uyvy422,
            PixelFormat::Yuyv422,
        ] {
            let description = AFramesPixelDescriptionOnTheMesh {
                pixel_format,
                ..a_description()
            };
            assert_eq!(
                AFramesPixelDescriptionOnTheMesh::from_wire_bytes(&description.to_wire_bytes()),
                Some(description),
                "{pixel_format:?} must survive the wire"
            );
        }
    }

    /// Bytes that are not one description read as none rather than as one
    /// whose missing fields were invented.
    #[test]
    fn bytes_that_are_not_one_description_read_as_none() {
        let wire_bytes = a_description().to_wire_bytes();
        for too_short in 0..wire_bytes.len() {
            assert_eq!(
                AFramesPixelDescriptionOnTheMesh::from_wire_bytes(&wire_bytes[..too_short]),
                None,
                "{too_short} bytes must read as no description"
            );
        }
        let mut an_unknown_format = wire_bytes.clone();
        an_unknown_format.truncate(FORMAT_WIRE_NAME_OFFSET);
        an_unknown_format.extend_from_slice(b"chartreuse");
        an_unknown_format[FORMAT_WIRE_NAME_LENGTH_OFFSET..FORMAT_WIRE_NAME_OFFSET]
            .copy_from_slice(&10u16.to_le_bytes());
        assert_eq!(
            AFramesPixelDescriptionOnTheMesh::from_wire_bytes(&an_unknown_format),
            None,
            "a format this engine does not know must refuse rather than guess one"
        );
    }

    /// The three parts go out and come back as themselves, whatever their
    /// lengths — the split is by the description's length and the pixels', so
    /// a bag whose bytes look like either is still read as the bag.
    #[test]
    fn the_three_parts_of_a_message_survive_the_round_trip() {
        let description = AFramesPixelDescriptionOnTheMesh {
            pixel_format: PixelFormat::Rgba32,
            width: 4,
            height: 2,
            pixel_byte_length: 32,
        };
        let bag_bytes = b"\x82\xaasurface_id\xa33#1\xa5width\x04".as_slice();
        let pixels: Vec<u8> = (0..32u8).collect();

        let message = a_mesh_message_carrying_a_frames_pixels(description, bag_bytes, |tail| {
            tail.copy_from_slice(&pixels)
        });
        assert_eq!(
            message.message_bytes.len(),
            message.description_bytes as usize + bag_bytes.len() + pixels.len()
        );

        let read = a_frames_pixels_off_the_mesh(&message.message_bytes, message.description_bytes)
            .expect("the message this engine just built reads back");
        assert_eq!(read.description, description);
        assert_eq!(read.bag_bytes, bag_bytes);
        assert_eq!(read.pixel_bytes, pixels);
    }

    /// A frame with no pixels at all, and a bag of no bytes, both still split
    /// where they should: the two empty edges of the same arithmetic.
    #[test]
    fn a_message_with_an_empty_part_still_splits_where_it_should() {
        let description = AFramesPixelDescriptionOnTheMesh {
            pixel_format: PixelFormat::Gray8,
            width: 0,
            height: 0,
            pixel_byte_length: 0,
        };
        let message = a_mesh_message_carrying_a_frames_pixels(description, b"\x80", |tail| {
            assert!(tail.is_empty())
        });
        let read = a_frames_pixels_off_the_mesh(&message.message_bytes, message.description_bytes)
            .expect("a frame with no pixels still reads");
        assert_eq!(read.bag_bytes, b"\x80");
        assert!(read.pixel_bytes.is_empty());

        let empty_bag = a_mesh_message_carrying_a_frames_pixels(
            AFramesPixelDescriptionOnTheMesh {
                pixel_byte_length: 4,
                ..description
            },
            b"",
            |tail| tail.copy_from_slice(&[9, 9, 9, 9]),
        );
        let read =
            a_frames_pixels_off_the_mesh(&empty_bag.message_bytes, empty_bag.description_bytes)
                .expect("a message whose bag is empty still reads");
        assert!(read.bag_bytes.is_empty());
        assert_eq!(read.pixel_bytes, [9, 9, 9, 9]);
    }

    /// A message claiming more pixels than arrived reads as none rather than
    /// handing back a bag sliced out of the pixels, or panicking on the
    /// ingress's writing thread.
    #[test]
    fn a_message_claiming_more_than_arrived_reads_as_none() {
        let description = AFramesPixelDescriptionOnTheMesh {
            pixel_format: PixelFormat::Rgba32,
            width: 4,
            height: 2,
            pixel_byte_length: 32,
        };
        let message =
            a_mesh_message_carrying_a_frames_pixels(description, b"\x80", |tail| tail.fill(7));

        let mut truncated = message.message_bytes.clone();
        truncated.truncate(truncated.len() - 8);
        assert!(
            a_frames_pixels_off_the_mesh(&truncated, message.description_bytes).is_none(),
            "a message short of the pixels it claims must read as none"
        );
        assert!(
            a_frames_pixels_off_the_mesh(
                &message.message_bytes,
                message.message_bytes.len() as u32 + 1
            )
            .is_none(),
            "a description longer than the whole payload must read as none"
        );
        assert!(
            a_frames_pixels_off_the_mesh(b"", 0).is_none(),
            "no payload at all must read as none"
        );
    }
}
