// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One bag as a plaintext MoQ object.
//!
//! Wire contract, shared with every subscriber: `u32` little-endian length
//! of the attachment ‖ the [`MeshDataMessageAttachment`] wire bytes ‖ the
//! payload exactly as the Zenoh data path carries it — the producer's bag with
//! its frame header stripped, or a frame's pixel message for a bag naming a
//! surface. The length is written rather than implied so a later, longer
//! attachment still splits at the right byte.

use crate::core::runtime::mesh::mesh_data_message_attachment::MeshDataMessageAttachment;

/// Bytes the attachment length prefix takes.
const ATTACHMENT_LENGTH_PREFIX_BYTES: usize = 4;

/// `attachment` and `payload` as one plaintext MoQ object.
pub(crate) fn a_plaintext_moq_object_carrying(
    attachment: MeshDataMessageAttachment,
    payload: &[u8],
) -> Vec<u8> {
    let attachment_wire_bytes = attachment.to_wire_bytes();
    let mut object = Vec::with_capacity(
        ATTACHMENT_LENGTH_PREFIX_BYTES + attachment_wire_bytes.len() + payload.len(),
    );
    object.extend_from_slice(&(attachment_wire_bytes.len() as u32).to_le_bytes());
    object.extend_from_slice(&attachment_wire_bytes);
    object.extend_from_slice(payload);
    object
}

/// The attachment and payload a plaintext MoQ object carries, or `None` when
/// the bytes are not one.
pub(crate) fn split_a_plaintext_moq_object(
    object: &[u8],
) -> Option<(MeshDataMessageAttachment, &[u8])> {
    let (attachment_length, rest) = object.split_first_chunk::<ATTACHMENT_LENGTH_PREFIX_BYTES>()?;
    let attachment_length = u32::from_le_bytes(*attachment_length) as usize;
    if rest.len() < attachment_length {
        return None;
    }
    let (attachment_wire_bytes, payload) = rest.split_at(attachment_length);
    let attachment = MeshDataMessageAttachment::from_wire_bytes(attachment_wire_bytes)?;
    Some((attachment, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime::mesh::MESH_DATA_MESSAGE_ATTACHMENT_BYTES;
    use crate::core::runtime::mesh::machine_clock_identity::MachineClockIdentity;
    use crate::core::runtime::mesh::mesh_data_message_attachment::PublisherGenerationOnTheMesh;

    fn an_attachment() -> MeshDataMessageAttachment {
        MeshDataMessageAttachment {
            timestamp_ns: 1_726_000_000_123_456_789,
            sequence_number: 42,
            publisher_generation: PublisherGenerationOnTheMesh(1),
            clock_identity: MachineClockIdentity::UNIDENTIFIED,
            frame_pixel_description_bytes: 0,
        }
    }

    #[test]
    fn an_object_is_the_length_then_the_attachment_then_the_payload() {
        let object = a_plaintext_moq_object_carrying(an_attachment(), b"\x81\xa1k\x01");
        assert_eq!(
            &object[..4],
            &(MESH_DATA_MESSAGE_ATTACHMENT_BYTES as u32).to_le_bytes()
        );
        assert_eq!(
            &object[4..4 + MESH_DATA_MESSAGE_ATTACHMENT_BYTES],
            &an_attachment().to_wire_bytes()
        );
        assert_eq!(
            &object[4 + MESH_DATA_MESSAGE_ATTACHMENT_BYTES..],
            b"\x81\xa1k\x01"
        );
    }

    #[test]
    fn an_object_splits_back_into_what_it_carried() {
        let object = a_plaintext_moq_object_carrying(an_attachment(), b"payload bytes");
        let (attachment, payload) = split_a_plaintext_moq_object(&object).expect("it splits");
        assert_eq!(attachment, an_attachment());
        assert_eq!(payload, b"payload bytes");
    }

    #[test]
    fn a_longer_attachment_splits_at_the_length_it_states() {
        let mut attachment_wire_bytes = an_attachment().to_wire_bytes().to_vec();
        attachment_wire_bytes.extend_from_slice(&[0xEE; 8]);
        let mut object = (attachment_wire_bytes.len() as u32).to_le_bytes().to_vec();
        object.extend_from_slice(&attachment_wire_bytes);
        object.extend_from_slice(b"after");
        let (attachment, payload) = split_a_plaintext_moq_object(&object).expect("it splits");
        assert_eq!(attachment, an_attachment());
        assert_eq!(payload, b"after");
    }

    #[test]
    fn bytes_that_are_not_an_object_split_into_nothing() {
        assert!(split_a_plaintext_moq_object(b"").is_none());
        assert!(split_a_plaintext_moq_object(&[44, 0, 0, 0, 1, 2]).is_none());
        assert!(split_a_plaintext_moq_object(&[2, 0, 0, 0, 1, 2, 3]).is_none());
    }
}
