// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Byte stream parser for NAL unit start code detection.
//!
//! Ports:
//!   - `NvVideoParser/include/ByteStreamParser.h` (ParseByteStreamSimd template)
//!   - `NvVideoParser/src/NextStartCodeC.cpp` (plain-C start code search)
//!
//! The C++ codebase has SIMD variants (SSE, AVX2, NEON, SVE) but we only port
//! the plain scalar fallback (`SIMD_ISA::NOSIMD`).  The algorithm scans a byte
//! buffer for the three-byte start code prefix `0x00 0x00 0x01` used in
//! H.264/H.265/AV1 Annex-B byte streams.
//!
//! Key C++ state that is mirrored here:
//!   - `m_BitBfr` (`u32`) — a rolling shift-register that accumulates the last
//!     few bytes so a start code split across two `next_start_code` calls is
//!     still detected.
//!   - `NvVkNalUnit` — tracks the byte offsets of the NAL unit currently being
//!     assembled inside the bitstream buffer.

/// Result returned by [`next_start_code`].
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
        Self {
            bit_bfr: !0u32,
        }
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

// ---------------------------------------------------------------------------
// NAL unit tracking — port of `NvVkNalUnit` from VulkanVideoDecoder.h
// ---------------------------------------------------------------------------

/// Tracks offsets into a bitstream buffer for a single NAL unit.
///
/// Port of the C++ `NvVkNalUnit` struct.
#[derive(Debug, Clone, Default)]
pub struct NalUnit {
    /// Start offset in the byte stream buffer.
    pub start_offset: i64,
    /// End offset in the byte stream buffer (exclusive — one past the last
    /// byte belonging to this NAL unit).
    pub end_offset: i64,
    /// Current read pointer inside this NAL unit.
    pub get_offset: i64,
    /// Running count of consecutive zero bytes (used during RBSP decoding).
    pub get_zero_cnt: i32,
    /// Bit buffer used for bitwise reading.
    pub get_bfr: u32,
    /// Current bit offset inside `get_bfr` (0..32).
    pub get_bfr_offs: u32,
    /// Count of emulation prevention bytes (`0x03`) encountered so far.
    pub get_emul_cnt: u32,
}

// ---------------------------------------------------------------------------
// ByteStreamParser — high-level NAL demuxer state machine
// ---------------------------------------------------------------------------

/// Errors that can be returned during byte-stream parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The parser has not been initialised (no bitstream buffer).
    NotInitialized,
    /// A bitstream buffer resize was needed but would exceed limits.
    BufferResizeFailed,
}

/// Packet flags mirroring `VkParserBitstreamPacket` fields that influence
/// the parse loop.
#[derive(Debug, Clone, Default)]
pub struct BitstreamPacket<'a> {
    /// Raw byte stream data.
    pub data: &'a [u8],
    /// Presentation time stamp.
    pub pts: Option<i64>,
    /// End-of-picture — flush the current picture after this packet.
    pub eop: bool,
    /// End-of-stream — flush everything.
    pub eos: bool,
    /// Discontinuity flag.
    pub discontinuity: bool,
    /// If true, return after every decoded frame.
    pub partial_parsing: bool,
    /// If true, the stream contains no start codes (length-prefixed NALUs).
    pub no_start_codes: bool,
}

/// Accumulated bitstream buffer that NAL units are assembled into.
///
/// In the C++ code this role is filled by `VulkanBitstreamBufferStream` +
/// a Vulkan device-memory backed buffer.  Here we use a plain `Vec<u8>` —
/// no GPU memory is touched at the parsing layer.
#[derive(Debug, Clone)]
pub struct BitstreamBuffer {
    data: Vec<u8>,
}

impl BitstreamBuffer {
    pub fn new(initial_capacity: usize) -> Self {
        Self {
            data: vec![0u8; initial_capacity],
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Ensure the buffer is at least `min_len` bytes long, growing if needed.
    pub fn ensure_capacity(&mut self, min_len: usize) {
        if self.data.len() < min_len {
            self.data.resize(min_len, 0u8);
        }
    }

    /// Copy `src` into the buffer starting at `offset`.
    pub fn copy_from_slice(&mut self, offset: usize, src: &[u8]) {
        let end = offset + src.len();
        self.ensure_capacity(end);
        self.data[offset..end].copy_from_slice(src);
    }

    /// Write the start code prefix `0x00 0x00 0x01` at `offset`.
    /// Corresponds to `SetSliceStartCodeAtOffset`.
    pub fn set_start_code_at(&mut self, offset: usize) {
        self.ensure_capacity(offset + 3);
        self.data[offset] = 0x00;
        self.data[offset + 1] = 0x00;
        self.data[offset + 2] = 0x01;
    }

    /// Check whether bytes at `offset` are `0x00 0x00 0x01`.
    /// Corresponds to `HasSliceStartCodeAtOffset`.
    pub fn has_start_code_at(&self, offset: usize) -> bool {
        offset + 3 <= self.data.len()
            && self.data[offset] == 0x00
            && self.data[offset + 1] == 0x00
            && self.data[offset + 2] == 0x01
    }

    /// Raw slice access.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Discard all data before `start`, keeping `len` bytes.
    /// Corresponds to `swapBitstreamBuffer`.
    pub fn swap(&mut self, start: usize, len: usize) -> usize {
        if len > 0 && start > 0 {
            self.data.copy_within(start..start + len, 0);
        }
        // Zero the rest (defensive, like C++ memset after swap)
        for b in &mut self.data[len..] {
            *b = 0;
        }
        self.data.len()
    }
}

/// High-level byte stream parser that assembles NAL units from a stream of
/// [`BitstreamPacket`]s.
///
/// This corresponds to the `ParseByteStreamSimd<NOSIMD>` template
/// instantiation in `ByteStreamParser.h`, combined with the scalar
/// `next_start_code` from `NextStartCodeC.cpp`.
#[derive(Debug)]
pub struct ByteStreamParser {
    /// Start code scanner state.
    pub finder: StartCodeFinder,
    /// Current NAL unit being assembled.
    pub nalu: NalUnit,
    /// Bitstream accumulation buffer.
    pub buffer: BitstreamBuffer,
    /// Total bytes parsed across all packets.
    pub parsed_bytes: i64,
    /// Byte position where the current NAL unit begins in the overall stream.
    pub nalu_start_location: i64,
    /// Completed NAL unit payloads emitted during the most recent
    /// `parse_byte_stream` call.  Each entry is the raw NAL bytes
    /// (excluding the start code prefix).
    pub completed_nalus: Vec<Vec<u8>>,
}

impl ByteStreamParser {
    /// Create a parser with an initial buffer of `capacity` bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            finder: StartCodeFinder::new(),
            nalu: NalUnit::default(),
            buffer: BitstreamBuffer::new(capacity),
            parsed_bytes: 0,
            nalu_start_location: 0,
            completed_nalus: Vec::new(),
        }
    }

    /// Reset the parser to its initial state (matches `Initialize` +
    /// `m_BitBfr = ~0` in C++).
    pub fn reset(&mut self) {
        self.finder.reset();
        self.nalu = NalUnit::default();
        self.parsed_bytes = 0;
        self.nalu_start_location = 0;
        self.completed_nalus.clear();
    }

    /// Feed a packet into the parser.
    ///
    /// This is the main entry point, porting the `while (curr_data_size > 0)`
    /// loop from `ParseByteStreamSimd`.  Each time a complete NAL unit
    /// boundary is found (start code -> start code), the NAL unit's byte
    /// range is pushed to `self.completed_nalus`.
    ///
    /// Returns the number of bytes consumed from `pck.data`.
    pub fn parse_byte_stream(&mut self, pck: &BitstreamPacket) -> Result<usize, ParseError> {
        if self.buffer.is_empty() {
            return Err(ParseError::NotInitialized);
        }

        self.completed_nalus.clear();

        let mut remaining = pck.data;

        // ---- Start code based parsing ----
        while !remaining.is_empty() {
            let result = self.finder.next_start_code(remaining);
            let data_used = if result.found {
                result.bytes_consumed
            } else {
                remaining.len()
            };

            // Copy consumed bytes into the bitstream buffer.
            // In the C++ code, data (including the start code bytes) is
            // copied into the buffer at end_offset, then when a start code
            // is found, end_offset is rolled back by 3 to exclude the
            // 00 00 01 prefix.
            if data_used > 0 {
                let end = self.nalu.end_offset as usize;
                self.buffer.ensure_capacity(end + data_used);
                let copy_len = data_used.min(self.buffer.len() - end);
                if copy_len > 0 {
                    self.buffer
                        .copy_from_slice(end, &remaining[..copy_len]);
                }
                self.nalu.end_offset += copy_len as i64;
                self.parsed_bytes += copy_len as i64;
            }

            remaining = &remaining[data_used..];

            if result.found {
                if self.nalu.start_offset == 0 {
                    self.nalu_start_location =
                        self.parsed_bytes - self.nalu.end_offset;
                }
                // Remove the trailing 0x00 0x00 0x01 from this NAL unit.
                self.nalu.end_offset =
                    if self.nalu.end_offset >= 3 { self.nalu.end_offset - 3 } else { 0 };

                // Record the completed NAL unit (if non-empty).
                if self.nalu.end_offset > self.nalu.start_offset {
                    let s = self.nalu.start_offset as usize;
                    let e = self.nalu.end_offset as usize;
                    self.completed_nalus
                        .push(self.buffer.as_slice()[s..e].to_vec());
                }

                // In the C++ code, after nal_unit() the parser writes a
                // start code prefix at end_offset for the *next* NAL unit,
                // then sets start_offset = end_offset + 3 (start of next
                // NAL payload).  We do the same: the buffer now contains
                // [completed NAL data][00 00 01][... next NAL ...].
                //
                // However, for our simplified output we reset offsets so
                // that completed_nalus ranges always index directly into
                // the buffer.  We achieve this by compacting: move the
                // write cursor to 0 for the next NAL unit.
                self.nalu.start_offset = 0;
                self.nalu.end_offset = 0;
            }
        }

        // Handle end-of-picture / end-of-stream.
        if pck.eop || pck.eos {
            if self.nalu.start_offset == 0 {
                self.nalu_start_location =
                    self.parsed_bytes - self.nalu.end_offset;
            }

            // Emit remaining NAL unit (the one after the last start code).
            if self.nalu.end_offset > self.nalu.start_offset {
                let s = self.nalu.start_offset as usize;
                let e = self.nalu.end_offset as usize;
                self.completed_nalus
                    .push(self.buffer.as_slice()[s..e].to_vec());
            }

            self.nalu.end_offset = 0;
            self.nalu.start_offset = 0;
            self.nalu_start_location = self.parsed_bytes;
        }

        Ok(pck.data.len() - remaining.len())
    }

    /// Access the completed NAL units from the most recent parse call.
    pub fn completed_nalus(&self) -> &[Vec<u8>] {
        &self.completed_nalus
    }
}

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

// ===========================================================================
// Unit tests
// ===========================================================================
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
        let input = [
            0xAA, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0xBB,
        ];
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
    // ByteStreamParser — integration-level tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_single_nalu() {
        let mut parser = ByteStreamParser::new(1024);

        // A packet containing: start_code | payload | start_code (marks end)
        // 00 00 01 65 AA BB 00 00 01
        let data = [0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0x00, 0x00, 0x01];
        let pck = BitstreamPacket {
            data: &data,
            eop: false,
            ..Default::default()
        };
        let consumed = parser.parse_byte_stream(&pck).unwrap();
        assert_eq!(consumed, data.len());

        // First start code is consumed as preamble (empty NAL before it),
        // the second start code terminates the NAL containing 0x65 0xAA 0xBB.
        // We should have exactly one non-empty NAL unit.
        assert_eq!(parser.completed_nalus.len(), 1);
        assert_eq!(parser.completed_nalus[0], &[0x65, 0xAA, 0xBB]);
    }

    #[test]
    fn parse_two_nalus() {
        let mut parser = ByteStreamParser::new(1024);

        // start | NAL1 | start | NAL2 | EOP
        let data = [
            0x00, 0x00, 0x01, // start code 1
            0x67, 0x42,       // NAL 1 payload (SPS-ish)
            0x00, 0x00, 0x01, // start code 2
            0x68, 0xCE,       // NAL 2 payload (PPS-ish)
        ];
        let pck = BitstreamPacket {
            data: &data,
            eop: true,
            ..Default::default()
        };
        let consumed = parser.parse_byte_stream(&pck).unwrap();
        assert_eq!(consumed, data.len());

        // Two completed NAL units expected.
        assert_eq!(parser.completed_nalus.len(), 2);
        assert_eq!(parser.completed_nalus[0], &[0x67, 0x42]);
        assert_eq!(parser.completed_nalus[1], &[0x68, 0xCE]);
    }

    #[test]
    fn parse_eop_flushes_trailing_nalu() {
        let mut parser = ByteStreamParser::new(1024);

        // start | payload — no trailing start code, but EOP forces flush.
        let data = [0x00, 0x00, 0x01, 0x65, 0xAA];
        let pck = BitstreamPacket {
            data: &data,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).unwrap();
        assert_eq!(parser.completed_nalus.len(), 1);
        assert_eq!(parser.completed_nalus[0], &[0x65, 0xAA]);
    }

    #[test]
    fn parse_empty_packet() {
        let mut parser = ByteStreamParser::new(1024);
        let pck = BitstreamPacket {
            data: &[],
            ..Default::default()
        };
        let consumed = parser.parse_byte_stream(&pck).unwrap();
        assert_eq!(consumed, 0);
        assert!(parser.completed_nalus.is_empty());
    }

    #[test]
    fn parse_no_start_code_in_data() {
        let mut parser = ByteStreamParser::new(1024);
        let data = [0xAA, 0xBB, 0xCC, 0xDD];
        let pck = BitstreamPacket {
            data: &data,
            eop: false,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).unwrap();
        // No start code found — nothing completed yet.
        assert!(parser.completed_nalus.is_empty());
    }

    #[test]
    fn parse_not_initialized() {
        let mut parser = ByteStreamParser {
            finder: StartCodeFinder::new(),
            nalu: NalUnit::default(),
            buffer: BitstreamBuffer { data: Vec::new() },
            parsed_bytes: 0,
            nalu_start_location: 0,
            completed_nalus: Vec::new(),
        };
        let pck = BitstreamPacket {
            data: &[0x00, 0x00, 0x01],
            ..Default::default()
        };
        assert_eq!(
            parser.parse_byte_stream(&pck),
            Err(ParseError::NotInitialized)
        );
    }

    #[test]
    fn parse_four_byte_start_code() {
        let mut parser = ByteStreamParser::new(1024);
        // 00 00 00 01 is a valid 4-byte start code (leading zero_byte).
        // The scanner sees the leading 0x00 as data before the start code,
        // so it emits a one-byte "NAL" containing 0x00 (the zero_byte),
        // followed by the real NAL payload.  In practice, the caller
        // discards any data before the first real start code.
        let data = [0x00, 0x00, 0x00, 0x01, 0x67, 0x42];
        let pck = BitstreamPacket {
            data: &data,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).unwrap();
        // Two "NAL units": [0x00] (zero_byte prefix) and [0x67, 0x42].
        assert_eq!(parser.completed_nalus.len(), 2);
        assert_eq!(parser.completed_nalus[0], &[0x00]);
        assert_eq!(parser.completed_nalus[1], &[0x67, 0x42]);
    }

    #[test]
    fn parse_split_across_packets() {
        let mut parser = ByteStreamParser::new(1024);

        // Packet 1: start code + beginning of NAL payload.
        let data1 = [0x00, 0x00, 0x01, 0x65, 0xAA];
        let pck1 = BitstreamPacket {
            data: &data1,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck1).unwrap();
        // NAL not terminated yet.
        assert!(parser.completed_nalus.is_empty());

        // Packet 2: rest of payload + next start code.
        let data2 = [0xBB, 0xCC, 0x00, 0x00, 0x01, 0x68];
        let pck2 = BitstreamPacket {
            data: &data2,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck2).unwrap();
        // Now we should have two NALUs: one from the start code boundary,
        // one flushed by EOP.
        assert!(parser.completed_nalus.len() >= 1);
    }

    #[test]
    fn parse_start_code_at_very_end_of_buffer() {
        let mut parser = ByteStreamParser::new(1024);

        // Exactly a start code and nothing else — should not produce a NAL
        // (empty payload).
        let data = [0x00, 0x00, 0x01];
        let pck = BitstreamPacket {
            data: &data,
            eop: true,
            ..Default::default()
        };
        parser.parse_byte_stream(&pck).unwrap();
        // The NAL between start-of-stream and the start code is empty,
        // and the NAL after the start code is also empty (EOP with no data).
        // Depending on implementation some empty nalus may or may not be emitted.
        // The key invariant is no panic and no garbage data.
        // All emitted NALUs should be non-empty (we skip empty ones).
        for nalu in &parser.completed_nalus {
            assert!(!nalu.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // BitstreamBuffer
    // -----------------------------------------------------------------------

    #[test]
    fn buffer_start_code_round_trip() {
        let mut buf = BitstreamBuffer::new(16);
        buf.set_start_code_at(5);
        assert!(buf.has_start_code_at(5));
        assert!(!buf.has_start_code_at(0));
    }

    #[test]
    fn buffer_swap_discards_prefix() {
        let mut buf = BitstreamBuffer::new(16);
        buf.copy_from_slice(0, &[0xAA, 0xBB, 0xCC, 0xDD]);
        buf.swap(2, 2); // keep bytes at offset 2..4
        assert_eq!(buf.as_slice()[0], 0xCC);
        assert_eq!(buf.as_slice()[1], 0xDD);
    }
}
