// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A reader on a channel's reserved slot from any process, engine or not: each
//! bag's payload, stamp and group flag, with every message lost ahead of it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iceoryx2::node::Node;
use iceoryx2::prelude::*;

use crate::engine_owned_iceoryx2_domain::{
    ChannelDataServiceOpenError, ChannelDataServicePortFactory, ChannelDataServiceSubscriber,
    ChannelReservedSlotSubscribeError, Iceoryx2DomainRefusal, channel_data_service_builder,
    create_iceoryx2_node_in_domain, create_the_reserved_slot_subscriber_of,
    engine_owned_iceoryx2_prefix_for_this_user,
};
use crate::{
    BagsAGapInTheNumberingSaysWereLost, DataChannelPayloadDescriptor, DataChannelSampleUserHeader,
    MessageGroupBoundary, data_channel_payload_bytes_of,
};

/// The ring a viewer's subscriber takes: one sample, so a viewer always reads
/// the latest message and the channel overwrites what it fell behind on.
pub const OUTPUT_PORT_VIEWER_RING_DEPTH: usize = 1;

/// The most samples one viewer read takes off its ring before answering.
pub const SAMPLES_ONE_VIEWER_READ_TAKES_AT_MOST: usize = 64;

/// One message read off a channel's reserved slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageReadFromAReservedSlot {
    /// The payload exactly as the producer wrote it.
    pub payload: Vec<u8>,
    /// The publish stamp.
    pub timestamp_ns: i64,
    /// Whether the message opens a group.
    pub group_boundary: MessageGroupBoundary,
    /// The messages lost between the previous message read and this one.
    pub lost_since_last_read: u64,
    /// What the payload is: its bytes, or a pooled GPU frame the reader claims.
    pub payload_descriptor: DataChannelPayloadDescriptor,
}

/// A reserved-slot reader's messages: each bag's payload, stamp and group
/// flag, with every message lost ahead of it — overwritten in the ring,
/// unreadable, passed over by the reader, or declared lost upstream by its
/// writer — told on the next message read. A run is one publisher's unbroken
/// numbering.
pub struct LossTallyOfAReservedSlotReader<OneUnbrokenRun> {
    bags_the_ring_overwrote: BagsAGapInTheNumberingSaysWereLost<OneUnbrokenRun>,
    lost_ahead_of_the_next_message_read: u64,
}

impl<OneUnbrokenRun> Default for LossTallyOfAReservedSlotReader<OneUnbrokenRun> {
    fn default() -> Self {
        Self {
            bags_the_ring_overwrote: BagsAGapInTheNumberingSaysWereLost::default(),
            lost_ahead_of_the_next_message_read: 0,
        }
    }
}

impl<OneUnbrokenRun: Copy + PartialEq> LossTallyOfAReservedSlotReader<OneUnbrokenRun> {
    /// The message a bag of `run` carries, or `None` when its stamped payload
    /// does not fit the sample, which counts as lost.
    pub fn message_from_a_bag_headed(
        &mut self,
        run: OneUnbrokenRun,
        channel_user_header: DataChannelSampleUserHeader,
        sample_payload_area: &[u8],
    ) -> Option<MessageReadFromAReservedSlot> {
        self.lost_ahead_of_the_next_message_read = self
            .lost_ahead_of_the_next_message_read
            .saturating_add(
                self.bags_the_ring_overwrote
                    .how_many_were_lost_before(run, channel_user_header.sequence_number),
            )
            .saturating_add(channel_user_header.messages_lost_upstream_before_this_one);
        let Ok(payload) = channel_user_header.payload_within(sample_payload_area) else {
            self.lost_ahead_of_the_next_message_read =
                self.lost_ahead_of_the_next_message_read.saturating_add(1);
            return None;
        };
        Some(MessageReadFromAReservedSlot {
            payload: payload.to_vec(),
            timestamp_ns: channel_user_header.timestamp_ns,
            group_boundary: channel_user_header.group_boundary(),
            lost_since_last_read: std::mem::take(&mut self.lost_ahead_of_the_next_message_read),
            payload_descriptor: channel_user_header.payload_descriptor,
        })
    }

    /// Count a message read but never delivered, with the losses it carried,
    /// against the next message read.
    pub fn count_a_message_never_delivered(&mut self, its_lost_since_last_read: u64) {
        self.lost_ahead_of_the_next_message_read = self
            .lost_ahead_of_the_next_message_read
            .saturating_add(its_lost_since_last_read.saturating_add(1));
    }
}

/// Why a viewer could not subscribe to a channel's reserved slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPortViewerSubscribeRefusal {
    /// The iceoryx2 domain or the viewer's node was refused.
    Domain(Iceoryx2DomainRefusal),
    /// The channel is not open: the viewer was not opened, or was closed.
    ChannelNotOpen(String),
    /// Another reader holds the channel's reserved slot.
    ReservedSlotOccupied(String),
    /// Any other iceoryx2 failure, rendered for the log.
    Transport(String),
}

impl std::fmt::Display for OutputPortViewerSubscribeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(refusal) => write!(formatter, "{refusal}"),
            Self::ChannelNotOpen(channel) => write!(
                formatter,
                "channel `{channel}` is not open, so a viewer has nothing to read: open the \
                 viewer first, and keep it open while it reads"
            ),
            Self::ReservedSlotOccupied(channel) => write!(
                formatter,
                "the reserved slot of channel `{channel}` is held by another reader"
            ),
            Self::Transport(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for OutputPortViewerSubscribeRefusal {}

/// A viewer's subscriber on an output port's channel, in the viewer's own
/// process: each read answers the latest message the port's node wrote and how
/// many it skipped since the read before, and a wait sleeps on the port's wake
/// until the node writes.
pub struct OutputPortViewerReader {
    subscriber: ChannelDataServiceSubscriber,
    wake: OutputPortViewerWake,
    loss_tally: LossTallyOfAReservedSlotReader<iceoryx2::identifiers::UniquePublisherId>,
    _channel_service: ChannelDataServicePortFactory,
    _iceoryx2_node: Node<ipc_threadsafe::Service>,
}

impl OutputPortViewerReader {
    /// Subscribe to the reserved slot of channel `channel_service_name` in the
    /// iceoryx2 domain rooted at `iceoryx2_domain_root`, and listen on the
    /// notify service `wake_service_name` the port's node wakes on each write,
    /// from an iceoryx2 node of this process named `viewer_node_name`.
    ///
    /// iceoryx2 binds the wake's listener socket under a process-wide umask
    /// for an instant; a child process the viewer's process starts in that
    /// instant inherits it.
    pub fn subscribe(
        iceoryx2_domain_root: &Path,
        channel_service_name: &str,
        wake_service_name: &str,
        viewer_node_name: &str,
    ) -> Result<Self, OutputPortViewerSubscribeRefusal> {
        let iceoryx2_node = create_iceoryx2_node_in_domain(
            iceoryx2_domain_root,
            &engine_owned_iceoryx2_prefix_for_this_user(),
            viewer_node_name,
        )
        .map_err(OutputPortViewerSubscribeRefusal::Domain)?;
        let service_name = ServiceName::new(channel_service_name).map_err(|refusal| {
            OutputPortViewerSubscribeRefusal::Transport(format!(
                "`{channel_service_name}` is not an iceoryx2 service name: {refusal:?}"
            ))
        })?;
        let channel_service =
            match channel_data_service_builder(&iceoryx2_node, &service_name).open() {
                Ok(channel_service) => channel_service,
                Err(
                    ChannelDataServiceOpenError::DoesNotExist
                    | ChannelDataServiceOpenError::IsMarkedForDestruction,
                ) => {
                    return Err(OutputPortViewerSubscribeRefusal::ChannelNotOpen(
                        channel_service_name.to_string(),
                    ));
                }
                Err(failure) => {
                    return Err(OutputPortViewerSubscribeRefusal::Transport(format!(
                        "a viewer could not open channel `{channel_service_name}`: {failure:?}"
                    )));
                }
            };
        let subscriber =
            create_the_reserved_slot_subscriber_of(&channel_service, OUTPUT_PORT_VIEWER_RING_DEPTH)
                .map_err(|refusal| match refusal {
                    ChannelReservedSlotSubscribeError::ReservedSlotOccupied => {
                        OutputPortViewerSubscribeRefusal::ReservedSlotOccupied(
                            channel_service_name.to_string(),
                        )
                    }
                    ChannelReservedSlotSubscribeError::Transport(detail) => {
                        OutputPortViewerSubscribeRefusal::Transport(format!(
                            "a viewer could not subscribe to the reserved slot of channel \
                             `{channel_service_name}`: {detail}"
                        ))
                    }
                })?;
        let wake_service_name_for_iceoryx2 =
            ServiceName::new(wake_service_name).map_err(|refusal| {
                OutputPortViewerSubscribeRefusal::Transport(format!(
                    "`{wake_service_name}` is not an iceoryx2 service name: {refusal:?}"
                ))
            })?;
        let wake_service = iceoryx2_node
            .service_builder(&wake_service_name_for_iceoryx2)
            .event()
            .open()
            .map_err(|failure| {
                OutputPortViewerSubscribeRefusal::Transport(format!(
                    "a viewer could not open the wake `{wake_service_name}` of channel \
                     `{channel_service_name}`: {failure:?}"
                ))
            })?;
        let wake_listener = wake_service
            .listener_builder()
            .create()
            .map_err(|failure| {
                OutputPortViewerSubscribeRefusal::Transport(format!(
                    "a viewer could not listen on the wake `{wake_service_name}` of channel \
                 `{channel_service_name}`: {failure:?}"
                ))
            })?;
        Ok(Self {
            subscriber,
            wake: OutputPortViewerWake {
                listener_and_its_service: Arc::new((wake_listener, wake_service)),
            },
            loss_tally: LossTallyOfAReservedSlotReader::default(),
            _channel_service: channel_service,
            _iceoryx2_node: iceoryx2_node,
        })
    }

    /// The latest message the port's node wrote since the read before, or
    /// `None` when it wrote nothing since. Its `lost_since_last_read` counts
    /// every message written in between that the viewer did not read. One read
    /// takes at most [`SAMPLES_ONE_VIEWER_READ_TAKES_AT_MOST`] samples, so a
    /// writer that never pauses cannot hold it.
    pub fn read_the_latest_message(
        &mut self,
    ) -> Result<Option<MessageReadFromAReservedSlot>, String> {
        let mut latest: Option<MessageReadFromAReservedSlot> = None;
        for _ in 0..SAMPLES_ONE_VIEWER_READ_TAKES_AT_MOST {
            let sample = self.subscriber.receive().map_err(|receive_failure| {
                format!("a viewer's subscriber failed to receive: {receive_failure:?}")
            })?;
            let Some(sample) = sample else {
                break;
            };
            let Some(mut message) = self.loss_tally.message_from_a_bag_headed(
                sample.origin(),
                *sample.user_header(),
                data_channel_payload_bytes_of(sample.payload()),
            ) else {
                continue;
            };
            if let Some(passed_over) = latest.take() {
                message.lost_since_last_read = message
                    .lost_since_last_read
                    .saturating_add(passed_over.lost_since_last_read.saturating_add(1));
            }
            latest = Some(message);
        }
        Ok(latest)
    }

    /// The wake the port's node notifies on each write, which another thread
    /// may wait on while this reader is held elsewhere.
    pub fn wake(&self) -> OutputPortViewerWake {
        self.wake.clone()
    }
}

/// The wake of one viewer's port: the port's node notifies it on each write.
#[derive(Clone)]
pub struct OutputPortViewerWake {
    listener_and_its_service: Arc<(
        iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
        iceoryx2::service::port_factory::event::PortFactory<ipc_threadsafe::Service>,
    )>,
}

impl OutputPortViewerWake {
    /// Wait up to `at_most` for the port's node to write, answering whether it
    /// wrote since the wait before; every write already told is taken with it.
    pub fn wait_for_a_write(&self, at_most: Duration) -> Result<bool, String> {
        let (listener, _wake_service) = &*self.listener_and_its_service;
        let mut writes_told = 0usize;
        listener
            .timed_wait(|_| writes_told += 1, at_most)
            .map_err(|wait_failure| format!("a viewer's wake failed: {wait_failure:?}"))?;
        listener
            .try_wait(|_| writes_told += 1)
            .map_err(|wait_failure| format!("a viewer's wake failed: {wait_failure:?}"))?;
        Ok(writes_told > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_message(payload: &[u8], lost_since_last_read: u64) -> MessageReadFromAReservedSlot {
        MessageReadFromAReservedSlot {
            payload: payload.to_vec(),
            timestamp_ns: 7,
            group_boundary: MessageGroupBoundary::StartsGroup,
            lost_since_last_read,
            payload_descriptor: DataChannelPayloadDescriptor::bytes(),
        }
    }

    /// A channel user header numbered `sequence_number`, stamping 7 and
    /// `stamped_payload_bytes`, starting a group, with no declared loss.
    fn numbered_stamping(
        sequence_number: u64,
        stamped_payload_bytes: usize,
    ) -> DataChannelSampleUserHeader {
        DataChannelSampleUserHeader {
            sequence_number,
            ..DataChannelSampleUserHeader::new(
                7,
                stamped_payload_bytes as u32,
                MessageGroupBoundary::StartsGroup,
                DataChannelPayloadDescriptor::bytes(),
            )
        }
    }

    /// A gap in the numbering, an unreadable bag and a message never delivered
    /// are each told on the next message read.
    #[test]
    fn the_tally_tells_a_gap_an_unreadable_bag_and_an_undelivered_message_on_the_next_message() {
        let mut tally = LossTallyOfAReservedSlotReader::<u64>::default();
        let run = 1u64;
        assert_eq!(
            tally.message_from_a_bag_headed(run, numbered_stamping(0, 1), b"a"),
            Some(a_message(b"a", 0))
        );
        assert!(
            tally
                .message_from_a_bag_headed(run, numbered_stamping(3, 8), &[0u8; 4])
                .is_none()
        );
        let never_delivered = tally
            .message_from_a_bag_headed(run, numbered_stamping(4, 1), b"b")
            .unwrap();
        assert_eq!(
            never_delivered.lost_since_last_read, 3,
            "two overwritten, one unreadable"
        );
        tally.count_a_message_never_delivered(never_delivered.lost_since_last_read);
        assert_eq!(
            tally
                .message_from_a_bag_headed(run, numbered_stamping(5, 1), b"c")
                .unwrap()
                .lost_since_last_read,
            4,
            "the undelivered message and the losses it carried"
        );
    }

    /// A loss the writer declared upstream is told on the message carrying it,
    /// even when that message is the run's first.
    #[test]
    fn the_tally_tells_a_declared_upstream_loss_on_the_message_carrying_it() {
        let mut tally = LossTallyOfAReservedSlotReader::<u64>::default();
        let run = 1u64;
        let first_carrying_two_lost_upstream = DataChannelSampleUserHeader {
            messages_lost_upstream_before_this_one: 2,
            ..numbered_stamping(0, 1)
        };
        assert_eq!(
            tally
                .message_from_a_bag_headed(run, first_carrying_two_lost_upstream, b"a")
                .unwrap()
                .lost_since_last_read,
            2
        );
        assert_eq!(
            tally
                .message_from_a_bag_headed(run, numbered_stamping(1, 1), b"b")
                .unwrap()
                .lost_since_last_read,
            0,
            "a declared loss is told once"
        );
    }
}
