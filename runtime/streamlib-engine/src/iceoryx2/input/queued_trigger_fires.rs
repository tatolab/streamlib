// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The fires queued on unlinked Trigger inputs, and the wake that ends the reactive loop's
//! wait for one, with or without a link into the node.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::{DataChannelPayloadDescriptor, InputMailboxesInner, MessageReadFromInputPort};
use crate::core::error::{Error, Result};
use crate::iceoryx2::input_port_message_payload::InputPortMessagePayload;
use crate::iceoryx2::{DeliveryProfile, MessageGroupBoundary};

/// A Records message holding the empty map, in msgpack: the body of every Trigger message.
const AN_EMPTY_RECORDS_MAP: u8 = 0x80;

/// The fires queued on one unlinked Trigger input.
#[derive(Default)]
pub(super) struct TriggerFiresQueuedOnOneInput {
    /// The stream time of each fire not yet read, oldest first, held to the depth an `ordered`
    /// input holds.
    fired_at_ns: VecDeque<i64>,
    /// The fires dropped from the front of the queue since the last read, told to the next.
    dropped_since_the_last_read: u64,
}

/// What a read of an input finds of the fires queued on it.
pub(super) enum QueuedTriggerFire {
    /// The oldest fire waiting, as the message the read hands over.
    Taken(MessageReadFromInputPort),
    /// None waits on a fired input no link has given a mailbox, so the read finds nothing.
    NoneWaitsOnAnInputWithNoLinkYet,
    /// None waits, and the read goes on to the input's mailbox.
    NeverFired,
}

/// Every unlinked Trigger input's queued fires, by its cast name.
pub(super) type TriggerFiresQueuedByInput = HashMap<String, TriggerFiresQueuedOnOneInput>;

impl InputMailboxesInner {
    /// Queue one fire of the Trigger input `port`, by its cast name, at stream time
    /// `fired_at_ns`, and wake the reactive loop to read it. A queue already holding as many
    /// fires as an `ordered` input holds drops its oldest, which the next read is told of.
    /// Refused while a link feeds the input, whose messages are its only events.
    pub fn queue_a_trigger_fire(&self, port: &str, fired_at_ns: i64) -> Result<()> {
        if self.is_fed_by_a_link(port) {
            return Err(Error::Link(format!(
                "input port '{port}' is fed by a link, whose messages are its only events, so \
                 a fire cannot be queued on it"
            )));
        }
        {
            let mut queued_trigger_fires = self.queued_trigger_fires.lock();
            let queued = queued_trigger_fires.entry(port.to_owned()).or_default();
            if queued.fired_at_ns.len() >= DeliveryProfile::ORDERED_DEPTH {
                queued.fired_at_ns.pop_front();
                queued.dropped_since_the_last_read += 1;
            }
            queued.fired_at_ns.push_back(fired_at_ns);
        }
        self.wake_the_reactive_loop();
        Ok(())
    }

    /// Queue one fire of the Trigger input `port`, by its cast name, stamped with the time of
    /// its stream's clock now, media time until the clock is reached; refused as
    /// [`Self::queue_a_trigger_fire`] refuses.
    pub fn queue_a_trigger_fire_now(&self, port: &str) -> Result<()> {
        self.queue_a_trigger_fire(
            port,
            crate::core::context::stream_time_now_ns_on(self.stream_clock.get().map(Arc::as_ref)),
        )
    }

    /// The next fire queued on `port` as the Trigger message a link would deliver, else
    /// whether it was ever fired.
    pub(super) fn take_a_queued_trigger_fire(&self, port: &str) -> QueuedTriggerFire {
        let mut queued_trigger_fires = self.queued_trigger_fires.lock();
        let Some(queued) = queued_trigger_fires.get_mut(port) else {
            return QueuedTriggerFire::NeverFired;
        };
        let Some(fired_at_ns) = queued.fired_at_ns.pop_front() else {
            drop(queued_trigger_fires);
            return if self.has_a_mailbox(port) {
                QueuedTriggerFire::NeverFired
            } else {
                QueuedTriggerFire::NoneWaitsOnAnInputWithNoLinkYet
            };
        };
        QueuedTriggerFire::Taken(MessageReadFromInputPort {
            payload: InputPortMessagePayload::OwnedBytes(vec![AN_EMPTY_RECORDS_MAP]),
            timestamp_ns: fired_at_ns,
            group_boundary: MessageGroupBoundary::StartsGroup,
            lost_since_last_read: std::mem::take(&mut queued.dropped_since_the_last_read),
            payload_descriptor: DataChannelPayloadDescriptor::records_message(),
        })
    }

    /// Whether `port` was ever fired, which makes it an input even while no link has given it
    /// a mailbox.
    pub(super) fn was_ever_fired(&self, port: &str) -> bool {
        self.queued_trigger_fires.lock().contains_key(port)
    }

    /// Whether a fire waits on `port`.
    pub(super) fn a_trigger_fire_is_queued_on(&self, port: &str) -> bool {
        self.queued_trigger_fires
            .lock()
            .get(port)
            .is_some_and(|queued| !queued.fired_at_ns.is_empty())
    }

    /// Whether a fire waits on any input.
    pub(super) fn a_trigger_fire_is_queued(&self) -> bool {
        self.queued_trigger_fires
            .lock()
            .values()
            .any(|queued| !queued.fired_at_ns.is_empty())
    }
}
