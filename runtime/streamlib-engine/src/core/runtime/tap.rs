// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! First-class channel tap — a read-only subscriber attachable to any named
//! channel via the reserved tap slot the channel data service is sized for.
//!
//! A channel data service is opened with
//! `max_subscribers = N_destinations + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL`
//! (1), so a tap is a pure subscriber-add onto the pre-sized reserved slot: it
//! reopens the existing service publisher-free (iceoryx2 verifies the identical
//! `max_subscribers`) and creates the reserved subscriber. No new service, no
//! publisher change, no sizing change.
//!
//! iceoryx2's `Subscriber` holds `Rc` internally and is `!Send`, so it cannot
//! move into the caller's tokio tasks. The tap therefore owns a dedicated OS
//! thread that holds the subscriber and forwards each raw bag's bytes over a
//! `tokio::sync::mpsc` (Send) to the async caller.
//!
//! The forwarder ALWAYS drains the iceoryx2 subscriber and NEVER blocks on the
//! downstream: it forwards with `try_send` over a BOUNDED mpsc and, when that
//! channel is full, DROPS the newest bag (counted, [`TapSubscription::dropped_bags`])
//! rather than parking. A read-only observability tap must never gate production
//! throughput — on a lossless channel a forwarder parked in a blocking send
//! would stop draining, fill the iceoryx2 buffer, and back-pressure the source
//! processor's `send()`. This is deliberately unlike the UNBOUNDED event
//! WebSocket bridge: the tap trades completeness for guaranteed non-interference.
//!
//! Detaching = dropping the [`TapSubscription`]: the mpsc receiver is closed,
//! the forwarder thread is signalled to stop, joined, and the reserved slot is
//! freed for the next tap.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::core::error::{Error, Result};
use crate::iceoryx2::{ChannelTapSubscribeError, Iceoryx2Node};

/// Idle backoff between empty `subscriber.receive()` polls on the forwarder
/// thread. The tap has no notify-listener slot of its own (the notify service
/// is destination-keyed and sized to fan-in), so it polls the channel ring; a
/// short backoff keeps a quiet channel from busy-spinning a core while still
/// draining a live channel promptly.
const TAP_IDLE_POLL_BACKOFF: Duration = Duration::from_micros(500);

/// Emit a drop warning on the first drop then once per this many subsequent
/// drops, so a persistently-slow downstream is visible without spamming a log
/// line per dropped bag on a hot channel.
const TAP_DROP_WARN_INTERVAL: u64 = 256;

/// The iceoryx2 sizing a tap must reopen its channel data service with. Both
/// the compiler op that created the service and this tap derive the same triple
/// from the live graph, so iceoryx2 accepts the publisher-free reopen.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TapChannelSizing {
    pub(crate) max_subscribers: usize,
    pub(crate) max_queued_messages: usize,
    pub(crate) enable_safe_overflow: bool,
}

/// The two Arc handles shared between a [`TapSubscription`] and its forwarder
/// thread: the stop flag the owner raises on detach, and the counter the
/// forwarder bumps every time it drops a bag into a full downstream.
#[derive(Debug, Clone)]
struct TapForwarderSignals {
    stop_flag: Arc<AtomicBool>,
    dropped_bags: Arc<AtomicU64>,
}

/// A live read-only subscription to a named channel, streaming that channel's
/// raw bag bytes ([`FrameHeader`](streamlib_ipc_types)-framed, exactly as
/// published) to the async caller.
///
/// Dropping the subscription detaches the tap: the forwarder thread stops,
/// joins, and drops the underlying subscriber — freeing the channel's reserved
/// tap slot so a subsequent tap can attach.
#[derive(Debug)]
pub struct TapSubscription {
    channel: String,
    receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    signals: TapForwarderSignals,
    forwarder_thread: Option<JoinHandle<()>>,
}

impl TapSubscription {
    /// The channel data-service name this tap is attached to.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Await the next raw bag from the channel. Returns `None` once the tap is
    /// exhausted — the bounded sample count was reached, the channel's
    /// forwarder thread ended, or the subscription is detaching.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.receiver.recv().await
    }

    /// Bags this tap dropped because the async consumer fell behind and the
    /// bounded forward channel was full. A read-only tap drops rather than
    /// back-pressuring the source, so a non-zero count means the observer is
    /// too slow for the channel's rate, not that the pipeline stalled.
    pub fn dropped_bags(&self) -> u64 {
        self.signals.dropped_bags.load(Ordering::Acquire)
    }
}

impl Drop for TapSubscription {
    fn drop(&mut self) {
        self.signals.stop_flag.store(true, Ordering::Release);
        // Belt-and-suspenders: the forwarder uses try_send and never parks, but
        // closing the receiver guarantees any in-flight send resolves to Closed
        // so join() cannot hang.
        self.receiver.close();
        if let Some(thread) = self.forwarder_thread.take() {
            let _ = thread.join();
        }
    }
}

/// Attach a tap to the channel `channel` on `node`, sized to match the service
/// the compiler opened, streaming raw bags to the returned [`TapSubscription`].
///
/// `count` bounds the tap to that many bags then ends; `None` streams live
/// until the subscription is dropped. Blocks briefly until the reserved-slot
/// subscriber is created so a [`Error::TapSlotOccupied`] second-tap rejection
/// surfaces to the caller rather than failing silently on the thread — call it
/// from `spawn_blocking`, not directly on an async worker.
pub(crate) fn start_channel_tap(
    node: Iceoryx2Node,
    channel: String,
    sizing: TapChannelSizing,
    count: Option<usize>,
) -> Result<TapSubscription> {
    let forward_capacity = sizing.max_queued_messages.max(1);
    let (forward_tx, receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(forward_capacity);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let signals = TapForwarderSignals {
        stop_flag: Arc::new(AtomicBool::new(false)),
        dropped_bags: Arc::new(AtomicU64::new(0)),
    };

    let thread_channel = channel.clone();
    let thread_signals = signals.clone();
    let forwarder_thread = std::thread::Builder::new()
        .name("streamlib-channel-tap".into())
        .spawn(move || {
            run_forwarder(
                node,
                thread_channel,
                sizing,
                count,
                forward_tx,
                ready_tx,
                thread_signals,
            );
        })
        .map_err(|e| Error::Runtime(format!("failed to spawn channel-tap thread: {e}")))?;

    // The forwarder reports its subscribe outcome once, before entering the
    // receive loop. A dropped sender (thread panicked before reporting) reads
    // as a Runtime error rather than a hang.
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(TapSubscription {
            channel,
            receiver,
            signals,
            forwarder_thread: Some(forwarder_thread),
        }),
        Ok(Err(subscribe_error)) => {
            let _ = forwarder_thread.join();
            Err(subscribe_error)
        }
        Err(_) => {
            let _ = forwarder_thread.join();
            Err(Error::Runtime(format!(
                "channel-tap thread for '{channel}' ended before reporting its subscribe outcome"
            )))
        }
    }
}

/// Body of the dedicated tap thread: open the channel service publisher-free,
/// create the reserved-slot subscriber, report the outcome, then forward raw
/// bags until stopped, the client detaches, or the bounded count is reached.
fn run_forwarder(
    node: Iceoryx2Node,
    channel: String,
    sizing: TapChannelSizing,
    count: Option<usize>,
    forward_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    ready_tx: std::sync::mpsc::Sender<Result<()>>,
    signals: TapForwarderSignals,
) {
    // Reopen the existing service on the OPEN path — same max_subscribers, no
    // publisher created ("no publisher re-open" holds). iceoryx2 verifies the
    // sizing against the live service.
    let service = match node.open_or_create_service(
        &channel,
        sizing.max_subscribers,
        sizing.max_queued_messages,
        sizing.enable_safe_overflow,
    ) {
        Ok(service) => service,
        Err(open_error) => {
            let _ = ready_tx.send(Err(open_error));
            return;
        }
    };

    let subscriber = match service.create_tap_subscriber() {
        Ok(subscriber) => subscriber,
        Err(ChannelTapSubscribeError::ReservedSlotOccupied) => {
            let _ = ready_tx.send(Err(Error::TapSlotOccupied(channel)));
            return;
        }
        Err(ChannelTapSubscribeError::Transport(detail)) => {
            let _ = ready_tx.send(Err(Error::Runtime(format!(
                "failed to attach tap subscriber to channel '{channel}': {detail}"
            ))));
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));

    let mut delivered: usize = 0;
    loop {
        if signals.stop_flag.load(Ordering::Acquire) {
            break;
        }
        match subscriber.receive() {
            Ok(Some(sample)) => {
                use tokio::sync::mpsc::error::TrySendError;
                match forward_tx.try_send(sample.payload().to_vec()) {
                    Ok(()) => {
                        delivered += 1;
                        if let Some(bound) = count {
                            if delivered >= bound {
                                break;
                            }
                        }
                    }
                    // Downstream full: DROP the newest bag so the iceoryx2
                    // subscriber stays drained and the source is never
                    // back-pressured by this read-only tap.
                    Err(TrySendError::Full(_dropped_bag)) => {
                        let total =
                            signals.dropped_bags.fetch_add(1, Ordering::AcqRel) + 1;
                        if total == 1 || total % TAP_DROP_WARN_INTERVAL == 0 {
                            tracing::warn!(
                                channel = %channel,
                                dropped_bags = total,
                                "channel tap downstream is full; dropping bags to keep the \
                                 iceoryx2 subscriber drained (a read-only tap never \
                                 back-pressures the source)"
                            );
                        }
                    }
                    // Receiver dropped/closed — the TapSubscription detached.
                    Err(TrySendError::Closed(_)) => break,
                }
            }
            Ok(None) => std::thread::sleep(TAP_IDLE_POLL_BACKOFF),
            Err(receive_error) => {
                tracing::warn!(
                    channel = %channel,
                    "channel tap subscriber.receive() failed, ending tap: {receive_error:?}"
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use iceoryx2::prelude::*;
    use streamlib_ipc_types::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;

    use crate::iceoryx2::{FRAME_HEADER_SIZE, Iceoryx2Node, Iceoryx2Service};

    const RING_DEPTH: usize = 16;

    fn unique_channel_name(tag: &str) -> String {
        format!(
            "test/tap/{}/{}/{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// Publish one bag whose last payload byte is `marker` (a full
    /// `FRAME_HEADER_SIZE + 1` slice, mirroring the channel wire shape).
    fn publish_marker(
        publisher: &iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>,
        marker: u8,
    ) {
        let mut payload = vec![0u8; FRAME_HEADER_SIZE + 1];
        payload[FRAME_HEADER_SIZE] = marker;
        let sample = publisher
            .loan_slice_uninit(payload.len())
            .expect("loan slot");
        let sample = sample.write_from_slice(&payload);
        sample.send().expect("send bag");
    }

    fn open_channel(node: &Iceoryx2Node, name: &str, max_subscribers: usize) -> Iceoryx2Service {
        node.open_or_create_service(name, max_subscribers, RING_DEPTH, true)
            .expect("open channel data service")
    }

    /// Realtime (drop-oldest) sizing matching [`open_channel`], for the tap's
    /// publisher-free reopen.
    fn realtime_sizing(max_subscribers: usize) -> TapChannelSizing {
        TapChannelSizing {
            max_subscribers,
            max_queued_messages: RING_DEPTH,
            enable_safe_overflow: true,
        }
    }

    /// A tap attaches to a live channel's reserved slot, observes the bags
    /// published after it attached, and — while it holds the reserved slot — a
    /// second concurrent tap is rejected with the named `TapSlotOccupied`.
    /// Detaching (drop) frees the slot so a subsequent tap attaches cleanly.
    ///
    /// Mentally revert `RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL` to 0: the
    /// service is sized to `destinations` only, so the FIRST tap (the
    /// destination+1'th subscriber) no longer fits and this test's initial
    /// attach fails.
    #[test]
    fn tap_observes_live_bags_then_detach_frees_the_reserved_slot() {
        // One destination occupies one slot; the reserved tap slot is the +1.
        let destinations = 1usize;
        let max_subscribers = destinations + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;

        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let channel = unique_channel_name("live");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");
        // Occupy the destination slot(s) so the tap can only take the reserved one.
        let _destination_subscribers: Vec<_> = (0..destinations)
            .map(|_| service.create_subscriber().expect("destination subscriber"))
            .collect();

        // Tap #1 fills the reserved slot.
        let mut tap = start_channel_tap(node.clone(), channel.clone(), realtime_sizing(max_subscribers), None)
            .expect("first tap attaches to the reserved slot");

        // Tap #2 must be rejected — the single reserved slot is taken.
        let err = start_channel_tap(node.clone(), channel.clone(), realtime_sizing(max_subscribers), None)
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
        let tap_again = start_channel_tap(node.clone(), channel.clone(), realtime_sizing(max_subscribers), None)
            .expect("reserved slot must be free again after the first tap detached");
        drop(tap_again);
    }

    /// A bounded tap (`count = Some(n)`) forwards exactly `n` bags then ends its
    /// stream — `recv()` returns `None` after the nth — and the bound-reached
    /// end also frees the reserved slot (the forwarder thread exits its loop).
    #[test]
    fn bounded_tap_delivers_exactly_n_bags_then_ends() {
        let max_subscribers = RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let channel = unique_channel_name("bounded");
        let service = open_channel(&node, &channel, max_subscribers);
        let publisher = service.create_publisher(64).expect("channel publisher");

        let mut tap = start_channel_tap(node.clone(), channel.clone(), realtime_sizing(max_subscribers), Some(2))
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

    /// A stalled downstream (the async consumer never calls `recv()`) must NOT
    /// block the forwarder's iceoryx2 drain, and detaching such a tap must
    /// return promptly instead of hanging on a parked forwarder.
    ///
    /// This is exercised on a LOSSLESS channel (`enable_safe_overflow = false`),
    /// the exact production hazard: a forwarder parked in a blocking send would
    /// stop draining iceoryx2 and back-pressure the source's `send()`. The
    /// forwarder must instead keep receiving and DROP into the full mpsc
    /// (asserted via `dropped_bags()`), and `drop(tap)` must not hang.
    ///
    /// Mentally revert to `blocking_send` (and remove `receiver.close()` from
    /// Drop): the forwarder parks the moment the bounded mpsc fills, never bumps
    /// `dropped_bags`, and — because Drop joins while the receiver is still
    /// alive — `drop(tap)` blocks forever, tripping the recv_timeout below.
    #[test]
    fn stalled_downstream_never_blocks_the_drain_and_detach_returns_promptly() {
        let max_subscribers = RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let channel = unique_channel_name("backpressure");
        // Lossless: a forwarder that parks would stall the producer.
        let service = node
            .open_or_create_service(&channel, max_subscribers, RING_DEPTH, false)
            .expect("open lossless channel data service");
        let publisher = service.create_publisher(64).expect("channel publisher");

        let sizing = TapChannelSizing {
            max_subscribers,
            max_queued_messages: RING_DEPTH,
            enable_safe_overflow: false,
        };
        // Never drain this tap: the bounded forward channel (capacity RING_DEPTH)
        // fills and stays full, so every further bag must be dropped.
        let tap = start_channel_tap(node.clone(), channel.clone(), sizing, None)
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
}
