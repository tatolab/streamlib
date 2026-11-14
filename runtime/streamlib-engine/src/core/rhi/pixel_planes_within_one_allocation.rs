// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Where a pixel format's planes sit inside one memory allocation, and
//! whether a device other than the GPU writes that memory.

use crate::core::rhi::{PixelFormat, SourceLayoutInfo};
use crate::core::{Error, Result};

/// Surface-share wire key for each plane's byte offset into the allocation.
const SURFACE_SHARE_PIXEL_PLANE_OFFSETS_FIELD: &str = "pixel_plane_offsets";

/// Surface-share wire key for each plane's row pitch in bytes.
const SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD: &str = "pixel_plane_strides";

/// Surface-share wire key for whether a device other than the GPU writes the
/// allocation.
const SURFACE_SHARE_WRITTEN_BY_A_FOREIGN_DEVICE_FIELD: &str = "written_by_a_foreign_device";

/// The plane layout of a pixel surface whose planes all live in one
/// allocation — a V4L2 single-planar buffer, an exporter's NV12 with padded
/// rows — validated against its format and extent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelPlanesWithinOneAllocation {
    plane_offsets_bytes: Vec<u64>,
    plane_strides_bytes: Vec<u64>,
    written_by_a_foreign_device: bool,
}

impl PixelPlanesWithinOneAllocation {
    /// Validate a stated layout: one offset and one stride per plane of
    /// `pixel_format`, each stride at least the plane's row of pixels, and
    /// every plane's rows inside `allocation_byte_size`.
    pub fn new(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        allocation_byte_size: u64,
        plane_offsets_bytes: Vec<u64>,
        plane_strides_bytes: Vec<u64>,
        written_by_a_foreign_device: bool,
    ) -> Result<Self> {
        let plane_count = pixel_format.plane_count() as usize;
        if plane_offsets_bytes.len() != plane_count || plane_strides_bytes.len() != plane_count {
            return Err(Error::Configuration(format!(
                "{} carries {plane_count} plane(s), but the layout states {} offset(s) and {} \
                 stride(s)",
                pixel_format.wire_name(),
                plane_offsets_bytes.len(),
                plane_strides_bytes.len()
            )));
        }
        for (plane_index, (&offset, &stride)) in plane_offsets_bytes
            .iter()
            .zip(&plane_strides_bytes)
            .enumerate()
        {
            let (row_bytes, row_count) =
                plane_row_bytes_and_row_count(pixel_format, width, height, plane_index)?;
            if stride < row_bytes {
                return Err(Error::Configuration(format!(
                    "plane {plane_index} of a {width}x{height} {} states a {stride}-byte row \
                     pitch, shorter than the {row_bytes} bytes one row of its pixels fills",
                    pixel_format.wire_name()
                )));
            }
            let plane_end = stride
                .checked_mul(u64::from(row_count - 1))
                .and_then(|rows_before_the_last| rows_before_the_last.checked_add(row_bytes))
                .and_then(|plane_extent| plane_extent.checked_add(offset));
            if plane_end.is_none_or(|plane_end| plane_end > allocation_byte_size) {
                return Err(Error::Configuration(format!(
                    "plane {plane_index} of a {width}x{height} {} at offset {offset} with a \
                     {stride}-byte pitch runs past the {allocation_byte_size}-byte allocation",
                    pixel_format.wire_name()
                )));
            }
        }
        Ok(Self {
            plane_offsets_bytes,
            plane_strides_bytes,
            written_by_a_foreign_device,
        })
    }

    /// The tight layout of `pixel_format` at `width` × `height`, written by the
    /// GPU or the CPU.
    pub fn tightly_packed(pixel_format: PixelFormat, width: u32, height: u32) -> Result<Self> {
        let mut plane_offsets_bytes = Vec::new();
        let mut plane_strides_bytes = Vec::new();
        let mut next_plane_offset = 0u64;
        for plane_index in 0..pixel_format.plane_count() as usize {
            let (row_bytes, row_count) =
                plane_row_bytes_and_row_count(pixel_format, width, height, plane_index)?;
            plane_offsets_bytes.push(next_plane_offset);
            plane_strides_bytes.push(row_bytes);
            next_plane_offset += row_bytes * u64::from(row_count);
        }
        Ok(Self {
            plane_offsets_bytes,
            plane_strides_bytes,
            written_by_a_foreign_device: false,
        })
    }

    /// The bytes the tight layout of `pixel_format` at `width` × `height` fills.
    pub fn tightly_packed_byte_size(
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
    ) -> Result<u64> {
        let mut byte_size = 0u64;
        for plane_index in 0..pixel_format.plane_count() as usize {
            let (row_bytes, row_count) =
                plane_row_bytes_and_row_count(pixel_format, width, height, plane_index)?;
            byte_size += row_bytes * u64::from(row_count);
        }
        Ok(byte_size)
    }

    /// The layout a surface-share registration or checkout states, or `None`
    /// when it states none; a partial or malformed statement is refused.
    pub fn from_surface_share_fields(fields: &serde_json::Value) -> Result<Option<Self>> {
        let offsets = fields.get(SURFACE_SHARE_PIXEL_PLANE_OFFSETS_FIELD);
        let strides = fields.get(SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD);
        let (offsets, strides) = match (offsets, strides) {
            (None, None) => return Ok(None),
            (Some(offsets), Some(strides)) => (offsets, strides),
            _ => {
                return Err(Error::Configuration(format!(
                    "a pixel plane layout states both {SURFACE_SHARE_PIXEL_PLANE_OFFSETS_FIELD} \
                     and {SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD}, or neither"
                )));
            }
        };
        let unsigned_integers = |field_name: &str, value: &serde_json::Value| {
            value
                .as_array()
                .and_then(|entries| {
                    entries
                        .iter()
                        .map(serde_json::Value::as_u64)
                        .collect::<Option<Vec<u64>>>()
                })
                .ok_or_else(|| {
                    Error::Configuration(format!(
                        "{field_name} is not an array of unsigned integers"
                    ))
                })
        };
        Ok(Some(Self {
            plane_offsets_bytes: unsigned_integers(
                SURFACE_SHARE_PIXEL_PLANE_OFFSETS_FIELD,
                offsets,
            )?,
            plane_strides_bytes: unsigned_integers(
                SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD,
                strides,
            )?,
            written_by_a_foreign_device: fields
                .get(SURFACE_SHARE_WRITTEN_BY_A_FOREIGN_DEVICE_FIELD)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }))
    }

    /// Write this layout onto a surface-share registration or lookup reply.
    pub fn write_surface_share_fields(&self, fields: &mut serde_json::Value) {
        fields[SURFACE_SHARE_PIXEL_PLANE_OFFSETS_FIELD] =
            self.plane_offsets_bytes.as_slice().into();
        fields[SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD] =
            self.plane_strides_bytes.as_slice().into();
        fields[SURFACE_SHARE_WRITTEN_BY_A_FOREIGN_DEVICE_FIELD] =
            self.written_by_a_foreign_device.into();
    }

    /// Each plane's byte offset into the allocation.
    pub fn plane_offsets_bytes(&self) -> &[u64] {
        &self.plane_offsets_bytes
    }

    /// Each plane's row pitch in bytes.
    pub fn plane_strides_bytes(&self) -> &[u64] {
        &self.plane_strides_bytes
    }

    /// Whether a device other than the GPU — a camera, a capture card —
    /// writes the allocation, so a GPU read must first make its writes
    /// available.
    pub fn written_by_a_foreign_device(&self) -> bool {
        self.written_by_a_foreign_device
    }

    /// The layout the engine's colour converter reads `pixel_format` through.
    pub fn color_converter_source_layout(
        &self,
        pixel_format: PixelFormat,
    ) -> Result<SourceLayoutInfo> {
        let narrowed = |what: &str, value: u64| {
            u32::try_from(value).map_err(|_| {
                Error::NotSupported(format!(
                    "the colour converter addresses its source with 32-bit offsets, and the \
                     {what} is {value} bytes"
                ))
            })
        };
        match pixel_format {
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange => {
                let luma_offset = self.plane_offsets_bytes[0];
                let chroma_offset = self.plane_offsets_bytes[1];
                let chroma_offset_from_luma =
                    chroma_offset.checked_sub(luma_offset).ok_or_else(|| {
                        Error::NotSupported(format!(
                            "the colour converter reads NV12 whose chroma plane follows its luma \
                         plane; this layout puts chroma at {chroma_offset} before luma at \
                         {luma_offset}"
                        ))
                    })?;
                Ok(SourceLayoutInfo::nv12_starting_at(
                    narrowed("luma plane offset", luma_offset)?,
                    narrowed("luma row pitch", self.plane_strides_bytes[0])?,
                    narrowed("chroma row pitch", self.plane_strides_bytes[1])?,
                    narrowed("chroma plane's distance from luma", chroma_offset_from_luma)?,
                ))
            }
            PixelFormat::Yuyv422 => {
                let packed_stride = self.plane_strides_bytes[0];
                if !packed_stride.is_multiple_of(4) {
                    return Err(Error::NotSupported(format!(
                        "the colour converter reads YUYV a 4-byte pixel pair at a time, and this \
                         layout's {packed_stride}-byte row pitch is not a multiple of 4"
                    )));
                }
                let mut layout = SourceLayoutInfo::yuyv(narrowed("row pitch", packed_stride)?);
                layout.plane0_offset_bytes = narrowed("plane offset", self.plane_offsets_bytes[0])?;
                Ok(layout)
            }
            other => Err(Error::NotSupported(format!(
                "the colour converter reads nv12_video_range, nv12_full_range and yuyv422 \
                 sources; {} is none of them",
                other.wire_name()
            ))),
        }
    }
}

/// The bytes one row of `plane_index`'s pixels fills, and how many rows the
/// plane holds.
fn plane_row_bytes_and_row_count(
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    plane_index: usize,
) -> Result<(u64, u32)> {
    if width == 0 || height == 0 {
        return Err(Error::Configuration(format!(
            "a {width}x{height} {} has no pixels to lay out",
            pixel_format.wire_name()
        )));
    }
    let width = u64::from(width);
    match (pixel_format, plane_index) {
        (PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange, 0) => Ok((width, height)),
        (PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange, 1) => {
            Ok((width.next_multiple_of(2), height.div_ceil(2)))
        }
        (PixelFormat::Unknown, _) => Err(Error::Configuration(
            "a surface of unknown format has no plane layout".to_string(),
        )),
        (_, 0) => Ok((
            width * u64::from(pixel_format.bits_per_pixel()).div_ceil(8),
            height,
        )),
        (_, beyond_the_last) => Err(Error::Configuration(format!(
            "{} has no plane {beyond_the_last}",
            pixel_format.wire_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn the_engine_and_the_surface_client_name_the_same_plane_strides_field() {
        assert_eq!(
            SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD,
            streamlib_surface_client::SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD
        );
    }

    const V4L2_NV12_1080P_BYTES_PER_LINE: u64 = 1920;
    const V4L2_NV12_1080P_SIZE_IMAGE: u64 = 1920 * 1080 * 3 / 2;

    fn v4l2_nv12_1080p() -> PixelPlanesWithinOneAllocation {
        PixelPlanesWithinOneAllocation::new(
            PixelFormat::Nv12VideoRange,
            1920,
            1080,
            V4L2_NV12_1080P_SIZE_IMAGE,
            vec![0, V4L2_NV12_1080P_BYTES_PER_LINE * 1080],
            vec![V4L2_NV12_1080P_BYTES_PER_LINE; 2],
            true,
        )
        .expect("a V4L2 single-planar NV12 layout is valid")
    }

    #[test]
    fn a_v4l2_nv12_buffer_reads_as_the_converters_nv12_layout() {
        let layout = v4l2_nv12_1080p()
            .color_converter_source_layout(PixelFormat::Nv12VideoRange)
            .expect("NV12 converts");
        assert_eq!(layout.plane0_offset_bytes, 0);
        assert_eq!(layout.plane0_stride_bytes, 1920);
        assert_eq!(layout.plane1_stride_bytes, 1920);
        assert_eq!(layout.plane1_offset_bytes, 1920 * 1080);
    }

    #[test]
    fn a_padded_yuyv_row_pitch_reaches_the_converter() {
        let layout = PixelPlanesWithinOneAllocation::new(
            PixelFormat::Yuyv422,
            1280,
            720,
            3072 * 720,
            vec![0],
            vec![3072],
            true,
        )
        .expect("a padded YUYV layout is valid")
        .color_converter_source_layout(PixelFormat::Yuyv422)
        .expect("YUYV converts");
        assert_eq!(layout.plane0_stride_bytes, 3072);
    }

    #[test]
    fn a_plane_count_other_than_the_formats_is_refused() {
        let refusal = PixelPlanesWithinOneAllocation::new(
            PixelFormat::Nv12VideoRange,
            64,
            64,
            64 * 96,
            vec![0],
            vec![64],
            false,
        )
        .expect_err("NV12 has two planes");
        assert!(refusal.to_string().contains("2 plane(s)"), "{refusal}");
    }

    #[test]
    fn a_row_pitch_shorter_than_a_row_is_refused() {
        let refusal = PixelPlanesWithinOneAllocation::new(
            PixelFormat::Yuyv422,
            64,
            64,
            64 * 2 * 64,
            vec![0],
            vec![64],
            false,
        )
        .expect_err("a YUYV row of 64 pixels fills 128 bytes");
        assert!(refusal.to_string().contains("128 bytes"), "{refusal}");
    }

    #[test]
    fn a_plane_past_the_allocation_is_refused() {
        let refusal = PixelPlanesWithinOneAllocation::new(
            PixelFormat::Nv12VideoRange,
            64,
            64,
            64 * 64,
            vec![0, 64 * 64],
            vec![64, 64],
            false,
        )
        .expect_err("the chroma plane starts where the allocation ends");
        assert!(refusal.to_string().contains("runs past"), "{refusal}");
    }

    #[test]
    fn the_last_row_need_not_carry_its_padding() {
        PixelPlanesWithinOneAllocation::new(
            PixelFormat::Rgba32,
            4,
            2,
            64 + 16,
            vec![0],
            vec![64],
            false,
        )
        .expect("the last row ends at its pixels, not its pitch");
    }

    #[test]
    fn a_layout_round_trips_through_the_surface_share_wire() {
        let layout = v4l2_nv12_1080p();
        let mut fields = serde_json::json!({});
        layout.write_surface_share_fields(&mut fields);
        assert_eq!(
            PixelPlanesWithinOneAllocation::from_surface_share_fields(&fields)
                .expect("a written layout parses"),
            Some(layout)
        );
    }

    #[test]
    fn a_registration_without_a_layout_states_none() {
        assert_eq!(
            PixelPlanesWithinOneAllocation::from_surface_share_fields(&serde_json::json!({
                "plane_offsets": [0],
            }))
            .expect("no layout is not malformed"),
            None
        );
    }

    #[test]
    fn a_layout_with_offsets_and_no_strides_is_refused() {
        PixelPlanesWithinOneAllocation::from_surface_share_fields(&serde_json::json!({
            "pixel_plane_offsets": [0, 4096],
        }))
        .expect_err("half a layout is malformed");
    }

    #[test]
    fn tight_nv12_puts_chroma_after_every_luma_row() {
        let tight =
            PixelPlanesWithinOneAllocation::tightly_packed(PixelFormat::Nv12FullRange, 6, 4)
                .expect("tight NV12 lays out");
        assert_eq!(tight.plane_offsets_bytes(), &[0, 24]);
        assert_eq!(tight.plane_strides_bytes(), &[6, 6]);
        assert!(!tight.written_by_a_foreign_device());
    }

    #[test]
    fn tight_nv12_fills_one_and_a_half_bytes_a_pixel() {
        assert_eq!(
            PixelPlanesWithinOneAllocation::tightly_packed_byte_size(
                PixelFormat::Nv12FullRange,
                6,
                4
            )
            .expect("tight NV12 sizes"),
            36
        );
    }

    #[test]
    fn an_rgba_source_is_refused_by_the_converter_by_name() {
        let refusal = PixelPlanesWithinOneAllocation::tightly_packed(PixelFormat::Rgba32, 4, 4)
            .expect("tight RGBA lays out")
            .color_converter_source_layout(PixelFormat::Rgba32)
            .expect_err("the converter reads YCbCr sources");
        assert!(refusal.to_string().contains("rgba32"), "{refusal}");
    }
}
