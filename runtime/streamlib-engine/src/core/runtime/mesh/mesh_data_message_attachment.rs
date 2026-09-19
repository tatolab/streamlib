// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What rides beside a bag on the mesh.
//!
//! The payload is the bag's bytes exactly as the producer wrote them, so
//! nothing the engine adds may be mixed into it. Everything the receiving
//! runtime needs and the bag does not carry rides in the message's attachment
//! instead, as a fixed little-endian record rather than a map: it is read once
//! per bag on the ingress thread, and a self-describing encoding would cost a
//! decode where the shape never varies.
//!
//! The one field today is the frame header's stamp, which crosses unchanged.

/// How many bytes one attachment is on the wire.
pub const MESH_DATA_MESSAGE_ATTACHMENT_BYTES: usize = 8;

/// The fixed record beside every bag a remote link carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshDataMessageAttachment {
    /// The producer's own stamp, off the frame header. Crosses unchanged and is
    /// never compared against a stamp from another clock.
    pub timestamp_ns: i64,
}

impl MeshDataMessageAttachment {
    /// This record on the wire.
    pub fn to_wire_bytes(self) -> [u8; MESH_DATA_MESSAGE_ATTACHMENT_BYTES] {
        self.timestamp_ns.to_le_bytes()
    }

    /// Read a record off the wire, or `None` when the bytes are not one — a
    /// peer of another engine version, or a message this engine did not write.
    pub fn from_wire_bytes(wire_bytes: &[u8]) -> Option<Self> {
        let stamp: [u8; MESH_DATA_MESSAGE_ATTACHMENT_BYTES] = wire_bytes
            .get(..MESH_DATA_MESSAGE_ATTACHMENT_BYTES)?
            .try_into()
            .ok()?;
        Some(Self {
            timestamp_ns: i64::from_le_bytes(stamp),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout is the wire contract: the stamp is eight little-endian bytes
    /// at offset zero, and these are the bytes a peer of any build reads.
    #[test]
    fn the_attachments_bytes_are_the_stamp_little_endian_at_offset_zero() {
        let attached = MeshDataMessageAttachment {
            timestamp_ns: 0x0102_0304_0506_0708,
        };
        assert_eq!(
            attached.to_wire_bytes(),
            [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(
            MESH_DATA_MESSAGE_ATTACHMENT_BYTES,
            std::mem::size_of::<i64>()
        );
    }

    /// Every stamp the frame header can carry survives the wire, negative ones
    /// included — the header's stamp is signed.
    #[test]
    fn every_stamp_the_frame_header_can_carry_round_trips() {
        for timestamp_ns in [0, 1, -1, i64::MIN, i64::MAX, 1_726_000_000_000_000_000] {
            let attached = MeshDataMessageAttachment { timestamp_ns };
            assert_eq!(
                MeshDataMessageAttachment::from_wire_bytes(&attached.to_wire_bytes()),
                Some(attached),
                "{timestamp_ns} must survive the wire"
            );
        }
    }

    /// Bytes that are not one record read as none rather than as a stamp with
    /// half its bytes invented.
    #[test]
    fn bytes_that_are_not_one_record_read_as_none() {
        for too_short in [&[][..], &[0u8; 1][..], &[0u8; 7][..]] {
            assert_eq!(MeshDataMessageAttachment::from_wire_bytes(too_short), None);
        }
    }

    /// A longer attachment reads its first eight bytes, which is what lets a
    /// later field be appended without the stamp moving.
    #[test]
    fn a_longer_attachment_still_reads_the_stamp_it_begins_with() {
        let mut wire_bytes = MeshDataMessageAttachment { timestamp_ns: 42 }
            .to_wire_bytes()
            .to_vec();
        wire_bytes.extend_from_slice(&[0xAA; 16]);
        assert_eq!(
            MeshDataMessageAttachment::from_wire_bytes(&wire_bytes),
            Some(MeshDataMessageAttachment { timestamp_ns: 42 })
        );
    }
}
