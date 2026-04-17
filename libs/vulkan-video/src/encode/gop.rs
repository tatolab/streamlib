// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! GOP (Group of Pictures) frame type decisions and B-frame reorder buffer.

use crate::video_context::VideoError;
use crate::vk_video_encoder::vk_video_gop_structure::{
    GopPosition, FrameType as GopFrameType,
};

use super::config::{EncodePacket, FrameType};
use super::SimpleEncoder;

/// Entry in the B-frame reorder buffer.
pub(crate) struct ReorderEntry {
    /// Raw NV12 pixel data (copied from the caller's buffer).
    pub(crate) nv12_data: Vec<u8>,
    /// Display-order timestamp (frame index in input order).
    pub(crate) display_pts: u64,
    /// Monotonic timestamp from the caller (passthrough).
    pub(crate) timestamp_ns: Option<i64>,
}

impl SimpleEncoder {
    /// Handle B-frame reordering and encode.
    ///
    /// For IP-only GOP (no B-frames), this encodes immediately and returns
    /// one packet. For IBBP GOP, B-frames are buffered until their future
    /// reference (P/I/IDR) arrives, then the reference is encoded first
    /// followed by the buffered B-frames.
    pub(crate) unsafe fn submit_frame_reordered(
        &mut self,
        nv12_data: &[u8],
        timestamp_ns: Option<i64>,
    ) -> Result<Vec<EncodePacket>, VideoError> {
        let display_pts = self.frame_counter;
        let frame_type = self.decide_frame_type();

        if frame_type == FrameType::B {
            // Buffer the B-frame — it needs a future reference that hasn't
            // been encoded yet.
            self.reorder_buffer.push(ReorderEntry {
                nv12_data: nv12_data.to_vec(),
                display_pts,
                timestamp_ns,
            });
            self.frame_counter += 1;
            return Ok(Vec::new());
        }

        // Non-B frame (IDR/I/P): encode it first (it's the future reference
        // for any buffered B-frames), then encode the buffered B-frames.
        let mut packets = Vec::new();

        // Encode the reference frame
        let pkt = self.upload_and_encode(nv12_data, frame_type, display_pts, timestamp_ns)?;
        packets.push(pkt);
        self.frame_counter += 1;

        // Now encode buffered B-frames in display order
        let buffered: Vec<ReorderEntry> = self.reorder_buffer.drain(..).collect();
        for entry in buffered {
            let b_pkt = self.upload_and_encode(
                &entry.nv12_data,
                FrameType::B,
                entry.display_pts,
                entry.timestamp_ns,
            )?;
            packets.push(b_pkt);
        }

        Ok(packets)
    }

    /// Flush any remaining frames.
    ///
    /// For IP-only GOP (no B-frames), this is a no-op and returns an empty
    /// vec.  For GOP structures with B-frames, this flushes the reorder
    /// buffer by encoding remaining B-frames as P-frames.
    pub fn finish(&mut self) -> Result<Vec<EncodePacket>, VideoError> {
        // Flush any remaining B-frames in the reorder buffer.
        // These B-frames lack a future reference, so encode them as P-frames
        // (they still have backward references from the DPB).
        let mut packets = Vec::new();
        let buffered: Vec<ReorderEntry> = self.reorder_buffer.drain(..).collect();
        for entry in buffered {
            let pkt = unsafe {
                self.upload_and_encode(&entry.nv12_data, FrameType::P, entry.display_pts, entry.timestamp_ns)?
            };
            packets.push(pkt);
        }
        Ok(packets)
    }

    /// Decide the frame type for the current frame using the GOP structure.
    pub(crate) fn decide_frame_type(&mut self) -> FrameType {
        // Check force_idr override
        if self.force_idr_flag {
            self.force_idr_flag = false;
            // Reset GOP state for new IDR sequence
            self.gop_state = Default::default();
            // Advance past the IDR position so next call continues from 1
            let mut pos = GopPosition::new(0);
            self.gop.get_position_in_gop(
                &mut self.gop_state,
                &mut pos,
                true,
                u32::MAX,
            );
            return FrameType::Idr;
        }

        let first_frame = self.frame_counter == 0;
        let mut pos = GopPosition::new(self.gop_state.position_in_input_order);

        self.gop.get_position_in_gop(
            &mut self.gop_state,
            &mut pos,
            first_frame,
            u32::MAX,
        );

        match pos.picture_type {
            GopFrameType::Idr => FrameType::Idr,
            GopFrameType::I => FrameType::I,
            GopFrameType::P => FrameType::P,
            GopFrameType::B => FrameType::B,
            _ => FrameType::P,
        }
    }
}
