// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JPEG decode for streamlib.
//!
//! - Baseline-sequential JPEG marker parser (SOI/DQT/DHT/SOF0/SOS/DRI/EOI,
//!   skips APPn/COM).
//! - Huffman entropy decoder that produces zig-zag-ordered DCT coefficient
//!   buffers, one per component.
//! - On Linux, a fused GPU compute kernel ([`kernel::JpegDecodeKernel`])
//!   that dequantizes, runs the 8x8 IDCT, upsamples 4:2:0 chroma, and
//!   converts BT.601 full-range YCbCr to RGB, writing the result to a
//!   caller-supplied `rgba8` storage image.

mod bit_reader;
mod error;
mod header;
mod huffman;
mod marker;
mod parser;
mod scan;

#[cfg(target_os = "linux")]
pub mod kernel;

pub use error::{JpegError, JpegResult};
pub use header::{
    ComponentScan, DecodedJpeg, FrameComponent, FrameHeader, QuantizationTable, ScanComponent,
    ScanHeader,
};
pub use huffman::HuffmanClass;
pub use marker::ZIGZAG;

#[cfg(target_os = "linux")]
pub use kernel::{JpegDecodeKernel, JPEG_DECODE_WORKGROUP_SIZE};

/// Parse and entropy-decode a baseline-sequential JPEG bitstream.
///
/// Returns a [`DecodedJpeg`] carrying the parsed headers and one
/// [`ComponentScan`] per component with the post-Huffman, pre-dequant
/// coefficient buffer in zig-zag order. Progressive, lossless, and
/// arithmetic-coded JPEGs are rejected with [`JpegError::UnsupportedSof`]
/// or [`JpegError::Unsupported`].
pub fn decode(bytes: &[u8]) -> JpegResult<DecodedJpeg> {
    let parser::ParsedHeaders {
        frame,
        quant_tables,
        huffman,
        scan,
        restart_interval,
        entropy_data,
    } = parser::parse_headers(bytes)?;

    // Sanity-check that every component's quant table is present.
    for component in &frame.components {
        if !quant_tables
            .iter()
            .any(|t| t.id == component.quant_table_id)
        {
            return Err(JpegError::MissingQuantizationTable {
                id: component.quant_table_id,
            });
        }
    }

    let components = scan::decode_entropy(
        &frame,
        &scan,
        &huffman,
        restart_interval,
        entropy_data,
    )?;

    Ok(DecodedJpeg {
        frame,
        quant_tables,
        scan,
        components,
        restart_interval,
    })
}
