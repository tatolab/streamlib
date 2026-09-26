// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Annex-B start-code scan: the three-byte prefix `0x00 0x00 0x01` that
//! opens every NAL unit of an H.264 / H.265 byte stream.
//!
//! A port of the scalar fallback of NVIDIA's `NvVideoParser`
//! (`NextStartCodeC.cpp`), kept platform-free so every walk of the seam's
//! Annex-B wire shares it: the Vulkan Video parser, the VideoToolbox arm and
//! the MP4 muxer.

/// Result returned by [`StartCodeFinder::next_start_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartCodeSearchResult {
    /// Number of bytes consumed from `data` (always >= 1 when `data` is
    /// non-empty).  When `found` is true this is the offset of the byte
    /// *after* the `0x01` of the start code prefix.
    pub bytes_consumed: usize,
    /// Whether a start code (`0x00 0x00 0x01`) was found.
    pub found: bool,
}

/// Persistent state for start code scanning, corresponding to `m_BitBfr` in
/// the C++ `VulkanVideoDecoder`.
///
/// The C++ code initialises `m_BitBfr` to `~0u` (all ones) so that no
/// accidental start code is detected at the very beginning.
#[derive(Debug, Clone)]
pub struct StartCodeFinder {
    /// Rolling bit buffer — the lower 24 bits are checked against `0x000001`.
    /// Corresponds to `VulkanVideoDecoder::m_BitBfr`.
    bit_bfr: u32,
}

impl Default for StartCodeFinder {
    /// Matches the C++ initialization: `m_BitBfr = (uint32_t)~0`.
    fn default() -> Self {
        Self { bit_bfr: !0u32 }
    }
}

impl StartCodeFinder {
    /// Create a new finder with the default (all-ones) shift register, matching
    /// the C++ initializer `m_BitBfr = (uint32_t)~0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the shift register to all-ones.  Called by the C++ code on
    /// `Initialize()` and `end_of_stream()`.
    pub fn reset(&mut self) {
        self.bit_bfr = !0u32;
    }

    /// Scan `data` for the next Annex-B start code prefix (`0x00 0x00 0x01`).
    ///
    /// This is a faithful port of:
    /// ```cpp
    /// template<>
    /// size_t VulkanVideoDecoder::next_start_code<SIMD_ISA::NOSIMD>(
    ///     const uint8_t *pdatain, size_t datasize, bool& found_start_code);
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `data` is empty — the C++ code enters a `do { } while`
    /// that always reads at least one byte.
    pub fn next_start_code(&mut self, data: &[u8]) -> StartCodeSearchResult {
        assert!(!data.is_empty(), "next_start_code called with empty data");

        let mut bfr = self.bit_bfr;
        let mut i: usize = 0;
        loop {
            bfr = (bfr << 8) | (data[i] as u32);
            i += 1;
            if (bfr & 0x00ff_ffff) == 1 {
                break;
            }
            if i >= data.len() {
                break;
            }
        }
        self.bit_bfr = bfr;
        let found = (bfr & 0x00ff_ffff) == 1;
        StartCodeSearchResult {
            bytes_consumed: i,
            found,
        }
    }

    /// Read-only access to the current shift-register value (for testing /
    /// diagnostics).
    pub fn bit_bfr(&self) -> u32 {
        self.bit_bfr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // StartCodeFinder — low-level next_start_code tests
    // -----------------------------------------------------------------------

    #[test]
    fn start_code_at_beginning() {
        let mut finder = StartCodeFinder::new();
        // Start code right at the front: 00 00 01 <payload>
        let data = [0x00, 0x00, 0x01, 0xAA, 0xBB];
        let res = finder.next_start_code(&data);
        assert!(res.found);
        // The C++ loop breaks *after* incrementing i past the 0x01 byte.
        assert_eq!(res.bytes_consumed, 3);
    }

    #[test]
    fn start_code_in_middle() {
        let mut finder = StartCodeFinder::new();
        let data = [0xFF, 0xFF, 0x00, 0x00, 0x01, 0x65];
        let res = finder.next_start_code(&data);
        assert!(res.found);
        assert_eq!(res.bytes_consumed, 5);
    }

    #[test]
    fn start_code_at_end() {
        let mut finder = StartCodeFinder::new();
        let data = [0xAA, 0xBB, 0x00, 0x00, 0x01];
        let res = finder.next_start_code(&data);
        assert!(res.found);
        assert_eq!(res.bytes_consumed, 5);
    }

    #[test]
    fn no_start_code() {
        let mut finder = StartCodeFinder::new();
        let data = [0xAA, 0xBB, 0xCC, 0xDD];
        let res = finder.next_start_code(&data);
        assert!(!res.found);
        assert_eq!(res.bytes_consumed, 4);
    }

    #[test]
    fn single_byte_no_start_code() {
        let mut finder = StartCodeFinder::new();
        let res = finder.next_start_code(&[0xFF]);
        assert!(!res.found);
        assert_eq!(res.bytes_consumed, 1);
    }

    #[test]
    #[should_panic(expected = "next_start_code called with empty data")]
    fn empty_data_panics() {
        let mut finder = StartCodeFinder::new();
        finder.next_start_code(&[]);
    }

    #[test]
    fn start_code_split_across_calls() {
        // First call ends with 0x00 0x00; second call begins with 0x01.
        // The shift register should carry the two zero bytes.
        let mut finder = StartCodeFinder::new();
        let part1 = [0xFF, 0x00, 0x00];
        let res1 = finder.next_start_code(&part1);
        assert!(!res1.found);
        assert_eq!(res1.bytes_consumed, 3);

        let part2 = [0x01, 0x65, 0x88];
        let res2 = finder.next_start_code(&part2);
        assert!(res2.found);
        // The 0x01 is the first byte; loop increments i to 1 then detects
        // the pattern.
        assert_eq!(res2.bytes_consumed, 1);
    }

    #[test]
    fn two_consecutive_start_codes() {
        let mut finder = StartCodeFinder::new();
        // Two back-to-back start codes: 00 00 01 | 00 00 01
        let data = [0x00, 0x00, 0x01, 0x00, 0x00, 0x01];
        let res1 = finder.next_start_code(&data);
        assert!(res1.found);
        assert_eq!(res1.bytes_consumed, 3);

        let res2 = finder.next_start_code(&data[res1.bytes_consumed..]);
        assert!(res2.found);
        assert_eq!(res2.bytes_consumed, 3);
    }

    #[test]
    fn four_byte_start_code() {
        // 00 00 00 01 is also a valid start code (the leading 0x00 is a
        // zero_byte). The finder should detect the 00 00 01 portion.
        let mut finder = StartCodeFinder::new();
        let data = [0x00, 0x00, 0x00, 0x01, 0x65];
        let res = finder.next_start_code(&data);
        assert!(res.found);
        // Consumed up through the 0x01 at index 3 -> 4 bytes consumed.
        assert_eq!(res.bytes_consumed, 4);
    }

    #[test]
    fn bit_bfr_initial_value() {
        let finder = StartCodeFinder::new();
        assert_eq!(finder.bit_bfr(), !0u32);
    }

    #[test]
    fn reset_restores_initial_state() {
        let mut finder = StartCodeFinder::new();
        finder.next_start_code(&[0x00, 0x00, 0x01]);
        finder.reset();
        assert_eq!(finder.bit_bfr(), !0u32);
    }
}
