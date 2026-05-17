// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::bit_reader::BitReader;
use crate::error::{JpegError, JpegResult};

/// Huffman table class — DC tables encode coefficient categories for the
/// 0th coefficient (delta-coded); AC tables encode `(zero-run, category)`
/// pairs for the 1st..63rd coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuffmanClass {
    Dc,
    Ac,
}

impl HuffmanClass {
    pub fn from_class_byte(value: u8) -> JpegResult<Self> {
        match value {
            0 => Ok(HuffmanClass::Dc),
            1 => Ok(HuffmanClass::Ac),
            _ => Err(JpegError::MalformedHuffmanTable("class must be 0 (DC) or 1 (AC)")),
        }
    }
}

/// Canonical Huffman decode table built per ITU-T T.81 Annex C.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `class` / `id` retained for diagnostics
pub struct HuffmanTable {
    pub class: HuffmanClass,
    pub id: u8,
    /// `min_code[n]` is the smallest n+1-bit code, or `i32::MAX` if no
    /// codes exist at that length.
    min_code: [i32; 16],
    /// `max_code[n]` is the largest n+1-bit code, or `-1` if no codes
    /// exist at that length.
    max_code: [i32; 16],
    /// `val_offset[n]` is the index into `huffval` for the first symbol
    /// of length n+1.
    val_offset: [usize; 16],
    /// Symbols (HUFFVAL) in canonical order.
    huffval: Vec<u8>,
}

impl HuffmanTable {
    /// Build a canonical Huffman table from BITS (16 entries, count of
    /// codes of each length 1..=16) and HUFFVAL (the symbol stream).
    ///
    /// Implements the recipe from T.81 Figure C.1 / C.2.
    pub fn build(class: HuffmanClass, id: u8, bits: &[u8; 16], huffval: &[u8]) -> JpegResult<Self> {
        let total: usize = bits.iter().map(|&b| b as usize).sum();
        if total > 256 {
            return Err(JpegError::MalformedHuffmanTable("BITS sum exceeds 256"));
        }
        if total != huffval.len() {
            return Err(JpegError::MalformedHuffmanTable(
                "BITS sum disagrees with HUFFVAL length",
            ));
        }

        let mut min_code = [i32::MAX; 16];
        let mut max_code = [-1i32; 16];
        let mut val_offset = [0usize; 16];

        let mut code: i32 = 0;
        let mut offset = 0usize;
        for n in 0..16 {
            let count = bits[n] as i32;
            if count == 0 {
                code <<= 1;
                continue;
            }
            // Codes of this length run [code, code + count - 1].
            if code + count > (1i32 << (n + 1)) {
                return Err(JpegError::MalformedHuffmanTable(
                    "BITS overflows the Huffman code space",
                ));
            }
            min_code[n] = code;
            max_code[n] = code + count - 1;
            val_offset[n] = offset;
            offset += count as usize;
            code = (code + count) << 1;
        }

        Ok(Self {
            class,
            id,
            min_code,
            max_code,
            val_offset,
            huffval: huffval.to_vec(),
        })
    }

    /// Decode one Huffman code from `reader` and return the symbol.
    pub fn decode_symbol(&self, reader: &mut BitReader<'_>) -> JpegResult<u8> {
        let mut code: i32 = 0;
        for n in 0..16 {
            code = (code << 1) | (reader.read_bit()? as i32);
            if code <= self.max_code[n] {
                let idx = self.val_offset[n] + (code - self.min_code[n]) as usize;
                if idx >= self.huffval.len() {
                    return Err(JpegError::InvalidHuffmanCode {
                        offset: reader.byte_offset(),
                    });
                }
                return Ok(self.huffval[idx]);
            }
        }
        Err(JpegError::InvalidHuffmanCode {
            offset: reader.byte_offset(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard JPEG luminance DC Huffman table from Annex K Table K.3.
    /// Used by every libjpeg-style encoder including jpeg-encoder.
    pub(crate) fn k3_luminance_dc() -> HuffmanTable {
        let bits = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let huffval = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        HuffmanTable::build(HuffmanClass::Dc, 0, &bits, &huffval).unwrap()
    }

    #[test]
    fn builds_canonical_table() {
        let table = k3_luminance_dc();
        // The shortest code is two bits (one entry); the longest is nine
        // bits (one entry). Spot-check both ends.
        assert_eq!(table.max_code[1], 0b00); // first 2-bit code is 0b00
        assert_eq!(table.max_code[8] - table.min_code[8], 0); // single 9-bit code
    }

    #[test]
    fn round_trip_decode() {
        // T.81 Figure C.2 says symbol 0 corresponds to the 2-bit code 0b00.
        // Encode + decode that exact bit pattern.
        let bytes: [u8; 1] = [0b00_00_00_00];
        let mut reader = BitReader::new(&bytes);
        let table = k3_luminance_dc();
        let sym = table.decode_symbol(&mut reader).unwrap();
        assert_eq!(sym, 0);
    }

    #[test]
    fn rejects_overflow() {
        // BITS sum > HUFFVAL length.
        let bits = [0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let huffval = [0u8; 1];
        let err = HuffmanTable::build(HuffmanClass::Dc, 0, &bits, &huffval).unwrap_err();
        assert!(matches!(err, JpegError::MalformedHuffmanTable(_)));
    }

    #[test]
    fn rejects_code_space_overflow() {
        // 3 codes of length 1 — only 2 possible 1-bit codes (0 and 1).
        let bits = [3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let huffval = [0u8, 1, 2];
        let err = HuffmanTable::build(HuffmanClass::Dc, 0, &bits, &huffval).unwrap_err();
        assert!(matches!(err, JpegError::MalformedHuffmanTable(_)));
    }
}
