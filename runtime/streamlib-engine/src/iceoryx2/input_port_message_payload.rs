// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The payload of one message read off an input port: the channel sample it
//! arrived in, borrowed in place, or bytes the engine owns.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::node::ChannelDataServiceSample;
use super::payload::{
    DataChannelPayloadDescriptor, DataChannelSampleUserHeader, data_channel_payload_bytes_of,
};

/// The samples one inbound link holds at once, queued in its port's mailbox or
/// held by its reader, shared by every sample of that link.
#[derive(Clone, Debug, Default)]
pub struct SamplesOneInboundLinkHolds {
    held_sample_count: Arc<AtomicUsize>,
}

impl SamplesOneInboundLinkHolds {
    /// How many samples the link holds right now.
    pub fn held_sample_count(&self) -> usize {
        self.held_sample_count.load(Ordering::Acquire)
    }

    fn count_one_more(&self) -> Self {
        self.held_sample_count.fetch_add(1, Ordering::AcqRel);
        self.clone()
    }

    fn count_one_fewer(&self) {
        self.held_sample_count.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One channel sample a link delivered, held in place in shared memory and
/// counted against that link until it drops.
pub struct BorrowedChannelSample {
    sample: ChannelDataServiceSample,
    samples_its_link_holds: SamplesOneInboundLinkHolds,
}

impl BorrowedChannelSample {
    /// Hold `sample`, counted against the link `samples_its_link_holds` counts.
    pub(crate) fn holding(
        sample: ChannelDataServiceSample,
        samples_its_link_holds: &SamplesOneInboundLinkHolds,
    ) -> Self {
        Self {
            sample,
            samples_its_link_holds: samples_its_link_holds.count_one_more(),
        }
    }

    /// The sample's user header.
    pub fn user_header(&self) -> &DataChannelSampleUserHeader {
        self.sample.user_header()
    }

    /// Every byte of the sample's payload area, past the stamped length too.
    pub fn sample_payload_area_bytes(&self) -> &[u8] {
        data_channel_payload_bytes_of(self.sample.payload())
    }
}

impl Drop for BorrowedChannelSample {
    fn drop(&mut self) {
        self.samples_its_link_holds.count_one_fewer();
    }
}

impl std::fmt::Debug for BorrowedChannelSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BorrowedChannelSample")
            .field("user_header", self.user_header())
            .finish_non_exhaustive()
    }
}

/// The payload of one message read off an input port.
#[derive(Debug)]
pub enum InputPortMessagePayload {
    /// The sample the message arrived in, its payload bounded by its header's
    /// stamped length.
    BorrowedChannelSample(BorrowedChannelSample),
    /// Bytes the engine owns: a window it cut, or a copy taken off a reserved
    /// slot.
    OwnedBytes(Vec<u8>),
}

impl InputPortMessagePayload {
    /// The payload's bytes. A borrowed sample's are bounded by the stamped
    /// length, which the read that handed it out checked against the sample.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::BorrowedChannelSample(borrowed) => {
                let area = borrowed.sample_payload_area_bytes();
                &area[..(borrowed.user_header().payload_byte_len as usize).min(area.len())]
            }
            Self::OwnedBytes(bytes) => bytes,
        }
    }

    /// What the payload is: a borrowed sample's descriptor, and
    /// [`DataChannelPayloadDescriptor::bytes`] for owned bytes.
    pub fn payload_descriptor(&self) -> DataChannelPayloadDescriptor {
        match self {
            Self::BorrowedChannelSample(borrowed) => borrowed.user_header().payload_descriptor,
            Self::OwnedBytes(_) => DataChannelPayloadDescriptor::bytes(),
        }
    }

    /// The payload's bytes as an owned copy.
    pub fn to_vec(&self) -> Vec<u8> {
        self.bytes().to_vec()
    }
}

impl std::ops::Deref for InputPortMessagePayload {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes()
    }
}

impl AsRef<[u8]> for InputPortMessagePayload {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

impl Default for InputPortMessagePayload {
    fn default() -> Self {
        Self::OwnedBytes(Vec::new())
    }
}

impl From<Vec<u8>> for InputPortMessagePayload {
    fn from(bytes: Vec<u8>) -> Self {
        Self::OwnedBytes(bytes)
    }
}

impl PartialEq for InputPortMessagePayload {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

impl Eq for InputPortMessagePayload {}

impl PartialEq<[u8]> for InputPortMessagePayload {
    fn eq(&self, other: &[u8]) -> bool {
        self.bytes() == other
    }
}

impl PartialEq<Vec<u8>> for InputPortMessagePayload {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.bytes() == other.as_slice()
    }
}

impl<const N: usize> PartialEq<[u8; N]> for InputPortMessagePayload {
    fn eq(&self, other: &[u8; N]) -> bool {
        self.bytes() == other.as_slice()
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for InputPortMessagePayload {
    fn eq(&self, other: &&[u8; N]) -> bool {
        self.bytes() == other.as_slice()
    }
}

impl PartialEq<&[u8]> for InputPortMessagePayload {
    fn eq(&self, other: &&[u8]) -> bool {
        self.bytes() == *other
    }
}
