// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::huffman::HuffmanTable;

/// Parsed SOF0 frame header.
#[derive(Debug, Clone)]
pub struct FrameHeader {
    /// Sample precision in bits (baseline JPEG: 8).
    pub precision: u8,
    pub height: u16,
    pub width: u16,
    pub components: Vec<FrameComponent>,
    pub max_h_sampling: u8,
    pub max_v_sampling: u8,
}

#[derive(Debug, Clone)]
pub struct FrameComponent {
    pub id: u8,
    pub h_sampling: u8,
    pub v_sampling: u8,
    pub quant_table_id: u8,
}

/// One quantization table parsed from a DQT segment.
#[derive(Debug, Clone)]
pub struct QuantizationTable {
    pub id: u8,
    /// 0 = 8-bit precision, 1 = 16-bit precision.
    pub precision: u8,
    /// 64 values in zig-zag scan order, exactly as stored in the JPEG
    /// bitstream. The GPU kernel applies these during dequantization.
    pub values: [u16; 64],
}

/// One scan-component entry parsed from the SOS segment.
#[derive(Debug, Clone)]
pub struct ScanComponent {
    pub component_id: u8,
    pub dc_table_id: u8,
    pub ac_table_id: u8,
}

/// Parsed SOS header. Baseline JPEG: `spectral_start = 0`,
/// `spectral_end = 63`, `successive_high = 0`, `successive_low = 0`.
#[derive(Debug, Clone)]
pub struct ScanHeader {
    pub components: Vec<ScanComponent>,
    pub spectral_start: u8,
    pub spectral_end: u8,
    pub successive_high: u8,
    pub successive_low: u8,
}

/// Per-component decoded coefficient plane.
#[derive(Debug, Clone)]
pub struct ComponentScan {
    pub component_id: u8,
    pub h_sampling: u8,
    pub v_sampling: u8,
    pub quant_table_id: u8,
    /// Number of 8x8 blocks horizontally (already widened to cover the
    /// MCU grid: `mcus_horizontal * h_sampling`).
    pub blocks_horizontal: usize,
    /// Number of 8x8 blocks vertically (already widened to cover the
    /// MCU grid: `mcus_vertical * v_sampling`).
    pub blocks_vertical: usize,
    /// Length = `blocks_horizontal * blocks_vertical * 64`. Indexed as
    /// `[(y * blocks_horizontal + x) * 64 + zz]`, where `zz` is the
    /// zig-zag-ordered coefficient index (0..64). Values are post-Huffman,
    /// pre-dequant signed coefficients.
    pub coefficients: Vec<i16>,
}

impl ComponentScan {
    /// Returns the 64-element zig-zag-ordered coefficient slice for the
    /// block at `(block_x, block_y)`.
    pub fn block(&self, block_x: usize, block_y: usize) -> &[i16] {
        let start = (block_y * self.blocks_horizontal + block_x) * 64;
        &self.coefficients[start..start + 64]
    }
}

/// Top-level decoded JPEG — parsed headers plus the per-component
/// coefficient buffers ready for a GPU kernel to consume.
#[derive(Debug, Clone)]
pub struct DecodedJpeg {
    pub frame: FrameHeader,
    pub quant_tables: Vec<QuantizationTable>,
    pub scan: ScanHeader,
    pub components: Vec<ComponentScan>,
    pub restart_interval: u16,
}

impl DecodedJpeg {
    /// Look up the quantization table by id.
    pub fn quantization_table(&self, id: u8) -> Option<&QuantizationTable> {
        self.quant_tables.iter().find(|t| t.id == id)
    }
}

/// Internal bag of Huffman tables indexed by class + id.
#[derive(Default)]
pub(crate) struct HuffmanTables {
    pub dc: [Option<HuffmanTable>; 4],
    pub ac: [Option<HuffmanTable>; 4],
}
