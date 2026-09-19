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

use crate::core::runtime::mesh::machine_clock_identity::{
    MACHINE_CLOCK_IDENTITY_BYTES, MachineClockIdentity,
};

/// Where each field begins, and how many bytes one attachment is on the wire.
///
/// The layout is the wire contract: a peer of any build reads these offsets,
/// and a field added later is appended so none of them moves.
const TIMESTAMP_NS_OFFSET: usize = 0;
const SEQUENCE_NUMBER_OFFSET: usize = TIMESTAMP_NS_OFFSET + size_of::<i64>();
const PUBLISHER_GENERATION_OFFSET: usize = SEQUENCE_NUMBER_OFFSET + size_of::<u64>();
const CLOCK_IDENTITY_OFFSET: usize = PUBLISHER_GENERATION_OFFSET + size_of::<u64>();

/// How many bytes one attachment is on the wire.
pub const MESH_DATA_MESSAGE_ATTACHMENT_BYTES: usize =
    CLOCK_IDENTITY_OFFSET + MACHINE_CLOCK_IDENTITY_BYTES;

/// The fixed record beside every bag a remote link carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshDataMessageAttachment {
    /// The producer's own stamp, off the frame header. Crosses unchanged and is
    /// never compared against a stamp from another clock.
    pub timestamp_ns: i64,
    /// The number the producing publisher gave this bag on its own channel —
    /// the engine's one sequence number, carried end to end rather than
    /// re-minted here, so a gap at the ingress covers the sending runtime's
    /// rings as well as the network.
    pub sequence_number: u64,
    /// How many times the port's publisher has been replaced under this
    /// egress. A bag whose generation differs from the last one's is a
    /// baseline and never a gap, so a recreated producer — whose numbering
    /// restarts — is not read as loss.
    pub publisher_generation: u64,
    /// The machine whose monotonic clock produced `timestamp_ns`.
    pub clock_identity: MachineClockIdentity,
}

impl MeshDataMessageAttachment {
    /// This record on the wire.
    pub fn to_wire_bytes(self) -> [u8; MESH_DATA_MESSAGE_ATTACHMENT_BYTES] {
        let mut wire_bytes = [0u8; MESH_DATA_MESSAGE_ATTACHMENT_BYTES];
        wire_bytes[TIMESTAMP_NS_OFFSET..SEQUENCE_NUMBER_OFFSET]
            .copy_from_slice(&self.timestamp_ns.to_le_bytes());
        wire_bytes[SEQUENCE_NUMBER_OFFSET..PUBLISHER_GENERATION_OFFSET]
            .copy_from_slice(&self.sequence_number.to_le_bytes());
        wire_bytes[PUBLISHER_GENERATION_OFFSET..CLOCK_IDENTITY_OFFSET]
            .copy_from_slice(&self.publisher_generation.to_le_bytes());
        wire_bytes[CLOCK_IDENTITY_OFFSET..].copy_from_slice(&self.clock_identity.to_wire_bytes());
        wire_bytes
    }

    /// Read a record off the wire, or `None` when the bytes are not one — a
    /// peer of another engine version, or a message this engine did not write.
    pub fn from_wire_bytes(wire_bytes: &[u8]) -> Option<Self> {
        let record: &[u8; MESH_DATA_MESSAGE_ATTACHMENT_BYTES] = wire_bytes
            .get(..MESH_DATA_MESSAGE_ATTACHMENT_BYTES)?
            .try_into()
            .ok()?;
        Some(Self {
            timestamp_ns: i64::from_le_bytes(
                record[TIMESTAMP_NS_OFFSET..SEQUENCE_NUMBER_OFFSET]
                    .try_into()
                    .ok()?,
            ),
            sequence_number: u64::from_le_bytes(
                record[SEQUENCE_NUMBER_OFFSET..PUBLISHER_GENERATION_OFFSET]
                    .try_into()
                    .ok()?,
            ),
            publisher_generation: u64::from_le_bytes(
                record[PUBLISHER_GENERATION_OFFSET..CLOCK_IDENTITY_OFFSET]
                    .try_into()
                    .ok()?,
            ),
            clock_identity: MachineClockIdentity::from_wire_bytes(
                record[CLOCK_IDENTITY_OFFSET..].try_into().ok()?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record() -> MeshDataMessageAttachment {
        MeshDataMessageAttachment {
            timestamp_ns: 0x0102_0304_0506_0708,
            sequence_number: 0x1112_1314_1516_1718,
            publisher_generation: 0x2122_2324_2526_2728,
            clock_identity: MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
                "2f1c8a30-6b4e-4d5a-9a11-2c7f0d5e8b93",
            ),
        }
    }

    /// The layout is the wire contract, pinned byte for byte: every field's
    /// offset, its little-endian order, and the record's length. These are the
    /// bytes a peer of any build reads, so a field that moved would be read as
    /// another field's value rather than as a decode failure.
    #[test]
    fn the_attachments_bytes_are_every_field_little_endian_at_its_own_offset() {
        assert_eq!(
            a_record().to_wire_bytes(),
            [
                // timestamp_ns
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, //
                // sequence_number
                0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, //
                // publisher_generation
                0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, //
                // clock_identity, verbatim rather than byte-swapped
                0x2f, 0x1c, 0x8a, 0x30, 0x6b, 0x4e, 0x4d, 0x5a, 0x9a, 0x11, 0x2c, 0x7f, 0x0d, 0x5e,
                0x8b, 0x93,
            ]
        );
        assert_eq!(MESH_DATA_MESSAGE_ATTACHMENT_BYTES, 40);
        assert_eq!(TIMESTAMP_NS_OFFSET, 0);
        assert_eq!(SEQUENCE_NUMBER_OFFSET, 8);
        assert_eq!(PUBLISHER_GENERATION_OFFSET, 16);
        assert_eq!(CLOCK_IDENTITY_OFFSET, 24);
    }

    /// Every value each field can carry survives the wire — the stamp's
    /// negative ones because the frame header's stamp is signed, and both
    /// counters' extremes because each wraps rather than saturating.
    #[test]
    fn every_value_the_record_can_carry_round_trips() {
        for timestamp_ns in [0, 1, -1, i64::MIN, i64::MAX, 1_726_000_000_000_000_000] {
            for counter in [0, 1, u64::MAX] {
                let record = MeshDataMessageAttachment {
                    timestamp_ns,
                    sequence_number: counter,
                    publisher_generation: counter,
                    clock_identity: MachineClockIdentity::of_this_machine(),
                };
                assert_eq!(
                    MeshDataMessageAttachment::from_wire_bytes(&record.to_wire_bytes()),
                    Some(record),
                    "{record:?} must survive the wire"
                );
            }
        }
    }

    /// A machine that names no clock still writes a readable record: the
    /// identity is nil, and nothing else about the bag is affected.
    #[test]
    fn a_record_from_a_machine_that_names_no_clock_still_reads() {
        let record = MeshDataMessageAttachment {
            clock_identity: MachineClockIdentity::UNIDENTIFIED,
            ..a_record()
        };
        let read = MeshDataMessageAttachment::from_wire_bytes(&record.to_wire_bytes());
        assert_eq!(read, Some(record));
        assert!(read.expect("a record").clock_identity.is_unidentified());
    }

    /// Bytes that are not one record read as none rather than as a record with
    /// the fields the bytes ran out of invented.
    #[test]
    fn bytes_that_are_not_one_record_read_as_none() {
        for too_short in [
            0,
            1,
            SEQUENCE_NUMBER_OFFSET,
            PUBLISHER_GENERATION_OFFSET,
            CLOCK_IDENTITY_OFFSET,
            MESH_DATA_MESSAGE_ATTACHMENT_BYTES - 1,
        ] {
            assert_eq!(
                MeshDataMessageAttachment::from_wire_bytes(&vec![0u8; too_short]),
                None,
                "{too_short} bytes must read as no record"
            );
        }
    }

    /// A longer attachment reads the record it begins with, which is what
    /// lets a later field be appended without any of these moving.
    #[test]
    fn a_longer_attachment_still_reads_the_record_it_begins_with() {
        let record = a_record();
        let mut wire_bytes = record.to_wire_bytes().to_vec();
        wire_bytes.extend_from_slice(&[0xAA; 16]);
        assert_eq!(
            MeshDataMessageAttachment::from_wire_bytes(&wire_bytes),
            Some(record)
        );
    }
}
