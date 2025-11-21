// H.264 RTP Depacketization (RFC 6184)
//
// Implements FU-A (Fragmentation Unit) reassembly for H.264 NAL units transmitted over RTP.
//
// RFC 6184 defines three packetization modes:
// 1. Single NAL Unit mode: One NAL per RTP packet (NAL size < MTU)
// 2. FU-A mode: NAL fragmented across multiple RTP packets (NAL size >= MTU)
// 3. STAP-A mode: Multiple small NALs aggregated in one RTP packet
//
// This implementation handles modes 1 and 2, which are most common for streaming.

use crate::core::{Result, StreamError};
use bytes::Bytes;
use std::collections::HashMap;

/// H.264 NAL unit types (bits 0-4 of NAL header)
#[allow(dead_code)]
const NAL_TYPE_SLICE: u8 = 1; // Non-IDR coded slice
#[allow(dead_code)]
const NAL_TYPE_IDR: u8 = 5; // IDR (keyframe) coded slice
#[allow(dead_code)]
const NAL_TYPE_SEI: u8 = 6; // Supplemental Enhancement Information
#[allow(dead_code)]
const NAL_TYPE_SPS: u8 = 7; // Sequence Parameter Set
#[allow(dead_code)]
const NAL_TYPE_PPS: u8 = 8; // Picture Parameter Set
#[allow(dead_code)]
const NAL_TYPE_AUD: u8 = 9; // Access Unit Delimiter
const NAL_TYPE_FU_A: u8 = 28; // Fragmentation Unit A
const NAL_TYPE_STAP_A: u8 = 24; // Single-Time Aggregation Packet A

/// H.264 RTP depacketizer with FU-A reassembly
///
/// Handles incoming RTP packets and reconstructs complete NAL units.
/// Maintains state for fragmented packets across multiple RTP packets.
pub struct H264RtpDepacketizer {
    /// Buffer for FU-A fragments being reassembled (keyed by timestamp)
    fu_buffers: HashMap<u32, FuBuffer>,
}

/// FU-A reassembly buffer for a single NAL unit
struct FuBuffer {
    /// NAL header (reconstructed from FU indicator and header)
    nal_header: u8,
    /// Accumulated fragments
    fragments: Vec<Bytes>,
    /// Expected sequence number for next fragment
    next_seq: Option<u16>,
    /// Total size of accumulated data
    total_size: usize,
}

impl H264RtpDepacketizer {
    pub fn new() -> Self {
        Self {
            fu_buffers: HashMap::new(),
        }
    }

    /// Process an RTP packet and return complete NAL units if available
    ///
    /// # Arguments
    /// * `payload` - Raw RTP payload (H.264 packetized data)
    /// * `timestamp` - RTP timestamp for this packet
    /// * `seq_num` - RTP sequence number for detecting packet loss
    ///
    /// # Returns
    /// Vector of complete NAL units (may be empty if packet is a fragment)
    pub fn process_packet(
        &mut self,
        payload: Bytes,
        timestamp: u32,
        seq_num: u16,
    ) -> Result<Vec<Bytes>> {
        if payload.is_empty() {
            return Err(StreamError::Runtime("Empty RTP payload".into()));
        }

        // First byte is NAL header (forbidden_zero_bit | nal_ref_idc | nal_unit_type)
        let nal_header = payload[0];
        let nal_type = nal_header & 0x1F;

        match nal_type {
            NAL_TYPE_FU_A => {
                // Fragmentation Unit A - reassemble fragments
                self.process_fu_a(payload, timestamp, seq_num)
            }
            NAL_TYPE_STAP_A => {
                // Single-Time Aggregation Packet A - split into multiple NALs
                self.process_stap_a(payload)
            }
            _ => {
                // Single NAL Unit mode - return as-is
                tracing::trace!(
                    "[H264 RTP] Single NAL unit: type={}, size={}",
                    nal_type,
                    payload.len()
                );
                Ok(vec![payload])
            }
        }
    }

    /// Process FU-A (Fragmentation Unit) packet
    ///
    /// FU-A format:
    /// ```text
    /// 0                   1                   2                   3
    /// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// | FU indicator  |   FU header   |                               |
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               |
    /// |                                                               |
    /// |                         FU payload                            |
    /// |                                                               |
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// ```
    ///
    /// FU indicator: F=0 | NRI (2 bits) | Type=28
    /// FU header: S (start) | E (end) | R=0 | NAL type (5 bits)
    fn process_fu_a(&mut self, payload: Bytes, timestamp: u32, seq_num: u16) -> Result<Vec<Bytes>> {
        if payload.len() < 2 {
            return Err(StreamError::Runtime("FU-A packet too small".into()));
        }

        let fu_indicator = payload[0];
        let fu_header = payload[1];

        // Extract flags from FU header
        let start_bit = (fu_header & 0x80) != 0;
        let end_bit = (fu_header & 0x40) != 0;
        let nal_type = fu_header & 0x1F;

        // Reconstruct NAL header from FU indicator and FU header
        // NAL header = F (from FU indicator) | NRI (from FU indicator) | Type (from FU header)
        let nal_header = (fu_indicator & 0xE0) | nal_type;

        // Fragment payload starts at byte 2
        let fragment = payload.slice(2..);

        if start_bit {
            // First fragment - create new buffer
            let fragment_len = fragment.len();
            tracing::trace!(
                "[H264 RTP] FU-A START: type={}, ts={}, seq={}, size={}",
                nal_type,
                timestamp,
                seq_num,
                fragment_len
            );

            self.fu_buffers.insert(
                timestamp,
                FuBuffer {
                    nal_header,
                    fragments: vec![fragment],
                    next_seq: Some(seq_num.wrapping_add(1)),
                    total_size: fragment_len,
                },
            );

            Ok(vec![])
        } else if end_bit {
            // Last fragment - assemble complete NAL unit
            let buffer = match self.fu_buffers.remove(&timestamp) {
                Some(buf) => buf,
                None => {
                    // END without START - we joined mid-stream, discard silently
                    tracing::trace!(
                        "[H264 RTP] FU-A END without START (ts={}, seq={}) - mid-stream join, discarding",
                        timestamp, seq_num
                    );
                    return Ok(vec![]);
                }
            };

            // Check sequence number continuity
            if let Some(expected_seq) = buffer.next_seq {
                if seq_num != expected_seq {
                    tracing::warn!(
                        "[H264 RTP] FU-A sequence gap: expected={}, got={} (packet loss)",
                        expected_seq,
                        seq_num
                    );
                    // Continue anyway - decoder may be able to handle partial NAL
                }
            }

            // Reassemble complete NAL unit: [NAL header][fragment1][fragment2]...[fragmentN]
            let total_size = 1 + buffer.total_size + fragment.len();
            let num_fragments = buffer.fragments.len() + 1;
            let mut complete_nal = Vec::with_capacity(total_size);
            complete_nal.push(buffer.nal_header);

            for frag in buffer.fragments {
                complete_nal.extend_from_slice(&frag);
            }
            complete_nal.extend_from_slice(&fragment);

            tracing::debug!(
                "[H264 RTP] FU-A COMPLETE: type={}, fragments={}, total_size={}",
                nal_type,
                num_fragments,
                complete_nal.len()
            );

            Ok(vec![Bytes::from(complete_nal)])
        } else {
            // Middle fragment - append to buffer
            let buffer = match self.fu_buffers.get_mut(&timestamp) {
                Some(buf) => buf,
                None => {
                    // MIDDLE without START - we joined mid-stream, discard silently
                    tracing::trace!(
                        "[H264 RTP] FU-A MIDDLE without START (ts={}, seq={}) - mid-stream join, discarding",
                        timestamp, seq_num
                    );
                    return Ok(vec![]);
                }
            };

            // Check sequence number continuity
            if let Some(expected_seq) = buffer.next_seq {
                if seq_num != expected_seq {
                    tracing::warn!(
                        "[H264 RTP] FU-A sequence gap: expected={}, got={} (packet loss)",
                        expected_seq,
                        seq_num
                    );
                }
            }

            tracing::trace!(
                "[H264 RTP] FU-A MIDDLE: ts={}, seq={}, size={}",
                timestamp,
                seq_num,
                fragment.len()
            );

            buffer.total_size += fragment.len();
            buffer.fragments.push(fragment);
            buffer.next_seq = Some(seq_num.wrapping_add(1));

            Ok(vec![])
        }
    }

    /// Process STAP-A (Single-Time Aggregation Packet) packet
    ///
    /// STAP-A format:
    /// ```text
    /// 0                   1                   2                   3
    /// 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// |                          RTP Header                           |
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// |STAP-A NAL HDR |         NALU 1 Size           | NALU 1 HDR    |
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// |                         NALU 1 Data                           |
    /// :                                                               :
    /// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// ```
    fn process_stap_a(&mut self, payload: Bytes) -> Result<Vec<Bytes>> {
        let mut nal_units = Vec::new();
        let mut offset = 1; // Skip STAP-A NAL header

        while offset + 2 < payload.len() {
            // Read 2-byte NAL unit size (network byte order)
            let nal_size = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
            offset += 2;

            if offset + nal_size > payload.len() {
                return Err(StreamError::Runtime(format!(
                    "STAP-A NAL size exceeds packet bounds: {} > {}",
                    offset + nal_size,
                    payload.len()
                )));
            }

            // Extract NAL unit
            let nal_unit = payload.slice(offset..offset + nal_size);
            nal_units.push(nal_unit);
            offset += nal_size;

            tracing::trace!("[H264 RTP] STAP-A NAL: size={}", nal_size);
        }

        tracing::debug!("[H264 RTP] STAP-A: {} NAL units extracted", nal_units.len());

        Ok(nal_units)
    }

    /// Clean up stale FU-A buffers (call periodically to prevent memory leaks)
    ///
    /// Removes buffers older than the given threshold (in RTP timestamp units).
    /// For H.264 @ 90kHz clock, 90000 units = 1 second.
    pub fn cleanup_stale_buffers(&mut self, current_timestamp: u32, threshold: u32) {
        self.fu_buffers.retain(|&ts, _| {
            // Handle timestamp wraparound
            let age = current_timestamp.wrapping_sub(ts);
            age < threshold
        });
    }
}

impl Default for H264RtpDepacketizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_nal_unit() {
        let mut depacketizer = H264RtpDepacketizer::new();

        // Single NAL unit: IDR frame (type 5)
        let nal_header = 0x65; // F=0, NRI=3, Type=5 (IDR)
        let payload = Bytes::from(vec![nal_header, 0x01, 0x02, 0x03]);

        let result = depacketizer
            .process_packet(payload.clone(), 1000, 1)
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], payload);
    }

    #[test]
    fn test_fu_a_reassembly() {
        let mut depacketizer = H264RtpDepacketizer::new();

        // FU-A packet 1: Start bit set
        let fu_indicator = 0x7C; // F=0, NRI=3, Type=28 (FU-A)
        let fu_header_start = 0x85; // S=1, E=0, Type=5 (IDR)
        let fragment1 = Bytes::from(vec![fu_indicator, fu_header_start, 0x01, 0x02]);

        let result = depacketizer.process_packet(fragment1, 2000, 10).unwrap();
        assert_eq!(result.len(), 0); // No complete NAL yet

        // FU-A packet 2: Middle fragment
        let fu_header_middle = 0x05; // S=0, E=0, Type=5
        let fragment2 = Bytes::from(vec![fu_indicator, fu_header_middle, 0x03, 0x04]);

        let result = depacketizer.process_packet(fragment2, 2000, 11).unwrap();
        assert_eq!(result.len(), 0); // Still not complete

        // FU-A packet 3: End bit set
        let fu_header_end = 0x45; // S=0, E=1, Type=5
        let fragment3 = Bytes::from(vec![fu_indicator, fu_header_end, 0x05, 0x06]);

        let result = depacketizer.process_packet(fragment3, 2000, 12).unwrap();
        assert_eq!(result.len(), 1);

        // Check reassembled NAL: [NAL header][frag1][frag2][frag3]
        let expected = vec![0x65, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        assert_eq!(result[0].as_ref(), &expected);
    }
}
