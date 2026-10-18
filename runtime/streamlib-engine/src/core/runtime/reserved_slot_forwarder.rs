// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The reader of a channel's reserved subscriber slot: a dedicated thread that
//! holds the slot's subscriber and hands each bag, made into whatever its
//! [`ReservedSlotBagForwarding`] makes of it, to an async receiver.
//!
//! iceoryx2's `Subscriber` holds an `Rc` and is `!Send`, so it lives on its own
//! OS thread and never moves into a tokio task.
//!
//! The thread always drains the slot and never blocks on its receiver: it
//! forwards with `try_send` over a bounded channel and, when that channel is
//! full, hands the item back to its forwarding as dropped rather than parking.
//! A parked thread would stop draining the slot, so the bags after a stall
//! would reach no one and dropping the forwarder would hang on a thread that
//! never returns.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use iceoryx2::identifiers::UniquePublisherId;

use crate::core::error::{Error, Result};
use crate::iceoryx2::{
    ChannelIdlePollBackoff, ChannelReservedSlotSubscribeError, ChannelSizing, Iceoryx2Node,
};

/// One bag as the reserved slot received it.
pub(crate) struct BagReceivedOnTheReservedSlot<'sample> {
    /// The publisher that sent it, whose numbering is one unbroken run.
    pub(crate) publisher: UniquePublisherId,
    /// The number its publisher gave it.
    pub(crate) sequence_number: u64,
    /// The bag as published: frame header, then payload.
    pub(crate) framed_bag: &'sample [u8],
}

/// What a reserved-slot forwarder makes of each bag it receives, and what it
/// does with an item its full receiver had no room for.
pub(crate) trait ReservedSlotBagForwarding: Send + 'static {
    /// What one bag becomes on its way to the receiver.
    type Forwarded: Send + 'static;

    /// The item `bag` forwards, or `None` for a bag that forwards nothing.
    fn forwarded_from(&mut self, bag: BagReceivedOnTheReservedSlot<'_>) -> Option<Self::Forwarded>;

    /// `dropped`, which the receiver had no room for.
    fn dropped_into_a_full_receiver(&mut self, dropped: Self::Forwarded, channel: &str);
}

/// The reserved slot of one channel, as the compiler opened the channel; the
/// ring its subscriber takes, no deeper than the channel; and how many items
/// to forward from it before ending, or `None` for no bound.
pub(crate) struct ReservedSlotOfAChannelToForward {
    pub(crate) node: Iceoryx2Node,
    pub(crate) channel: String,
    pub(crate) sizing: ChannelSizing,
    pub(crate) ring_depth: usize,
    pub(crate) count: Option<usize>,
}

/// A running reader of one channel's reserved slot, handing what it forwards
/// to [`Self::recv`]. Dropping it stops and joins its thread, freeing the slot.
#[derive(Debug)]
pub(crate) struct ReservedSlotForwarder<Forwarded> {
    channel: String,
    receiver: tokio::sync::mpsc::Receiver<Forwarded>,
    stop_flag: Arc<AtomicBool>,
    forwarder_thread: Option<JoinHandle<()>>,
}

impl<Forwarded> ReservedSlotForwarder<Forwarded> {
    /// The channel data service this forwarder reads.
    pub(crate) fn channel(&self) -> &str {
        &self.channel
    }

    /// The next forwarded item, or `None` once the forwarder has ended — its
    /// bound reached, its subscriber failed, or it is being dropped.
    pub(crate) async fn recv(&mut self) -> Option<Forwarded> {
        self.receiver.recv().await
    }
}

#[cfg(feature = "test-support")]
impl<Forwarded> ReservedSlotForwarder<Forwarded> {
    /// A forwarder over a preset receiver, with no thread and no subscriber.
    pub(crate) fn from_a_preset_receiver(
        channel: String,
        receiver: tokio::sync::mpsc::Receiver<Forwarded>,
    ) -> Self {
        Self {
            channel,
            receiver,
            stop_flag: Arc::new(AtomicBool::new(false)),
            forwarder_thread: None,
        }
    }
}

impl<Forwarded> Drop for ReservedSlotForwarder<Forwarded> {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        // The thread never parks in a send, but a closed receiver also makes any
        // send under way resolve, so the join cannot hang.
        self.receiver.close();
        if let Some(thread) = self.forwarder_thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start reading `slot`, forwarding through `forwarding` until dropped or the
/// slot's bound is reached. A slot already held is refused with
/// [`Error::TapSlotOccupied`].
///
/// Blocks until the slot's subscriber exists, so a refusal reaches the caller
/// rather than the thread: call it from `spawn_blocking`, never directly on an
/// async worker.
pub(crate) fn start_reserved_slot_forwarder<Forwarding: ReservedSlotBagForwarding>(
    slot: ReservedSlotOfAChannelToForward,
    forwarding: Forwarding,
) -> Result<ReservedSlotForwarder<Forwarding::Forwarded>> {
    let (forward_tx, receiver) = tokio::sync::mpsc::channel::<Forwarding::Forwarded>(
        slot.ring_depth
            .min(slot.sizing.channel_service_creation_depth),
    );
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let channel = slot.channel.clone();

    let reader = ReaderOfAReservedSlot {
        slot,
        forwarding,
        forward_tx,
        stop_flag: Arc::clone(&stop_flag),
    };
    let forwarder_thread = std::thread::Builder::new()
        .name("streamlib-reserved-slot".into())
        .spawn(
            crate::core::logging::carrying_this_threads_loaded_stream_log_route(move || {
                reader.run(ready_tx)
            }),
        )
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "failed to spawn the reserved-slot reader of '{channel}': {spawn_failure}"
            ))
        })?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(ReservedSlotForwarder {
            channel,
            receiver,
            stop_flag,
            forwarder_thread: Some(forwarder_thread),
        }),
        Ok(Err(refusal)) => {
            let _ = forwarder_thread.join();
            Err(refusal)
        }
        Err(_) => {
            let _ = forwarder_thread.join();
            Err(Error::Runtime(format!(
                "the reserved-slot reader of '{channel}' ended before saying whether it \
                 subscribed"
            )))
        }
    }
}

/// What the reserved slot's thread holds.
struct ReaderOfAReservedSlot<Forwarding: ReservedSlotBagForwarding> {
    slot: ReservedSlotOfAChannelToForward,
    forwarding: Forwarding,
    forward_tx: tokio::sync::mpsc::Sender<Forwarding::Forwarded>,
    stop_flag: Arc<AtomicBool>,
}

impl<Forwarding: ReservedSlotBagForwarding> ReaderOfAReservedSlot<Forwarding> {
    fn run(self, ready_tx: std::sync::mpsc::Sender<Result<()>>) {
        let Self {
            slot:
                ReservedSlotOfAChannelToForward {
                    node,
                    channel,
                    sizing,
                    ring_depth,
                    count,
                },
            mut forwarding,
            forward_tx,
            stop_flag,
        } = self;
        // The service is reopened with the sizing it was created with and no
        // publisher; iceoryx2 verifies the sizing against the live service.
        let service = match node.open_or_create_service(
            &channel,
            sizing.max_subscribers,
            sizing.channel_service_creation_depth,
        ) {
            Ok(service) => service,
            Err(open_refusal) => {
                let _ = ready_tx.send(Err(open_refusal));
                return;
            }
        };
        let subscriber = match service
            .create_reserved_slot_subscriber(ring_depth.min(sizing.channel_service_creation_depth))
        {
            Ok(subscriber) => subscriber,
            Err(ChannelReservedSlotSubscribeError::ReservedSlotOccupied) => {
                let _ = ready_tx.send(Err(Error::TapSlotOccupied(channel)));
                return;
            }
            Err(ChannelReservedSlotSubscribeError::Transport(detail)) => {
                let _ = ready_tx.send(Err(Error::Runtime(format!(
                    "failed to subscribe to the reserved slot of channel '{channel}': {detail}"
                ))));
                return;
            }
        };
        let _ = ready_tx.send(Ok(()));

        let mut forwarded: usize = 0;
        let mut idle_poll_backoff = ChannelIdlePollBackoff::starting_at_the_shortest_sleep();
        loop {
            if stop_flag.load(Ordering::Acquire) {
                break;
            }
            // The bound is checked before receiving, so a bound of zero forwards
            // nothing at all.
            if count.is_some_and(|bound| forwarded >= bound) {
                break;
            }
            match subscriber.receive() {
                Ok(Some(sample)) => {
                    use tokio::sync::mpsc::error::TrySendError;
                    idle_poll_backoff.reset_after_a_bag_arrived();
                    let Some(item) = forwarding.forwarded_from(BagReceivedOnTheReservedSlot {
                        publisher: sample.origin(),
                        sequence_number: sample.user_header().sequence_number,
                        framed_bag: sample.payload(),
                    }) else {
                        continue;
                    };
                    match forward_tx.try_send(item) {
                        Ok(()) => forwarded += 1,
                        Err(TrySendError::Full(dropped)) => {
                            forwarding.dropped_into_a_full_receiver(dropped, &channel)
                        }
                        Err(TrySendError::Closed(_)) => break,
                    }
                }
                Ok(None) => std::thread::sleep(
                    idle_poll_backoff.sleep_this_empty_poll_earns(Instant::now()),
                ),
                Err(receive_failure) => {
                    tracing::warn!(
                        channel = %channel,
                        "the reserved-slot subscriber failed to receive, so its reader ends: \
                         {receive_failure:?}"
                    );
                    break;
                }
            }
        }
    }
}
