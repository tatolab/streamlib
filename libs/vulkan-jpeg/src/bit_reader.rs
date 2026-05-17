// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::error::{JpegError, JpegResult};
use crate::marker;

/// MSB-first bit reader over a JPEG entropy-coded segment.
///
/// Handles the `0xFF 0x00` byte-stuffing escape (the literal data byte
/// is `0xFF`) and surfaces any other `0xFF xx` sequence as a marker via
/// [`BitReader::pending_marker`]. The bit-by-bit decode loop never crosses
/// a marker boundary; the entropy decoder is expected to inspect the
/// pending marker (typically a restart `RSTm` or terminating `EOI`) and
/// either consume it via [`BitReader::consume_marker`] or stop.
pub struct BitReader<'a> {
    bytes: &'a [u8],
    /// Byte offset into `bytes` of the *next* byte to fetch into `buffer`.
    cursor: usize,
    /// Bit buffer, MSB-first; `bits_in_buffer` is how many valid bits remain.
    buffer: u64,
    bits_in_buffer: u32,
    /// Marker byte (the byte after `0xFF`) encountered during refill, if any.
    pending_marker: Option<u8>,
    /// Byte offset where the pending marker was found.
    pending_marker_offset: Option<usize>,
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            cursor: 0,
            buffer: 0,
            bits_in_buffer: 0,
            pending_marker: None,
            pending_marker_offset: None,
        }
    }

    /// Byte offset of the next byte the reader would fetch (after the
    /// current bit buffer). Used for error messages and marker offset.
    pub fn byte_offset(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub fn pending_marker(&self) -> Option<u8> {
        self.pending_marker
    }

    /// Discard any partial-byte bits and clear the bit buffer. Called by
    /// the entropy decoder before consuming a restart marker so the next
    /// read starts on a byte boundary.
    pub fn reset_to_byte_boundary(&mut self) {
        self.buffer = 0;
        self.bits_in_buffer = 0;
    }

    /// Read the next marker byte (the byte after `0xFF`) from the
    /// underlying stream, advancing past both bytes. Honors a previously-
    /// surfaced pending marker if present. Fill `0xFF` bytes are skipped.
    pub fn read_marker(&mut self) -> JpegResult<u8> {
        if let Some(marker_byte) = self.pending_marker {
            let offset = self
                .pending_marker_offset
                .unwrap_or(self.cursor);
            self.cursor = offset + 2;
            self.pending_marker = None;
            self.pending_marker_offset = None;
            return Ok(marker_byte);
        }
        // No pending marker — read raw from the source. Skip any run of
        // 0xFF fill bytes preceding the actual marker.
        while self.cursor + 1 < self.bytes.len()
            && self.bytes[self.cursor] == marker::PREFIX
            && self.bytes[self.cursor + 1] == marker::PREFIX
        {
            self.cursor += 1;
        }
        if self.cursor + 1 >= self.bytes.len() {
            return Err(JpegError::UnexpectedEof {
                offset: self.cursor,
            });
        }
        if self.bytes[self.cursor] != marker::PREFIX {
            return Err(JpegError::UnexpectedMarker {
                marker: self.bytes[self.cursor],
                offset: self.cursor,
            });
        }
        let marker_byte = self.bytes[self.cursor + 1];
        self.cursor += 2;
        Ok(marker_byte)
    }

    /// Read a single bit. Returns the bit as 0 or 1.
    pub fn read_bit(&mut self) -> JpegResult<u8> {
        if self.bits_in_buffer == 0 {
            self.refill()?;
            if self.bits_in_buffer == 0 {
                // Refill stalled on a marker or EOF.
                return Err(self.eof_or_marker_error());
            }
        }
        self.bits_in_buffer -= 1;
        Ok(((self.buffer >> self.bits_in_buffer) & 1) as u8)
    }

    /// Read `n` bits (1..=16) as an MSB-first unsigned integer.
    pub fn read_bits(&mut self, n: u32) -> JpegResult<u32> {
        debug_assert!(n <= 16);
        while self.bits_in_buffer < n {
            let before = self.bits_in_buffer;
            self.refill()?;
            if self.bits_in_buffer == before {
                return Err(self.eof_or_marker_error());
            }
        }
        self.bits_in_buffer -= n;
        let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
        Ok(((self.buffer >> self.bits_in_buffer) as u32) & mask)
    }

    /// JPEG F.2.1.3.1: extend an `n`-bit value read from the stream into a
    /// signed coefficient. Values with the MSB set are positive as-is;
    /// values with the MSB clear are negated relative to `(1 << n) - 1`.
    pub fn extend(value: u32, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let vt = 1i32 << (n - 1);
        let v = value as i32;
        if v < vt { v + (-1i32 << n) + 1 } else { v }
    }

    fn refill(&mut self) -> JpegResult<()> {
        // Pull bytes one at a time so we can honor stuffed-byte escapes and
        // surface markers immediately. The buffer is 64 bits wide so we
        // can keep going until we hit a marker or run out of room.
        while self.bits_in_buffer <= 56 && self.pending_marker.is_none() {
            if self.cursor >= self.bytes.len() {
                return Ok(());
            }
            let byte = self.bytes[self.cursor];
            if byte == marker::PREFIX {
                // Need at least one more byte to disambiguate stuff vs. marker.
                if self.cursor + 1 >= self.bytes.len() {
                    // Hanging 0xFF at end of input — treat as EOF.
                    return Ok(());
                }
                let next = self.bytes[self.cursor + 1];
                if next == 0x00 {
                    // Byte stuff: consume both, push a single 0xFF data byte.
                    self.cursor += 2;
                    self.buffer = (self.buffer << 8) | (marker::PREFIX as u64);
                    self.bits_in_buffer += 8;
                } else {
                    // Real marker. Surface to caller. Do NOT advance past it;
                    // the entropy decoder will consume it via consume_marker
                    // and then jump the parser cursor accordingly.
                    self.pending_marker = Some(next);
                    self.pending_marker_offset = Some(self.cursor);
                    return Ok(());
                }
            } else {
                self.cursor += 1;
                self.buffer = (self.buffer << 8) | (byte as u64);
                self.bits_in_buffer += 8;
            }
        }
        Ok(())
    }

    fn eof_or_marker_error(&self) -> JpegError {
        if let Some(marker_byte) = self.pending_marker {
            JpegError::UnexpectedMarker {
                marker: marker_byte,
                offset: self.pending_marker_offset.unwrap_or(self.cursor),
            }
        } else {
            JpegError::UnexpectedEof {
                offset: self.cursor,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first() {
        // 0b1010_1100 0b0011_0101
        let bytes = [0xAC, 0x35];
        let mut br = BitReader::new(&bytes);
        assert_eq!(br.read_bits(4).unwrap(), 0b1010);
        assert_eq!(br.read_bits(4).unwrap(), 0b1100);
        assert_eq!(br.read_bits(8).unwrap(), 0x35);
    }

    #[test]
    fn unstuffs_ff_00() {
        // Stuffed 0xFF data byte: in stream as `0xFF 0x00`.
        let bytes = [0xFF, 0x00, 0x55];
        let mut br = BitReader::new(&bytes);
        assert_eq!(br.read_bits(8).unwrap(), 0xFF);
        assert_eq!(br.read_bits(8).unwrap(), 0x55);
        assert!(br.pending_marker().is_none());
    }

    #[test]
    fn surfaces_marker_without_consuming() {
        // 0xFF D0 = RST0 marker. Bits before it are valid, then refill stops.
        let bytes = [0xAB, 0xFF, 0xD0, 0x99];
        let mut br = BitReader::new(&bytes);
        assert_eq!(br.read_bits(8).unwrap(), 0xAB);
        // Next read must fail because the marker isn't data.
        let err = br.read_bits(8).unwrap_err();
        assert!(matches!(err, JpegError::UnexpectedMarker { marker: 0xD0, .. }));
        assert_eq!(br.pending_marker(), Some(0xD0));
    }

    #[test]
    fn read_marker_after_pending() {
        let bytes = [0xAB, 0xFF, 0xD0, 0x99];
        let mut br = BitReader::new(&bytes);
        assert_eq!(br.read_bits(8).unwrap(), 0xAB);
        // Force a refill to surface the marker.
        let _ = br.read_bits(8);
        br.reset_to_byte_boundary();
        assert_eq!(br.read_marker().unwrap(), 0xD0);
        // After consuming the marker the next byte is data again.
        assert_eq!(br.byte_offset(), 3);
    }

    #[test]
    fn read_marker_from_raw_stream() {
        let bytes = [0xFF, 0xD9];
        let mut br = BitReader::new(&bytes);
        assert_eq!(br.read_marker().unwrap(), 0xD9);
    }

    #[test]
    fn extend_matches_jpeg_spec() {
        // Per F.2.1.3.1: a category-3 value of 0b000 means -7, 0b111 means +7.
        assert_eq!(BitReader::extend(0b000, 3), -7);
        assert_eq!(BitReader::extend(0b001, 3), -6);
        assert_eq!(BitReader::extend(0b011, 3), -4);
        assert_eq!(BitReader::extend(0b100, 3), 4);
        assert_eq!(BitReader::extend(0b111, 3), 7);
        assert_eq!(BitReader::extend(0, 0), 0);
    }
}
