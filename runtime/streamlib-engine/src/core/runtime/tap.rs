// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A read-only tap on a named channel: the channel's reserved subscriber slot,
//! read by a [`ReservedSlotForwarder`] that hands over each bag verbatim,
//! frame header included. Decoding is the client's concern; a raw video
//! channel carries surface descriptors, meaningless off the machine.
//!
//! A full receiver drops the newest bag, counted on
//! [`TapSubscription::dropped_bags`]: a tap trades completeness for liveness.
//! Dropping the [`TapSubscription`] stops its reader and frees the slot.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::reserved_slot_forwarder::{
    BagReceivedOnTheReservedSlot, ReservedSlotBagForwarding, ReservedSlotForwarder,
    ReservedSlotOfAChannelToForward, start_reserved_slot_forwarder,
};
use crate::core::error::Result;
use crate::iceoryx2::{ChannelSizing, Iceoryx2Node};

/// A drop is warned of at the first and then once per this many, so a
/// persistently slow client is visible without a log line per bag.
const TAP_DROP_WARN_INTERVAL: u64 = 256;

/// A tap's forwarding: each bag's bytes as published, and a count of the bags
/// a full receiver dropped.
struct TapBagForwarding {
    dropped_bags: Arc<AtomicU64>,
}

impl ReservedSlotBagForwarding for TapBagForwarding {
    type Forwarded = Vec<u8>;

    fn forwarded_from(&mut self, bag: BagReceivedOnTheReservedSlot<'_>) -> Option<Vec<u8>> {
        Some(bag.framed_bag.to_vec())
    }

    fn dropped_into_a_full_receiver(&mut self, _dropped: Vec<u8>, channel: &str) {
        let total = self.dropped_bags.fetch_add(1, Ordering::AcqRel) + 1;
        if total == 1 || total.is_multiple_of(TAP_DROP_WARN_INTERVAL) {
            tracing::warn!(
                channel = %channel,
                dropped_bags = total,
                "channel tap downstream is full; dropping bags to keep the iceoryx2 subscriber \
                 drained (a read-only tap never back-pressures the source)"
            );
        }
    }
}

/// A live read-only subscription to a named channel, streaming that channel's
/// raw bags as published. Dropping it detaches the tap and frees the channel's
/// reserved slot.
#[derive(Debug)]
pub struct TapSubscription {
    forwarder: ReservedSlotForwarder<Vec<u8>>,
    dropped_bags: Arc<AtomicU64>,
}

impl TapSubscription {
    /// The channel data-service name this tap is attached to.
    pub fn channel(&self) -> &str {
        self.forwarder.channel()
    }

    /// Await the next raw bag from the channel, or `None` once the tap is
    /// exhausted — its bound reached, its reader ended, or it is detaching.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.forwarder.recv().await
    }

    /// Bags this tap dropped because its client fell behind and the bounded
    /// forward channel was full: a slow observer, never a stalled pipeline.
    pub fn dropped_bags(&self) -> u64 {
        self.dropped_bags.load(Ordering::Acquire)
    }
}

#[cfg(feature = "test-support")]
impl TapSubscription {
    /// An in-memory tap over a preset forward receiver and dropped-bag count,
    /// with no reader thread and no iceoryx2 subscriber.
    pub fn from_forward_channel(
        channel: String,
        receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
        dropped_bags: u64,
    ) -> Self {
        Self {
            forwarder: ReservedSlotForwarder::from_a_preset_receiver(channel, receiver),
            dropped_bags: Arc::new(AtomicU64::new(dropped_bags)),
        }
    }
}

/// Attach a tap to the channel `channel` on `node`, sized as the compiler
/// opened it. `count` bounds the tap to that many bags; `None` streams until
/// the subscription is dropped. A slot already held is refused with
/// [`Error::TapSlotOccupied`]. Blocks until subscribed: call it from
/// `spawn_blocking`.
pub(crate) fn start_channel_tap(
    node: Iceoryx2Node,
    channel: String,
    sizing: ChannelSizing,
    count: Option<usize>,
) -> Result<TapSubscription> {
    let dropped_bags = Arc::new(AtomicU64::new(0));
    let forwarder = start_reserved_slot_forwarder(
        ReservedSlotOfAChannelToForward {
            iceoryx2_node: node,
            channel,
            ring_depth: sizing.channel_service_creation_depth,
            sizing,
            items_to_forward_before_ending: count,
        },
        TapBagForwarding {
            dropped_bags: Arc::clone(&dropped_bags),
        },
    )?;
    Ok(TapSubscription {
        forwarder,
        dropped_bags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::Error;
    use std::time::Duration;

    use streamlib_ipc_types::RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;

    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use crate::iceoryx2::{
        CHANNEL_QUIET_BEFORE_THE_BACKOFF_CLIMBS, FRAME_HEADER_SIZE, Iceoryx2Node, Iceoryx2Service,
    };

    const RING_DEPTH: usize = 16;

    fn unique_channel_name(tag: &str) -> String {
        format!(
            "test/tap/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    /// Publish one bag whose last payload byte is `marker` (a full
    /// `FRAME_HEADER_SIZE + 1` slice, mirroring the channel wire shape).
    fn publish_marker(publisher: &crate::iceoryx2::ChannelDataServicePublisher, marker: u8) {
        let mut payload = vec![0u8; FRAME_HEADER_SIZE + 1];
        payload[FRAME_HEADER_SIZE] = marker;
        let sample = publisher
            .loan_slice_uninit(payload.len())
            .expect("loan slot");
        let sample = sample.write_from_slice(&payload);
        sample.send().expect("send bag");
    }

    fn open_channel(node: &Iceoryx2Node, name: &str, max_subscribers: usize) -> Iceoryx2Service {
        node.open_or_create_service(name, max_subscribers, RING_DEPTH)
            .expect("open channel data service")
    }

    /// Sizing matching [`open_channel`], for the tap's publisher-free reopen.
    fn tap_channel_sizing_matching_open_channel(max_subscribers: usize) -> ChannelSizing {
        ChannelSizing {
            max_subscribers,
            channel_service_creation_depth: RING_DEPTH,
        }
    }

    /// A tap attaches to a live channel's reserved slot, observes the bags
    /// published after it attached, and — while it holds the reserved slot — a
    /// second concurrent tap is rejected with the named `TapSlotOccupied`.
    /// Detaching (drop) frees the slot so a subsequent tap attaches cleanly.
    ///
    /// Mentally revert `RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL` to 0: the
    /// service is sized to `destinations` only, so the FIRST tap (the
    /// destination+1'th subscriber) no longer fits and this test's initial
    /// attach fails.
    #[test]
    fn tap_observes_live_bags_then_detach_frees_the_reserved_slot() {
        // One destination occupies one slot; the reserved tap slot is the +1.
        let destinations = 1usize;
        let max_subscribers = destinations + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;

        let node = Iceoryx2Node::for_this_test_process();
        let channel = unique_channel_name("live");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");
        // Occupy the destination slot(s) so the tap can only take the reserved one.
        let _destination_subscribers: Vec<_> = (0..destinations)
            .map(|_| {
                service
                    .create_subscriber(RING_DEPTH)
                    .expect("destination subscriber")
            })
            .collect();

        // Tap #1 fills the reserved slot.
        let mut tap = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            None,
        )
        .expect("first tap attaches to the reserved slot");

        // Tap #2 must be rejected — the single reserved slot is taken.
        let err = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            None,
        )
        .expect_err("a second concurrent tap must be rejected");
        assert!(
            matches!(err, Error::TapSlotOccupied(_)),
            "second concurrent tap must fail with TapSlotOccupied; got {err:?}",
        );

        // The tap observes bags published after it attached.
        for marker in 0u8..3 {
            publish_marker(&publisher, marker);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            for expected in 0u8..3 {
                let bag = tokio::time::timeout(Duration::from_secs(2), tap.recv())
                    .await
                    .expect("tap delivers a bag within 2s")
                    .expect("tap stream is still open");
                assert_eq!(
                    bag.len(),
                    FRAME_HEADER_SIZE + 1,
                    "tap forwards the full framed bag verbatim",
                );
                assert_eq!(
                    bag[FRAME_HEADER_SIZE], expected,
                    "tap delivers bags in publish order",
                );
            }
        });

        // Detach — dropping the subscription joins the forwarder thread and
        // frees the reserved slot.
        drop(tap);

        // A fresh tap now attaches only because the slot was freed on detach.
        let tap_again = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            None,
        )
        .expect("reserved slot must be free again after the first tap detached");
        drop(tap_again);
    }

    /// A bounded tap (`count = Some(n)`) forwards exactly `n` bags then ends its
    /// stream — `recv()` returns `None` after the nth — and the bound-reached
    /// end also frees the reserved slot (the forwarder thread exits its loop).
    #[test]
    fn bounded_tap_delivers_exactly_n_bags_then_ends() {
        let max_subscribers = RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::for_this_test_process();
        let channel = unique_channel_name("bounded");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");

        let mut tap = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            Some(2),
        )
        .expect("bounded tap attaches");

        for marker in 0u8..5 {
            publish_marker(&publisher, marker);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            let first = tokio::time::timeout(Duration::from_secs(2), tap.recv())
                .await
                .expect("first bag arrives")
                .expect("stream open");
            assert_eq!(first[FRAME_HEADER_SIZE], 0);
            let second = tokio::time::timeout(Duration::from_secs(2), tap.recv())
                .await
                .expect("second bag arrives")
                .expect("stream open");
            assert_eq!(second[FRAME_HEADER_SIZE], 1);
            // The bound was 2 — the stream ends, so recv() resolves to None.
            let ended = tokio::time::timeout(Duration::from_secs(2), tap.recv())
                .await
                .expect("bounded tap ends its stream promptly");
            assert!(
                ended.is_none(),
                "a count=2 tap must end its stream after exactly 2 bags",
            );
        });
    }

    /// A `count = Some(0)` tap forwards ZERO bags and closes its stream cleanly,
    /// even while the channel keeps publishing. The bound is enforced before the
    /// first forward, so no frame leaks past a zero bound.
    ///
    /// Mentally revert the pre-forward bound check to the post-forward one: the
    /// first bag is delivered before `delivered >= 0` is observed, so `recv()`
    /// yields one frame instead of `None` and this test goes red.
    #[test]
    fn zero_count_tap_delivers_no_bags_then_closes() {
        let max_subscribers = RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::for_this_test_process();
        let channel = unique_channel_name("zero-count");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");

        let mut tap = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            Some(0),
        )
        .expect("zero-count tap attaches");

        for marker in 0u8..5 {
            publish_marker(&publisher, marker);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            let ended = tokio::time::timeout(Duration::from_secs(2), tap.recv())
                .await
                .expect("zero-count tap closes its stream promptly");
            assert!(
                ended.is_none(),
                "a count=0 tap must deliver zero bags and close cleanly",
            );
        });
    }

    /// A stalled downstream (the async consumer never calls `recv()`) must NOT
    /// block the forwarder's iceoryx2 drain, and detaching such a tap must
    /// return promptly instead of hanging on a parked forwarder.
    ///
    /// The forwarder must keep receiving and DROP into the full mpsc (asserted
    /// via `dropped_bags()`) rather than parking: a parked forwarder stops
    /// draining the reserved slot, so the bags it never picks up reach no
    /// observer at all, and `drop(tap)` joins a thread that never returns.
    ///
    /// Mentally revert to `blocking_send` (and remove `receiver.close()` from
    /// Drop): the forwarder parks the moment the bounded mpsc fills, never bumps
    /// `dropped_bags`, and — because Drop joins while the receiver is still
    /// alive — `drop(tap)` blocks forever, tripping the recv_timeout below.
    #[test]
    fn stalled_downstream_never_blocks_the_drain_and_detach_returns_promptly() {
        let max_subscribers = RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::for_this_test_process();
        let channel = unique_channel_name("stalled-downstream");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");

        // Never drain this tap: the bounded forward channel (capacity RING_DEPTH)
        // fills and stays full, so every further bag must be dropped.
        let tap = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            None,
        )
        .expect("tap attaches");

        // Drive far more than the mpsc capacity through the channel until the
        // forwarder reports drops — proving it kept receiving past a full
        // downstream rather than parking.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut marker: u8 = 0;
        while tap.dropped_bags() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "forwarder never dropped a bag — it is not draining past a full downstream",
            );
            for _ in 0..(RING_DEPTH * 2) {
                publish_marker(&publisher, marker);
                marker = marker.wrapping_add(1);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            tap.dropped_bags() > 0,
            "a stalled downstream must make the forwarder drop bags, not park",
        );

        // Detaching must return promptly — the forwarder is never parked in a
        // blocking send, so join() completes within its next poll.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let joiner = std::thread::spawn(move || {
            drop(tap);
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("drop(tap) must return promptly, not hang on a parked forwarder");
        joiner.join().expect("detach thread joins");
    }

    /// The forwarder's own use of the backoff: a channel left quiet long enough
    /// for the sleep to reach its ceiling still hands the next bag over.
    ///
    /// What it catches: a backoff with no ceiling. Remove the `min` and a quiet
    /// channel's sleep doubles without bound, so the first bag below never
    /// arrives inside the timeout. What it does NOT catch, stated rather than
    /// implied: a missing `reset_after_a_bag_arrived` — that leaves the sleep at
    /// its ceiling, which is 20 ms against a 2 s bound. The reset is pinned by
    /// the policy test above; discriminating it here would mean timing a 20 ms
    /// sleep against a 1 ms one, which is a flake in CI rather than a guard.
    #[test]
    fn a_bag_published_after_a_quiet_stretch_still_reaches_a_tap() {
        let max_subscribers = RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::for_this_test_process();
        let channel = unique_channel_name("quiet-then-carrying");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");

        let mut tap = start_channel_tap(
            node.clone(),
            channel.clone(),
            tap_channel_sizing_matching_open_channel(max_subscribers),
            None,
        )
        .expect("tap attaches");

        // Long enough that the forwarder is sleeping at its ceiling by now.
        std::thread::sleep(CHANNEL_QUIET_BEFORE_THE_BACKOFF_CLIMBS * 3);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            for marker in [7u8, 9u8] {
                publish_marker(&publisher, marker);
                let bag = tokio::time::timeout(Duration::from_secs(2), tap.recv())
                    .await
                    .expect("a bag after a quiet stretch must not be stranded by the backoff")
                    .expect("the tap stream must still be open");
                assert_eq!(
                    bag[FRAME_HEADER_SIZE], marker,
                    "the tap must forward the bag that ended the quiet stretch"
                );
            }
        });
    }
}
