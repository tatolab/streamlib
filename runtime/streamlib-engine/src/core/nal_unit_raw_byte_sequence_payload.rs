// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A NAL unit's raw byte sequence payload (ITU-T H.264 §7.3.1, H.265 §7.3.1.1):
//! the emulation-prevention bytes stripped, and the `u(n)` / `ue(v)` / `se(v)`
//! reads both specs define over it.

// ---------------------------------------------------------------------------
// Emulation prevention byte removal (RBSP extraction)
// ---------------------------------------------------------------------------

/// Remove emulation prevention bytes (`0x00 0x00 0x03`) from a raw NAL unit
/// payload, producing the Raw Byte Sequence Payload (RBSP).
///
/// In H.264/H.265 Annex-B byte streams, the byte sequence `0x00 0x00 0x03`
/// inside a NAL unit is an *emulation prevention* mechanism: the `0x03` byte
/// is not part of the coded data and must be stripped before further parsing.
///
/// This is not a direct port of a single C++ function (the C++ code handles
/// this inline inside the bit-reader), but encapsulates the same logic for
/// convenience and testability.
pub fn remove_emulation_prevention_bytes(nalu: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(nalu.len());
    let mut i = 0;
    while i < nalu.len() {
        if i + 2 < nalu.len() && nalu[i] == 0x00 && nalu[i + 1] == 0x00 && nalu[i + 2] == 0x03 {
            rbsp.push(0x00);
            rbsp.push(0x00);
            i += 3; // skip the 0x03 emulation prevention byte
        } else {
            rbsp.push(nalu[i]);
            i += 1;
        }
    }
    rbsp
}

// ---------------------------------------------------------------------------
// RbspBitstreamReader — minimal bitstream reading abstraction
// ---------------------------------------------------------------------------

/// Minimal bitstream reader for parsing NAL unit data.
///
/// Divergence from C++: The C++ code uses methods inherited from VulkanVideoDecoder
/// (`u()`, `ue()`, `se()`, etc.). We provide an equivalent standalone struct.
pub struct RbspBitstreamReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> RbspBitstreamReader<'a> {
    /// Create a new reader from a byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    /// Read `n` bits as a `u32`. Returns `None` if not enough bits available.
    pub fn u(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 {
            return None;
        }
        let n = n as usize;
        if self.bit_offset + n > self.data.len() * 8 {
            return None;
        }
        let mut val = 0u32;
        for _ in 0..n {
            let byte_idx = self.bit_offset / 8;
            let bit_idx = 7 - (self.bit_offset % 8);
            val = (val << 1) | ((self.data[byte_idx] >> bit_idx) as u32 & 1);
            self.bit_offset += 1;
        }
        Some(val)
    }

    /// Read an unsigned Exp-Golomb coded value (ue(v)).
    pub fn ue(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0u32;
        loop {
            let bit = self.u(1)?;
            if bit != 0 {
                break;
            }
            leading_zero_bits += 1;
            if leading_zero_bits > 31 {
                return None;
            }
        }
        if leading_zero_bits == 0 {
            return Some(0);
        }
        let suffix = self.u(leading_zero_bits)?;
        Some((1 << leading_zero_bits) - 1 + suffix)
    }

    /// Read a signed Exp-Golomb coded value (se(v)).
    pub fn se(&mut self) -> Option<i32> {
        let code_num = self.ue()?;
        let value = ((code_num + 1) >> 1) as i32;
        if code_num & 1 == 0 {
            Some(-value)
        } else {
            Some(value)
        }
    }

    /// Return the number of bits consumed so far.
    pub fn consumed_bits(&self) -> usize {
        self.bit_offset
    }

    /// Return the number of bits still available.
    pub fn available_bits(&self) -> usize {
        self.data.len() * 8 - self.bit_offset
    }

    /// Peek at the next `n` bits without consuming them.
    pub fn next_bits(&self, n: u32) -> Option<u32> {
        if n == 0 || n > 32 {
            return None;
        }
        let n = n as usize;
        if self.bit_offset + n > self.data.len() * 8 {
            return None;
        }
        let mut val = 0u32;
        for k in 0..n {
            let byte_idx = (self.bit_offset + k) / 8;
            let bit_idx = 7 - ((self.bit_offset + k) % 8);
            val = (val << 1) | ((self.data[byte_idx] >> bit_idx) as u32 & 1);
        }
        Some(val)
    }

    /// Skip `n` bits.
    pub fn skip_bits(&mut self, n: usize) {
        self.bit_offset += n;
    }

    /// Check if current position is byte-aligned.
    pub fn byte_aligned(&self) -> bool {
        self.bit_offset % 8 == 0
    }

    /// Read a fixed-pattern of `n` bits and verify it matches `expected`.
    pub fn f(&mut self, n: u32, expected: u32) -> Option<u32> {
        let val = self.u(n)?;
        if val != expected {
            tracing::warn!(
                "Fixed pattern mismatch: expected 0x{:x}, got 0x{:x}",
                expected,
                val
            );
        }
        Some(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Emulation prevention byte removal
    // -----------------------------------------------------------------------

    #[test]
    fn epb_removal_basic() {
        // 00 00 03 should strip the 03
        let input = [0x00, 0x00, 0x03, 0x01];
        let rbsp = remove_emulation_prevention_bytes(&input);
        assert_eq!(rbsp, vec![0x00, 0x00, 0x01]);
    }

    #[test]
    fn epb_removal_multiple() {
        let input = [0xAA, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0xBB];
        let rbsp = remove_emulation_prevention_bytes(&input);
        assert_eq!(rbsp, vec![0xAA, 0x00, 0x00, 0x00, 0x00, 0xBB]);
    }

    #[test]
    fn epb_removal_none_needed() {
        let input = [0xAA, 0xBB, 0xCC];
        let rbsp = remove_emulation_prevention_bytes(&input);
        assert_eq!(rbsp, input.to_vec());
    }

    #[test]
    fn epb_removal_empty() {
        let rbsp = remove_emulation_prevention_bytes(&[]);
        assert!(rbsp.is_empty());
    }

    #[test]
    fn epb_at_end() {
        // Trailing 00 00 03 with nothing after — still stripped.
        let input = [0xFF, 0x00, 0x00, 0x03];
        let rbsp = remove_emulation_prevention_bytes(&input);
        assert_eq!(rbsp, vec![0xFF, 0x00, 0x00]);
    }

    #[test]
    fn epb_not_confused_by_00_00_04() {
        // 00 00 04 is NOT an emulation prevention sequence.
        let input = [0x00, 0x00, 0x04];
        let rbsp = remove_emulation_prevention_bytes(&input);
        assert_eq!(rbsp, input.to_vec());
    }
    // -----------------------------------------------------------------------
    // RbspBitstreamReader tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_bitstream_reader_u() {
        // 0b10110001 = 0xB1
        let data = [0xB1];
        let mut reader = RbspBitstreamReader::new(&data);
        assert_eq!(reader.u(1), Some(1)); // '1'
        assert_eq!(reader.u(3), Some(0b011)); // '011'
        assert_eq!(reader.u(4), Some(0b0001)); // '0001'
    }

    #[test]
    fn test_bitstream_reader_ue() {
        // ue(0) = '1' -> 0
        // ue(1) = '010' -> 1
        // ue(2) = '011' -> 2
        // ue(3) = '00100' -> 3
        // Concatenated: 1|010|011|00100 = 1010011 00100... pad
        // = 0b10100110 0100_0000 = 0xA6 0x40
        let data = [0xA6, 0x40];
        let mut reader = RbspBitstreamReader::new(&data);
        assert_eq!(reader.ue(), Some(0));
        assert_eq!(reader.ue(), Some(1));
        assert_eq!(reader.ue(), Some(2));
        assert_eq!(reader.ue(), Some(3));
    }

    #[test]
    fn test_bitstream_reader_se() {
        // se(v): code_num 0 -> 0, 1 -> 1, 2 -> -1, 3 -> 2, 4 -> -2
        // ue: 0='1', 1='010', 2='011', 3='00100', 4='00101'
        // Concatenated: 1|010|011|00100|00101 = 10100110 01000010 1...
        // = 0xA6 0x42 0x80
        let data = [0xA6, 0x42, 0x80];
        let mut reader = RbspBitstreamReader::new(&data);
        assert_eq!(reader.se(), Some(0));
        assert_eq!(reader.se(), Some(1));
        assert_eq!(reader.se(), Some(-1));
        assert_eq!(reader.se(), Some(2));
        assert_eq!(reader.se(), Some(-2));
    }
}
