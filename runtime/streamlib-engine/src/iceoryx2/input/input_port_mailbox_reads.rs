// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::de::DeserializeOwned;

use crate::core::error::{Error, Result};
use crate::iceoryx2::channel_name::InboundLinkName;
use crate::iceoryx2::input_port_message_payload::{BorrowedChannelSample, InputPortMessagePayload};
use crate::iceoryx2::loss_counters::InboundLinkDroppedBagCounter;
use crate::iceoryx2::read_mode::ReadMode;

use super::queued_trigger_fires::QueuedTriggerFire;
use super::{
    BagBodyForTheReader, DataChannelPayloadDescriptorKind, InputMailboxesInner,
    InstalledInputPortAudioWindowing, MessageReadFromInputPort, SharedAudioWindowStage,
    WindowedPortStageInstallation, deserialize_msgpack_payload, unknown_input_port,
};

/// Where a bag taken off a port's mailbox goes.
#[derive(Clone, Copy)]
enum WhereABagTakenOffTheMailboxGoes<'stage> {
    /// To the reader, told of the losses ahead of it.
    ToTheReader,
    /// Into the windowed port's stage, while it is still the stage installed;
    /// the window cut from the bag is told of the losses instead.
    IntoTheStageWhileItIsInstalled(&'stage SharedAudioWindowStage),
}

impl InputMailboxesInner {
    /// Receive all pending samples from every channel subscriber and queue them
    /// on mailboxes by the subscriber's local-port binding.
    ///
    /// A plain port queues the sample itself, so a read borrows it in place. A
    /// windowed port queues a copy, because its stage may hold more blocks than
    /// a link may borrow.
    ///
    /// Every loss this pass can see is counted on the link it happened to: the
    /// bags a jump in the sequence number says the ring overwrote, on an
    /// `ordered` port — a `newest` port passing over bags is the profile working
    /// — a sample bound to a port with no mailbox, and the newest sample of a
    /// link already holding its port's mailbox depth plus borrow quota, each of
    /// which consumed a number and reaches no reader. The messages a writer
    /// declared lost upstream of it are told to an `ordered` port's reader and
    /// counted on no link.
    ///
    /// This is called automatically by `read()` and `has_data()`, but can be
    /// called explicitly if needed.
    pub fn receive_pending(&self) {
        let mut subscribers_and_listener = self.inbound_link_subscribers_and_listener.lock();
        for bound in &mut subscribers_and_listener.subscribers {
            loop {
                let sample = match bound.subscriber.receive() {
                    Ok(Some(sample)) => sample,
                    Ok(None) => break, // no more samples on this subscriber
                    Err(iceoryx2::port::ReceiveError::ExceedsMaxBorrows) => {
                        tracing::trace!(
                            link = %bound.inbound_link_name,
                            "InputMailboxes: the link holds every sample its service lets one \
                             subscriber borrow; the rest wait in its ring"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::error!("InputMailboxes: subscriber.receive() FAILED: {:?}", e);
                        break;
                    }
                };
                let mut channel_user_header = *sample.user_header();
                let bags_the_ring_overwrote =
                    bound.bags_the_ring_overwrote.how_many_were_lost_before(
                        sample.origin(),
                        channel_user_header.sequence_number,
                    );
                let borrowed =
                    BorrowedChannelSample::holding(sample, &bound.samples_this_link_holds);
                let mut ports = self.ports.lock();
                let port_config = ports.get_mut(&bound.local_port);
                if port_config
                    .as_ref()
                    .is_some_and(|port| port.mailbox.read_mode().a_bag_passed_over_is_lost())
                {
                    if bags_the_ring_overwrote > 0 {
                        bound
                            .dropped_bag_counter
                            .record_dropped_bags(bags_the_ring_overwrote);
                    }
                    if channel_user_header.messages_lost_upstream_before_this_one > 0 {
                        bound
                            .dropped_bag_counter
                            .record_messages_the_writer_declared_lost_upstream(
                                channel_user_header.messages_lost_upstream_before_this_one,
                            );
                    }
                }
                let Some(port_config) = port_config else {
                    bound.dropped_bag_counter.record_one_dropped_bag();
                    tracing::warn!(
                        port = %bound.local_port,
                        link = %bound.inbound_link_name,
                        "InputMailboxes: channel delivered a frame but its bound local port has \
                         no mailbox; dropped and counted"
                    );
                    continue;
                };
                let members_at_the_read = &bound.bundle_members_at_the_read;
                let payload = if members_at_the_read.reads_a_member() {
                    let area = borrowed.sample_payload_area_bytes();
                    let whole_payload = channel_user_header.payload_within(area).unwrap_or(area);
                    match members_at_the_read.the_message_the_port_reads(
                        &channel_user_header,
                        whole_payload,
                        &mut port_config.newest_value_of_each_member_put_at_the_read,
                    ) {
                        Ok(Some((member_header, member_payload))) => {
                            channel_user_header = member_header;
                            InputPortMessagePayload::OwnedBytes(member_payload)
                        }
                        Ok(None) => {
                            tracing::trace!(
                                port = %bound.local_port,
                                link = %bound.inbound_link_name,
                                "InputMailboxes: the bundle this port reads still lacks a member \
                                 that declares no default, so it hands its reader nothing yet"
                            );
                            continue;
                        }
                        Err(refusal) => {
                            bound.dropped_bag_counter.record_one_dropped_bag();
                            tracing::warn!(
                                port = %bound.local_port,
                                link = %bound.inbound_link_name,
                                "InputMailboxes: the link reads members of a bundle, and this \
                                 message gives none it reads, so it is dropped and counted: \
                                 {refusal}"
                            );
                            continue;
                        }
                    }
                } else if port_config.audio_windowing.is_windowed() {
                    let area = borrowed.sample_payload_area_bytes();
                    InputPortMessagePayload::OwnedBytes(
                        channel_user_header
                            .payload_within(area)
                            .unwrap_or(area)
                            .to_vec(),
                    )
                } else {
                    let most_samples_the_link_may_hold =
                        port_config.mailbox.capacity() + port_config.borrow_quota;
                    if bound.samples_this_link_holds.held_sample_count()
                        > most_samples_the_link_may_hold
                    {
                        bound.dropped_bag_counter.record_one_dropped_bag();
                        tracing::trace!(
                            port = %bound.local_port,
                            link = %bound.inbound_link_name,
                            borrow_quota = port_config.borrow_quota,
                            "InputMailboxes: the link's reader holds past its borrow quota; the \
                             newest frame is dropped and counted"
                        );
                        continue;
                    }
                    InputPortMessagePayload::BorrowedChannelSample(borrowed)
                };
                port_config.mailbox.push_frame_from_inbound_link(
                    payload,
                    channel_user_header,
                    &bound.dropped_bag_counter,
                    &bound.inbound_link_name,
                );
            }
        }
    }

    /// The next bag body `port` hands a reader, following its read mode, or
    /// `None` when there is none — the one read seam every reader shares, and
    /// where a windowed port's stage cuts its windows.
    fn next_bag_for_the_reader(&self, port: &str) -> Result<Option<BagBodyForTheReader>> {
        self.receive_pending();

        let audio_windowing = self
            .ports
            .lock()
            .get(port)
            .map(|port_config| port_config.audio_windowing.clone())
            .ok_or_else(|| unknown_input_port(port))?;

        // The stage's decode, channel convert, resample and framing all run
        // here, with the `ports` mutex released — it guards the port map, and a
        // resampler pass is not port-map work.
        match audio_windowing {
            InstalledInputPortAudioWindowing::NotWindowed => {
                self.pop_one_bag_off_the_mailbox(port, WhereABagTakenOffTheMailboxGoes::ToTheReader)
            }
            // Nothing, and deliberately not the bag underneath: a port whose
            // contract is unsettled has no exact size to cut one to.
            InstalledInputPortAudioWindowing::AwaitingItsDeviceStreamFormat => Ok(None),
            InstalledInputPortAudioWindowing::Windowed(stage) => {
                self.next_window_out_of_the_stage(port, &stage)
            }
        }
    }

    /// Pop the next queued frame for `port` per its read mode and strip its
    /// header, or `None` when the mailbox is empty — or, bound for a stage, when
    /// that stage is no longer the one installed on the port, so a read that
    /// outlived a rewire takes none of the new wiring's bags.
    ///
    /// A bag bound for the reader is told of its losses under the port map
    /// lock, the lock every push and pop of a mailbox runs under, so no
    /// eviction or other pop lands between the pop and the telling.
    fn pop_one_bag_off_the_mailbox(
        &self,
        port: &str,
        where_the_bag_goes: WhereABagTakenOffTheMailboxGoes<'_>,
    ) -> Result<Option<BagBodyForTheReader>> {
        let (delivered, lost_since_last_read) = {
            let mut ports = self.ports.lock();
            let port_config = ports
                .get_mut(port)
                .ok_or_else(|| unknown_input_port(port))?;
            if let WhereABagTakenOffTheMailboxGoes::IntoTheStageWhileItIsInstalled(stage) =
                where_the_bag_goes
                && !port_config.audio_windowing.installs_stage(stage)
            {
                return Ok(None);
            }
            let popped = match port_config.mailbox.read_mode() {
                ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
                ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
            };
            let Some(delivered) = popped else {
                return Ok(None);
            };
            let lost_since_last_read = match where_the_bag_goes {
                WhereABagTakenOffTheMailboxGoes::ToTheReader => {
                    port_config.dropped_bag_counter_of_the_bag_last_handed_to_the_reader =
                        delivered.dropped_bag_counter.clone();
                    delivered.dropped_bag_counter.as_ref().map_or(
                        0,
                        InboundLinkDroppedBagCounter::take_lost_messages_the_reader_has_not_been_told_of,
                    )
                }
                WhereABagTakenOffTheMailboxGoes::IntoTheStageWhileItIsInstalled(_) => 0,
            };
            (delivered, lost_since_last_read)
        };
        let inbound_link_name = delivered.inbound_link_name;
        let channel_user_header = delivered.channel_user_header;
        let payload_area_byte_len = match &delivered.payload {
            InputPortMessagePayload::BorrowedChannelSample(borrowed) => {
                borrowed.sample_payload_area_bytes().len()
            }
            InputPortMessagePayload::OwnedBytes(bytes) => bytes.len(),
        };
        let stamped_payload_bytes = channel_user_header.payload_byte_len as usize;
        if stamped_payload_bytes > payload_area_byte_len {
            return Err(Error::ChannelSampleStampsAPayloadPastTheSample {
                port: port.to_string(),
                stamped_payload_bytes,
                available_payload_bytes: payload_area_byte_len,
            });
        }
        match channel_user_header.payload_descriptor.known_kind() {
            Ok(
                DataChannelPayloadDescriptorKind::Bytes
                | DataChannelPayloadDescriptorKind::GpuPoolSlotFrame
                | DataChannelPayloadDescriptorKind::ImageFrame
                | DataChannelPayloadDescriptorKind::ChannelsBlock
                | DataChannelPayloadDescriptorKind::PacketsUnit
                | DataChannelPayloadDescriptorKind::RecordsMessage
                | DataChannelPayloadDescriptorKind::ArrayFrame,
            ) => {}
            Err(refusal) => {
                return Err(Error::Link(format!(
                    "a message on input port '{port}' was refused: {refusal}"
                )));
            }
        }
        let payload = match delivered.payload {
            InputPortMessagePayload::OwnedBytes(mut bytes) => {
                bytes.truncate(stamped_payload_bytes);
                InputPortMessagePayload::OwnedBytes(bytes)
            }
            borrowed => borrowed,
        };
        Ok(Some(BagBodyForTheReader {
            message: MessageReadFromInputPort {
                payload,
                timestamp_ns: channel_user_header.timestamp_ns,
                group_boundary: channel_user_header.group_boundary(),
                lost_since_last_read,
                payload_descriptor: channel_user_header.payload_descriptor,
            },
            inbound_link_name,
        }))
    }

    /// The next full window from a windowed port's stage, feeding it consumed
    /// bags until it can emit one or the mailbox runs dry.
    ///
    /// The emitted window carries the timestamp the stage derived from the
    /// device's own stamp on the source block, not the wire frame's — the
    /// frame header stamps when a bag was published, and a window's first
    /// sample is an instant inside the block it came from.
    pub(super) fn next_window_out_of_the_stage(
        &self,
        port: &str,
        stage: &SharedAudioWindowStage,
    ) -> Result<Option<BagBodyForTheReader>> {
        let WindowedPortStageInstallation::StillInstalled { feeding_link } =
            self.installation_of_a_windowed_ports_stage(port, stage)
        else {
            return Ok(None);
        };
        loop {
            let ready_window = stage.lock().next_ready_window()?;
            if let Some(window) = ready_window {
                let (lost_since_last_read, inbound_link_name) =
                    feeding_link.as_ref().map_or((0, None), |feeding| {
                        (
                            feeding
                                .dropped_bag_counter
                                .take_lost_messages_the_reader_has_not_been_told_of(),
                            Some(feeding.inbound_link_name.clone()),
                        )
                    });
                return Ok(Some(BagBodyForTheReader {
                    message: MessageReadFromInputPort {
                        lost_since_last_read,
                        ..window.message
                    },
                    inbound_link_name,
                }));
            }
            let Some(bag) = self.pop_one_bag_off_the_mailbox(
                port,
                WhereABagTakenOffTheMailboxGoes::IntoTheStageWhileItIsInstalled(stage),
            )?
            else {
                return Ok(None);
            };
            let flush = stage.lock().accept(&bag.message)?;
            if let Some(flush) = flush {
                Self::count_a_flush_on_the_link_feeding(port, feeding_link.as_ref(), flush);
            }
        }
    }

    /// The next message on `port`, its payload untouched, or `None` when the
    /// mailbox is empty. A fire queued on an unlinked Trigger input is read as the Trigger
    /// message a link would deliver, stamped with the stream time it was fired at.
    pub fn read_raw(&self, port: &str) -> Result<Option<MessageReadFromInputPort>> {
        match self.take_a_queued_trigger_fire(port) {
            QueuedTriggerFire::Taken(fire) => return Ok(Some(fire)),
            QueuedTriggerFire::NoneWaitsOnAnInputWithNoLinkYet => return Ok(None),
            QueuedTriggerFire::NeverFired => {}
        }
        if let Some(island_port) = self.function_node_island_feeding(port) {
            return island_port.read_the_island_output(
                self.a_message_waits_at_a_reader_input_port_of(&island_port),
            );
        }
        if let Some(pinned_port) = self.pin_value_read_by_an_unlinked(port) {
            return pinned_port.read_the_island_output(false);
        }
        Ok(self
            .next_bag_for_the_reader(port)?
            .map(|bag_for_the_reader| bag_for_the_reader.message))
    }

    /// The next frame for `port` with the inbound link it arrived on.
    ///
    /// The read a destination fanning in N links uses: the mailbox already
    /// queues every frame holding the link it came in on — that is how an
    /// eviction is charged to the right one — and this hands that identity back
    /// rather than leaving a reader unable to tell its producers apart.
    ///
    /// Bags from one link arrive in the order that link sent them. Nothing is
    /// promised about how two links interleave: that follows the receive pass,
    /// not the stamps, so a reader that needs time order reasons per link.
    ///
    /// A bag no link delivered has no link to name. It is refused by name
    /// rather than given another link's, and the refusal **consumes** it: the
    /// mailbox admits no peek, so a frame is off the queue before its link is
    /// looked at. [`Self::read_raw`] names no link and is the read for such a
    /// bag.
    pub fn read_raw_from_inbound_link(
        &self,
        port: &str,
    ) -> Result<Option<(MessageReadFromInputPort, InboundLinkName)>> {
        if self.function_node_island_feeding(port).is_some() {
            return Err(Error::Link(format!(
                "input port '{port}' is fed by a function node island, evaluated at each read \
                 rather than delivered over a link, so there is no link to name. Read it with \
                 `read_raw`, which names no link"
            )));
        }
        if self.pin_value_read_by_an_unlinked(port).is_some() {
            return Err(Error::Link(format!(
                "input port '{port}' has no link and reads its pin value, so there is no link \
                 to name. Read it with `read_raw`, which names no link"
            )));
        }
        let Some(mut message_body_for_the_reader) = self.next_bag_for_the_reader(port)? else {
            return Ok(None);
        };
        let Some(inbound_link_name) = message_body_for_the_reader.inbound_link_name.take() else {
            return Err(Error::Link(format!(
                "a bag on input port '{port}' arrived with no inbound link behind it, so \
                 there is no link to name and the bag is consumed. Read such a port with \
                 `read_raw`, which names no link"
            )));
        };
        Ok(Some((
            message_body_for_the_reader.message,
            inbound_link_name,
        )))
    }

    /// The next msgpack message on `port` deserialized into `T`, with the inbound
    /// link it arrived on, or `None` when the mailbox is empty.
    ///
    /// `None` rather than an error on an empty mailbox, unlike
    /// [`InputMailboxes::read_msgpack`]: a destination fanning in N links reads
    /// until its port runs dry, and an empty port is that loop ending.
    ///
    /// [`InputMailboxes::read_msgpack`]: super::InputMailboxes::read_msgpack
    pub fn read_msgpack_from_inbound_link<T: DeserializeOwned>(
        &self,
        port: &str,
    ) -> Result<Option<(T, InboundLinkName)>> {
        let Some((message, inbound_link_name)) = self.read_raw_from_inbound_link(port)? else {
            return Ok(None);
        };
        Ok(Some((
            deserialize_msgpack_payload(&message.payload)?,
            inbound_link_name,
        )))
    }

    /// Count the message last read from `port` as lost on the inbound link
    /// that delivered it, once.
    ///
    /// For a reader that took a message and found what it names gone — a frame
    /// its producer recycled before the reader's claim. The loss lands in the
    /// link's dropped count `graph` renders and in the next message's
    /// `lost_since_last_read` from that link, never as an error. Refused by name
    /// when nothing read from the port since its last count came over a link:
    /// no read yet, that message already counted, a bag no link delivered, or a
    /// window.
    pub fn count_the_message_last_read_as_lost(&self, port: &str) -> Result<()> {
        let dropped_bag_counter = self
            .ports
            .lock()
            .get_mut(port)
            .ok_or_else(|| unknown_input_port(port))?
            .dropped_bag_counter_of_the_bag_last_handed_to_the_reader
            .take()
            .ok_or_else(|| {
                Error::Link(format!(
                    "no message read from input port '{port}' since its last counted loss came \
                     over a link, so there is no link to count this loss on — nothing was read, \
                     the message was already counted, no link delivered it, or it was a window"
                ))
            })?;
        dropped_bag_counter.record_one_bag_its_reader_counted_lost_after_taking_it();
        Ok(())
    }
}
