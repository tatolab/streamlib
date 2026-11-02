// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CPU half of `surface_write_from_host_memory`: a module's four-byte
//! pixel rows copied into the engine's staging rows, channel order swapped
//! where the two formats differ.

use streamlib::sdk::rhi::PixelFormat;

/// Rows of four-byte pixels in one block of memory.
pub(super) struct FourBytePixelRows<'memory> {
    pub(super) bytes: &'memory [u8],
    pub(super) format: PixelFormat,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) row_stride_bytes: u64,
}

/// How a source pixel's bytes become a destination pixel's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FourBytePixelChannelOrder {
    Kept,
    RedAndBlueSwapped,
}

fn channel_order_between(
    source: PixelFormat,
    destination: PixelFormat,
) -> Result<FourBytePixelChannelOrder, String> {
    match (source, destination) {
        (PixelFormat::Bgra32, PixelFormat::Bgra32) | (PixelFormat::Rgba32, PixelFormat::Rgba32) => {
            Ok(FourBytePixelChannelOrder::Kept)
        }
        (PixelFormat::Bgra32, PixelFormat::Rgba32) | (PixelFormat::Rgba32, PixelFormat::Bgra32) => {
            Ok(FourBytePixelChannelOrder::RedAndBlueSwapped)
        }
        (PixelFormat::Bgra32 | PixelFormat::Rgba32, other) => Err(format!(
            "the destination holds {} pixels; a host-memory write fills a texture whose \
             pixels are bgra32 or rgba32",
            other.wire_name()
        )),
        (other, _) => Err(format!(
            "host-memory pixels are bgra32 or rgba32, not {}",
            other.wire_name()
        )),
    }
}

impl FourBytePixelRows<'_> {
    fn refuse_rows_past_the_memory(&self, named: &str) -> Result<(), String> {
        let row_bytes = u64::from(self.width) * 4;
        if self.row_stride_bytes < row_bytes {
            return Err(format!(
                "the {named} rows are {} bytes apart, short of one row of {} four-byte pixels",
                self.row_stride_bytes, self.width
            ));
        }
        let spanned = if self.height == 0 {
            0
        } else {
            self.row_stride_bytes
                .checked_mul(u64::from(self.height) - 1)
                .and_then(|before_the_last_row| before_the_last_row.checked_add(row_bytes))
                .ok_or_else(|| format!("the {named} rows overflow the address space"))?
        };
        if (self.bytes.len() as u64) < spanned {
            return Err(format!(
                "the {named} rows span {spanned} bytes; the {named} memory holds {}",
                self.bytes.len()
            ));
        }
        Ok(())
    }
}

/// Copy every pixel of `source` into `destination`, which states the same
/// extent and is `destination_bytes` long.
pub(super) fn copy_four_byte_pixel_rows(
    source: &FourBytePixelRows<'_>,
    destination_format: PixelFormat,
    destination_row_stride_bytes: u64,
    destination_width: u32,
    destination_height: u32,
    destination_bytes: &mut [u8],
) -> Result<(), String> {
    if (source.width, source.height) != (destination_width, destination_height) {
        return Err(format!(
            "the pixels are {}x{} and the destination is {destination_width}x\
             {destination_height}; a host-memory write scales nothing",
            source.width, source.height
        ));
    }
    let channel_order = channel_order_between(source.format, destination_format)?;
    source.refuse_rows_past_the_memory("source")?;
    FourBytePixelRows {
        bytes: destination_bytes,
        format: destination_format,
        width: destination_width,
        height: destination_height,
        row_stride_bytes: destination_row_stride_bytes,
    }
    .refuse_rows_past_the_memory("destination")?;

    let row_bytes = source.width as usize * 4;
    for row in 0..source.height as usize {
        let source_start = row * source.row_stride_bytes as usize;
        let destination_start = row * destination_row_stride_bytes as usize;
        let source_row = &source.bytes[source_start..source_start + row_bytes];
        let destination_row =
            &mut destination_bytes[destination_start..destination_start + row_bytes];
        match channel_order {
            FourBytePixelChannelOrder::Kept => destination_row.copy_from_slice(source_row),
            FourBytePixelChannelOrder::RedAndBlueSwapped => {
                for (source_pixel, destination_pixel) in source_row
                    .chunks_exact(4)
                    .zip(destination_row.chunks_exact_mut(4))
                {
                    destination_pixel[0] = source_pixel[2];
                    destination_pixel[1] = source_pixel[1];
                    destination_pixel[2] = source_pixel[0];
                    destination_pixel[3] = source_pixel[3];
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(
        bytes: &[u8],
        format: PixelFormat,
        width: u32,
        height: u32,
        stride: u64,
    ) -> FourBytePixelRows<'_> {
        FourBytePixelRows {
            bytes,
            format,
            width,
            height,
            row_stride_bytes: stride,
        }
    }

    #[test]
    fn padded_bgra_rows_land_tightly_packed_as_rgba() {
        let source = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xee, 0xee, //
            9, 10, 11, 12, 13, 14, 15, 16,
        ];
        let mut destination = [0u8; 16];
        copy_four_byte_pixel_rows(
            &rows(&source, PixelFormat::Bgra32, 2, 2, 10),
            PixelFormat::Rgba32,
            8,
            2,
            2,
            &mut destination,
        )
        .expect("the copy is taken");
        assert_eq!(
            destination,
            [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
    }

    #[test]
    fn matching_formats_copy_the_bytes_unchanged() {
        let source = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut destination = [0u8; 12];
        copy_four_byte_pixel_rows(
            &rows(&source, PixelFormat::Rgba32, 1, 2, 4),
            PixelFormat::Rgba32,
            8,
            1,
            2,
            &mut destination,
        )
        .expect("the copy is taken");
        assert_eq!(destination, [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8]);
    }

    #[test]
    fn a_source_short_of_its_rows_is_refused_before_any_byte_moves() {
        let source = [7u8; 15];
        let mut destination = [0u8; 16];
        let refusal = copy_four_byte_pixel_rows(
            &rows(&source, PixelFormat::Bgra32, 2, 2, 8),
            PixelFormat::Rgba32,
            8,
            2,
            2,
            &mut destination,
        )
        .expect_err("15 bytes cannot hold two 8-byte rows");
        assert!(refusal.contains("span 16 bytes"), "{refusal}");
        assert_eq!(destination, [0u8; 16]);
    }

    #[test]
    fn another_extent_another_format_and_a_short_stride_are_each_refused_by_name() {
        let source = [0u8; 64];
        let mut destination = [0u8; 64];
        let extent = copy_four_byte_pixel_rows(
            &rows(&source, PixelFormat::Bgra32, 4, 4, 16),
            PixelFormat::Rgba32,
            16,
            2,
            2,
            &mut destination,
        )
        .expect_err("another extent");
        assert!(extent.contains("scales nothing"), "{extent}");
        let format = copy_four_byte_pixel_rows(
            &rows(&source, PixelFormat::Nv12FullRange, 4, 4, 16),
            PixelFormat::Rgba32,
            16,
            4,
            4,
            &mut destination,
        )
        .expect_err("another format");
        assert!(format.contains("nv12_full_range"), "{format}");
        let stride = copy_four_byte_pixel_rows(
            &rows(&source, PixelFormat::Bgra32, 4, 4, 12),
            PixelFormat::Rgba32,
            16,
            4,
            4,
            &mut destination,
        )
        .expect_err("a stride short of a row");
        assert!(stride.contains("12 bytes apart"), "{stride}");
    }
}
