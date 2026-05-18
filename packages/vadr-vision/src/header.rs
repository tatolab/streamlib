// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! VADR-TS-002 §4.6 24-byte little-endian per-datagram header.

/// VADR-TS-002 §4.6 header is always 24 bytes — `u32 + u16 + u16 + u32 +
/// u32 + u64`, packed, little-endian.
pub const HEADER_LEN: usize = 24;

/// Parsed per-datagram header. Field order matches §4.6 wire layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    pub frame_id: u32,
    pub chunk_id: u16,
    pub total_chunks: u16,
    pub jpeg_size: u32,
    pub payload_size: u32,
    pub sim_time_ns: u64,
}

/// Errors observed parsing a datagram's leading bytes as a `ChunkHeader`.
/// Surfaced for tests + logging — the depayloader treats all failures the
/// same (drop the datagram, advance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// Datagram smaller than 24 bytes — can't possibly carry a header.
    DatagramTooShort { len: usize },
    /// Datagram's `payload_size` field disagrees with the actual byte
    /// count beyond the header.
    PayloadSizeMismatch { declared: u32, observed: usize },
    /// `total_chunks == 0` — spec doesn't define this and dividing by it
    /// would crash the assembler. Drop.
    ZeroTotalChunks,
    /// `chunk_id >= total_chunks` — out-of-range index. Drop.
    ChunkIdOutOfRange { chunk_id: u16, total_chunks: u16 },
    /// `payload_size > jpeg_size` — single chunk claims to carry more
    /// bytes than the whole frame.
    PayloadLargerThanFrame { payload_size: u32, jpeg_size: u32 },
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DatagramTooShort { len } => write!(f, "datagram too short ({len} < 24 bytes)"),
            Self::PayloadSizeMismatch { declared, observed } => write!(
                f,
                "payload_size mismatch (header declares {declared}, datagram carries {observed})"
            ),
            Self::ZeroTotalChunks => write!(f, "total_chunks = 0"),
            Self::ChunkIdOutOfRange {
                chunk_id,
                total_chunks,
            } => write!(
                f,
                "chunk_id {chunk_id} >= total_chunks {total_chunks}"
            ),
            Self::PayloadLargerThanFrame {
                payload_size,
                jpeg_size,
            } => write!(
                f,
                "payload_size {payload_size} > jpeg_size {jpeg_size}"
            ),
        }
    }
}

impl std::error::Error for HeaderError {}

/// Parse the first 24 bytes of `datagram` as a `ChunkHeader` and validate
/// it against the remaining payload. Returns the parsed header + a slice
/// borrowing the chunk payload bytes (i.e. `datagram[24..24+payload_size]`).
pub fn parse(datagram: &[u8]) -> Result<(ChunkHeader, &[u8]), HeaderError> {
    if datagram.len() < HEADER_LEN {
        return Err(HeaderError::DatagramTooShort {
            len: datagram.len(),
        });
    }
    // Hard-coded offsets per §4.6. Each .try_into() is infallible
    // because the slices are statically the right width — unwrap is the
    // same cost as a checked panic but documents intent.
    let header = ChunkHeader {
        frame_id: u32::from_le_bytes(datagram[0..4].try_into().unwrap()),
        chunk_id: u16::from_le_bytes(datagram[4..6].try_into().unwrap()),
        total_chunks: u16::from_le_bytes(datagram[6..8].try_into().unwrap()),
        jpeg_size: u32::from_le_bytes(datagram[8..12].try_into().unwrap()),
        payload_size: u32::from_le_bytes(datagram[12..16].try_into().unwrap()),
        sim_time_ns: u64::from_le_bytes(datagram[16..24].try_into().unwrap()),
    };

    let payload = &datagram[HEADER_LEN..];

    if header.total_chunks == 0 {
        return Err(HeaderError::ZeroTotalChunks);
    }
    if header.chunk_id >= header.total_chunks {
        return Err(HeaderError::ChunkIdOutOfRange {
            chunk_id: header.chunk_id,
            total_chunks: header.total_chunks,
        });
    }
    if header.payload_size as usize != payload.len() {
        return Err(HeaderError::PayloadSizeMismatch {
            declared: header.payload_size,
            observed: payload.len(),
        });
    }
    if header.payload_size > header.jpeg_size {
        return Err(HeaderError::PayloadLargerThanFrame {
            payload_size: header.payload_size,
            jpeg_size: header.jpeg_size,
        });
    }

    Ok((header, payload))
}

/// Encode a `ChunkHeader` + payload into the on-wire datagram bytes. The
/// processor never encodes — this is in the public API so unit and
/// integration tests can build chunked streams to feed `parse()` /
/// `DepayloaderState::ingest()` without duplicating the byte layout.
pub fn encode(header: &ChunkHeader, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&header.frame_id.to_le_bytes());
    out.extend_from_slice(&header.chunk_id.to_le_bytes());
    out.extend_from_slice(&header.total_chunks.to_le_bytes());
    out.extend_from_slice(&header.jpeg_size.to_le_bytes());
    out.extend_from_slice(&header.payload_size.to_le_bytes());
    out.extend_from_slice(&header.sim_time_ns.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> ChunkHeader {
        ChunkHeader {
            frame_id: 0xDEAD_BEEF,
            chunk_id: 3,
            total_chunks: 7,
            jpeg_size: 32_000,
            payload_size: 5,
            sim_time_ns: 0x0123_4567_89AB_CDEF,
        }
    }

    /// Round-trip a representative header + payload through `encode` →
    /// `parse` and assert byte-for-byte fidelity. Locks the §4.6 LE field
    /// ordering: any byte-swap or field reorder would fail this.
    #[test]
    fn encode_parse_round_trip_preserves_all_fields() {
        let payload = b"hello";
        let bytes = encode(&sample_header(), payload);
        assert_eq!(bytes.len(), HEADER_LEN + payload.len());

        let (parsed, parsed_payload) = parse(&bytes).expect("parse");
        assert_eq!(parsed, sample_header());
        assert_eq!(parsed_payload, payload);
    }

    /// Spot-check the exact LE byte positions of `frame_id`, `chunk_id`,
    /// `total_chunks`, and `sim_time_ns`. If the parser silently
    /// big-endians one of them, this fails. The hand-rolled expected
    /// bytes mirror the §4.6 wire layout exactly.
    #[test]
    fn encoded_bytes_match_spec_le_layout() {
        let header = ChunkHeader {
            frame_id: 0x01020304,
            chunk_id: 0x0506,
            total_chunks: 0x0708,
            jpeg_size: 0x090A0B0C,
            payload_size: 0x0D0E0F10,
            sim_time_ns: 0x1112131415161718,
        };
        // payload_size declares 4 bytes but encode() doesn't enforce the
        // invariant — this test asserts pure byte layout, not validation.
        let payload = b"\xAA\xBB\xCC\xDD";
        let bytes = encode(&header, payload);

        // frame_id: 0x01020304 → LE = [0x04, 0x03, 0x02, 0x01]
        assert_eq!(&bytes[0..4], &[0x04, 0x03, 0x02, 0x01]);
        // chunk_id: 0x0506 → LE = [0x06, 0x05]
        assert_eq!(&bytes[4..6], &[0x06, 0x05]);
        // total_chunks: 0x0708 → LE = [0x08, 0x07]
        assert_eq!(&bytes[6..8], &[0x08, 0x07]);
        // jpeg_size: 0x090A0B0C → LE = [0x0C, 0x0B, 0x0A, 0x09]
        assert_eq!(&bytes[8..12], &[0x0C, 0x0B, 0x0A, 0x09]);
        // payload_size: 0x0D0E0F10 → LE
        assert_eq!(&bytes[12..16], &[0x10, 0x0F, 0x0E, 0x0D]);
        // sim_time_ns: 0x1112131415161718 → LE
        assert_eq!(
            &bytes[16..24],
            &[0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11]
        );
        assert_eq!(&bytes[24..], payload);
    }

    /// Boundary values: max u32 frame_id, max u16 chunk_id +
    /// total_chunks, max u64 sim_time_ns. Locks that we're not silently
    /// truncating any field's full range.
    #[test]
    fn encode_parse_round_trips_boundary_values() {
        let header = ChunkHeader {
            frame_id: u32::MAX,
            chunk_id: u16::MAX - 1,
            total_chunks: u16::MAX,
            jpeg_size: u32::MAX,
            payload_size: 0,
            sim_time_ns: u64::MAX,
        };
        let bytes = encode(&header, &[]);
        let (parsed, payload) = parse(&bytes).expect("parse");
        assert_eq!(parsed, header);
        assert!(payload.is_empty());
    }

    /// Datagram with fewer than 24 bytes can't be a valid header. The
    /// parser surfaces the typed error without panicking on the short
    /// slice.
    #[test]
    fn parse_rejects_short_datagram() {
        let short = vec![0u8; 10];
        let err = parse(&short).unwrap_err();
        assert_eq!(err, HeaderError::DatagramTooShort { len: 10 });
    }

    /// total_chunks = 0 is a divide-by-zero waiting to happen in the
    /// assembler. Reject at parse time.
    #[test]
    fn parse_rejects_zero_total_chunks() {
        let header = ChunkHeader {
            frame_id: 1,
            chunk_id: 0,
            total_chunks: 0,
            jpeg_size: 10,
            payload_size: 0,
            sim_time_ns: 0,
        };
        let bytes = encode(&header, &[]);
        assert_eq!(parse(&bytes).unwrap_err(), HeaderError::ZeroTotalChunks);
    }

    /// `chunk_id == total_chunks` is out of range (valid indices are
    /// 0..total_chunks). Reject — otherwise the assembler would overflow
    /// its slot vec.
    #[test]
    fn parse_rejects_chunk_id_equal_to_total_chunks() {
        let header = ChunkHeader {
            frame_id: 1,
            chunk_id: 5,
            total_chunks: 5,
            jpeg_size: 100,
            payload_size: 20,
            sim_time_ns: 0,
        };
        let bytes = encode(&header, &[0u8; 20]);
        let err = parse(&bytes).unwrap_err();
        assert_eq!(
            err,
            HeaderError::ChunkIdOutOfRange {
                chunk_id: 5,
                total_chunks: 5,
            }
        );
    }

    /// The header field `payload_size` MUST agree with the actual byte
    /// count following the 24-byte header. A 50-byte declared payload
    /// with 10 bytes of actual trailing data is malformed; parser
    /// rejects.
    #[test]
    fn parse_rejects_payload_size_mismatch() {
        let header = ChunkHeader {
            frame_id: 1,
            chunk_id: 0,
            total_chunks: 1,
            jpeg_size: 50,
            payload_size: 50,
            sim_time_ns: 0,
        };
        // Encode declares 50 bytes but we only ship 10.
        let mut bytes = encode(&header, &[]);
        bytes.extend_from_slice(&[0u8; 10]);
        let err = parse(&bytes).unwrap_err();
        assert_eq!(
            err,
            HeaderError::PayloadSizeMismatch {
                declared: 50,
                observed: 10,
            }
        );
    }

    /// A single chunk claiming more bytes than the whole frame holds is
    /// malformed.
    #[test]
    fn parse_rejects_payload_larger_than_frame() {
        let header = ChunkHeader {
            frame_id: 1,
            chunk_id: 0,
            total_chunks: 1,
            jpeg_size: 10,
            payload_size: 100,
            sim_time_ns: 0,
        };
        let bytes = encode(&header, &[0u8; 100]);
        let err = parse(&bytes).unwrap_err();
        assert_eq!(
            err,
            HeaderError::PayloadLargerThanFrame {
                payload_size: 100,
                jpeg_size: 10,
            }
        );
    }
}
