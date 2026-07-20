// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 Node wrapper for StreamLib runtime.

use std::sync::Arc;

use iceoryx2::node::Node;
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use super::{EventPayload, FRAME_HEADER_SIZE, MAX_PUBLISHERS_PER_CHANNEL};
use crate::core::error::{Error, Result};

/// Thread-safe wrapper for iceoryx2 Node.
///
/// The Node is created once per runtime and shared across all processors.
/// Services, Publishers, and Subscribers are created through this Node.
#[derive(Clone)]
pub struct Iceoryx2Node {
    inner: Arc<Mutex<Node<ipc::Service>>>,
}

impl Iceoryx2Node {
    /// Create a new iceoryx2 Node.
    pub fn new() -> Result<Self> {
        let node = NodeBuilder::new()
            .create::<ipc::Service>()
            .map_err(|e| Error::Runtime(format!("Failed to create iceoryx2 node: {:?}", e)))?;

        Ok(Self {
            inner: Arc::new(Mutex::new(node)),
        })
    }

    /// Open or create a publish-subscribe service for EventPayload.
    ///
    /// The service name should follow the format: "streamlib/{runtime_id}/events/{topic}"
    pub fn open_or_create_event_service(&self, service_name: &str) -> Result<Iceoryx2EventService> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .publish_subscribe::<EventPayload>()
            .max_publishers(16)
            .subscriber_max_buffer_size(64)
            .open_or_create()
            .map_err(|e| Error::Runtime(format!("Failed to open/create event service: {:?}", e)))?;

        Ok(Iceoryx2EventService { inner: service })
    }

    /// Open or create an iceoryx2 Event service for fd-multiplexed wakeups.
    ///
    /// Pairs with a destination's data channels for fd-multiplexed wakeups: the
    /// notify service stays destination-keyed (`streamlib/<dest>/notify`) so a
    /// destination waits on ONE `Listener` fd regardless of fan-in, while every
    /// upstream source publishing into one of its channels holds a `Notifier`
    /// here. `max_notifiers` is the destination's compile-time fan-in (the count
    /// of inbound links). Distinct from [`Iceoryx2EventService`] which is a typed
    /// pub/sub for runtime events.
    pub fn open_or_create_notify_service(
        &self,
        service_name: &str,
        max_notifiers: usize,
    ) -> Result<Iceoryx2NotifyService> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .event()
            .max_notifiers(max_notifiers)
            .max_listeners(1)
            .open_or_create()
            .map_err(|e| {
                Error::Runtime(format!("Failed to open/create notify service: {:?}", e))
            })?;

        Ok(Iceoryx2NotifyService { inner: service })
    }

    /// Open or create a channel-centric publish-subscribe service for `[u8]`
    /// slices.
    ///
    /// The service name is the source-port channel
    /// (`{source_processor}/{source_output_port}`). The service carries exactly
    /// [`MAX_PUBLISHERS_PER_CHANNEL`] (1) publisher — the source — and
    /// `max_subscribers` slots: one per compile-time-known destination plus the
    /// reserved tap slot ([`crate::iceoryx2::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL`]).
    /// Every opener (host + subprocess SDKs) must request the SAME `max_subscribers`
    /// — iceoryx2 verifies it on `open`.
    ///
    /// `max_queued_messages` caps how many `[u8]` samples any subscriber on this
    /// service can buffer — resolved from the channel's wire schema's
    /// `metadata.max_queued_messages` via
    /// [`crate::core::embedded_schemas::max_queued_messages_for_port_spec`],
    /// defaulting to [`crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES`].
    ///
    /// `enable_safe_overflow` derives from the channel's destination overflow
    /// policy (see [`crate::core::embedded_schemas::overflow_for_input_port`]).
    /// When `true` (the realtime default — `Overflow::DropOldest`), the subscriber
    /// buffer auto-evicts the oldest sample on overflow and the publisher's
    /// `send()` never blocks. When `false` (`Overflow::Block`), the producer blocks
    /// until the consumer drains a slot — reserve for muxers / file writers that
    /// need every sample in order.
    pub fn open_or_create_service(
        &self,
        service_name: &str,
        max_subscribers: usize,
        max_queued_messages: usize,
        enable_safe_overflow: bool,
    ) -> Result<Iceoryx2Service> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .max_publishers(MAX_PUBLISHERS_PER_CHANNEL)
            .max_subscribers(max_subscribers)
            .subscriber_max_buffer_size(max_queued_messages)
            .enable_safe_overflow(enable_safe_overflow)
            .open_or_create()
            .map_err(|e| Error::Runtime(format!("Failed to open/create service: {:?}", e)))?;

        Ok(Iceoryx2Service {
            inner: service,
            max_queued_messages,
        })
    }
}

/// Handle to an iceoryx2 publish-subscribe service for `[u8]` slices.
pub struct Iceoryx2Service {
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<ipc::Service, [u8], ()>,
    max_queued_messages: usize,
}

impl Iceoryx2Service {
    /// Maximum number of messages this service's subscribers can queue.
    pub fn max_queued_messages(&self) -> usize {
        self.max_queued_messages
    }

    /// Create a publisher for this service.
    ///
    /// `max_payload_bytes` sets the per-slot shared memory capacity (data only, header is added
    /// internally). Use `embedded_schemas::max_payload_bytes_for_port_spec` to derive this value
    /// from the output port's schema declaration.
    pub fn create_publisher(
        &self,
        max_payload_bytes: usize,
    ) -> Result<iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>> {
        self.inner
            .publisher_builder()
            .initial_max_slice_len(max_payload_bytes + FRAME_HEADER_SIZE)
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create publisher: {:?}", e)))
    }

    /// Create a subscriber for this service, requesting the service's
    /// configured ring depth.
    pub fn create_subscriber(
        &self,
    ) -> Result<iceoryx2::port::subscriber::Subscriber<ipc::Service, [u8], ()>> {
        self.inner
            .subscriber_builder()
            .buffer_size(self.max_queued_messages)
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create subscriber: {:?}", e)))
    }
}

/// Handle to an iceoryx2 Event service used for fd-multiplexed wakeups.
///
/// Distinct from [`Iceoryx2EventService`] (which is a typed pub/sub for runtime events).
/// This wraps iceoryx2's `MessagingPattern::Event` — `Notifier::notify()` causes any
/// `Listener` on the same service to become readable on its underlying fd.
pub struct Iceoryx2NotifyService {
    inner: iceoryx2::service::port_factory::event::PortFactory<ipc::Service>,
}

impl Iceoryx2NotifyService {
    /// Create a notifier for this service.
    pub fn create_notifier(&self) -> Result<Notifier<ipc::Service>> {
        self.inner
            .notifier_builder()
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create notifier: {:?}", e)))
    }

    /// Create a listener for this service.
    pub fn create_listener(&self) -> Result<Listener<ipc::Service>> {
        self.inner
            .listener_builder()
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create listener: {:?}", e)))
    }
}

/// Handle to an iceoryx2 publish-subscribe service for events.
pub struct Iceoryx2EventService {
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc::Service,
        EventPayload,
        (),
    >,
}

impl Iceoryx2EventService {
    /// Create a publisher for this event service.
    pub fn create_publisher(
        &self,
    ) -> Result<iceoryx2::port::publisher::Publisher<ipc::Service, EventPayload, ()>> {
        self.inner
            .publisher_builder()
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create event publisher: {:?}", e)))
    }

    /// Create a subscriber for this event service.
    pub fn create_subscriber(
        &self,
    ) -> Result<iceoryx2::port::subscriber::Subscriber<ipc::Service, EventPayload, ()>> {
        self.inner
            .subscriber_builder()
            .buffer_size(64)
            .create()
            .map_err(|e| Error::Runtime(format!("Failed to create event subscriber: {:?}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_service_name(tag: &str) -> String {
        format!(
            "test/node/{}/{}/{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// The destination-keyed notify service honors the requested `max_notifiers`
    /// (its compile-time fan-in) — exactly that many notifiers can be created and
    /// the (fan-in+1)th must fail. Every source publishing into one of the
    /// destination's channels holds one notifier here, so the cap must equal the
    /// inbound-link count the compiler passes.
    #[test]
    fn notify_service_honors_requested_max_notifiers() {
        let fanin = 3usize;
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_notify_service(&unique_service_name("notify_cap"), fanin)
            .expect("open notify service");

        let mut notifiers = Vec::with_capacity(fanin);
        for i in 0..fanin {
            notifiers.push(
                service
                    .create_notifier()
                    .unwrap_or_else(|e| panic!("notifier {i} (under cap) must succeed: {e:?}")),
            );
        }
        assert!(
            service.create_notifier().is_err(),
            "creating notifier {} must fail — notify service was opened with \
             max_notifiers={fanin}",
            fanin + 1,
        );
    }

    /// A channel data service carries exactly ONE publisher (the source) and
    /// `N + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL` subscribers. This is the
    /// transport inversion (#1419): the old destination-centric service pinned
    /// `max_subscribers = 1` and let N publishers fan in; a channel service pins
    /// `max_publishers = 1` and lets N subscribers fan OUT one zero-copy loan,
    /// reserving one extra slot for a phase-3.5 tap.
    ///
    /// Mentally-revert: raise `max_publishers` back above 1 in
    /// [`Iceoryx2Node::open_or_create_service`] and the second `create_publisher`
    /// stops failing; drop the reserved tap slot and the (N+1)th subscriber (the
    /// tap) stops fitting. Both halves fail here when the contract is broken.
    #[test]
    fn channel_service_single_publisher_n_plus_tap_subscribers() {
        use streamlib_ipc_types::RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;

        let destinations = 3usize;
        let max_subscribers = destinations + RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL;
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_service(&unique_service_name("chan_caps"), max_subscribers, 4, true)
            .expect("open channel data service");

        // Exactly one publisher — the source.
        let _publisher = service.create_publisher(64).expect("the source publisher");
        assert!(
            service.create_publisher(64).is_err(),
            "a channel carries exactly one publisher — max_publishers drifted above \
             MAX_PUBLISHERS_PER_CHANNEL (1)",
        );

        // N destination subscribers plus the one reserved tap slot fit; the slot
        // after that does not.
        let mut subscribers = Vec::with_capacity(max_subscribers);
        for i in 0..max_subscribers {
            subscribers.push(
                service
                    .create_subscriber()
                    .unwrap_or_else(|e| panic!("subscriber {i} (destination or tap) must fit: {e:?}")),
            );
        }
        assert!(
            service.create_subscriber().is_err(),
            "the {}th subscriber must fail — max_subscribers was N({destinations}) + \
             reserved tap({RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL})",
            max_subscribers + 1,
        );
    }

    /// A channel data service is created once and reopened by every subscriber
    /// (each destination + a subprocess SDK opening the same name). iceoryx2
    /// rejects reopening with a LARGER buffer than the existing service —
    /// `DoesNotSupportRequestedMinBufferSize`, the exact crash the drone-racer
    /// pilot hit — but accepts reopening with a SMALLER one. Channel sizing relies
    /// on this: create the service at the channel's declared depth and every
    /// shallower reopen fits, regardless of wiring order. If a future iceoryx2
    /// changes this open-validation behavior, this test is the trip-wire.
    #[test]
    fn channel_service_reopen_larger_fails_smaller_succeeds() {
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let subs = 2usize;

        // Bug shape: a shallow-depth open creates the service first, then a
        // deeper reopen is rejected.
        let bug_name = unique_service_name("reopen_bug");
        let _shallow = node
            .open_or_create_service(&bug_name, subs, 4, true)
            .expect("create channel service at depth 4");
        assert!(
            node.open_or_create_service(&bug_name, subs, 64, true).is_err(),
            "reopening the channel service with a deeper buffer must fail — \
             this is the DoesNotSupportRequestedMinBufferSize crash the \
             channel-depth sizing prevents",
        );

        // Fix shape: create at the deepest depth first, then every shallower
        // reopen succeeds cleanly.
        let fixed_name = unique_service_name("reopen_fixed");
        let _deep = node
            .open_or_create_service(&fixed_name, subs, 64, true)
            .expect("create channel service at depth 64");
        node.open_or_create_service(&fixed_name, subs, 4, true)
            .expect("reopening the channel service with a shallower buffer must succeed");
    }

    /// With `enable_safe_overflow(true)` (the engine-wide realtime
    /// default), the iceoryx2 subscriber buffer auto-evicts the oldest
    /// sample on overflow and the publisher's `send()` never blocks.
    /// Sends `depth * 3` samples to a depth-N service whose subscriber
    /// is attached but never drains; every publish must return promptly.
    /// Attaching a non-draining subscriber is load-bearing — iceoryx2's
    /// publisher only observes back-pressure once at least one
    /// subscriber is present (without one, samples are dropped on the
    /// floor at send time regardless of the overflow flag).
    ///
    /// Mentally-revert: drop the `.enable_safe_overflow(value)` line
    /// in [`Iceoryx2Node::open_or_create_service`] and this test stays
    /// green by accident (iceoryx2 0.8.1's static_config default is
    /// also `true`). The companion test
    /// [`overflow_disabled_publisher_back_pressures_on_full_buffer`]
    /// is the load-bearing half — the false-path locks the contract.
    #[test]
    fn overflow_enabled_publisher_does_not_block_on_full_buffer() {
        use std::time::{Duration, Instant};

        let depth: usize = 4;
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_service(
                &unique_service_name("overflow_true"),
                2,
                depth,
                /* enable_safe_overflow */ true,
            )
            .expect("open service");
        let publisher = service.create_publisher(64).expect("publisher");
        // Subscriber attached but never read — the buffer fills against
        // it. Required for the publisher to observe back-pressure at
        // all (without a subscriber, sends silently no-op).
        let _subscriber = service.create_subscriber().expect("subscriber");

        let start = Instant::now();
        for _ in 0..(depth * 3) {
            let sample = publisher.loan_slice_uninit(8).expect("loan");
            let sample = sample.write_from_slice(&[0u8; 8]);
            sample.send().expect("send must succeed with overflow on");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "overflow-on publisher must not block — sent {} samples to a depth-{} ring \
             in {:?}, expected sub-200ms",
            depth * 3,
            depth,
            elapsed,
        );
    }

    /// With `enable_safe_overflow(false)`, the iceoryx2 service holds
    /// the buffer at capacity and the publisher's send must observe
    /// the back-pressure contract. iceoryx2's per-publisher default
    /// `unable_to_deliver_strategy` may yield either a `Block` (send
    /// blocks until consumer drains) or a non-`Ok` return; both honor
    /// the "producer is not silently silently dropping under the
    /// overflow-off contract" invariant we promise muxer / file-writer
    /// callers. This test fills the buffer past depth and asserts the
    /// (depth+1)th send is observably back-pressured — either it
    /// errors out OR it does not complete promptly relative to the
    /// trivially-completing overflow-on baseline.
    ///
    /// Mentally-revert: drop the `.enable_safe_overflow(value)` line
    /// in [`Iceoryx2Node::open_or_create_service`] and this test
    /// fails — the service falls back to iceoryx2's default
    /// `enable_safe_overflow=true` and the (depth+1)th send returns
    /// promptly via oldest-eviction, breaking the back-pressure
    /// contract mp4-style sinks rely on.
    #[test]
    fn overflow_disabled_publisher_back_pressures_on_full_buffer() {
        use std::sync::mpsc;
        use std::time::Duration;

        let depth: usize = 4;
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service_name = unique_service_name("overflow_false");

        // Open with overflow disabled — back-pressure on.
        let service_for_main = node
            .open_or_create_service(&service_name, 2, depth, /* enable_safe_overflow */ false)
            .expect("open service");
        drop(service_for_main); // keep the service alive only via the
        // worker-side reopen; iceoryx2 services
        // are reference-counted.

        // iceoryx2 Publishers hold `Rc<>` internally and aren't `Send`,
        // so the publisher must be created on the worker thread. Pre-
        // fill the buffer on the worker, then attempt one more send;
        // the (depth+1)th send is the test signal.
        let (filled_tx, filled_rx) = mpsc::channel::<()>();
        let (result_tx, result_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let node_clone = node.clone();
        let service_name_clone = service_name.clone();
        let _worker = std::thread::Builder::new()
            .name("overflow-test-publisher".into())
            .spawn(move || {
                let svc = match node_clone.open_or_create_service(
                    &service_name_clone,
                    2,
                    depth,
                    /* enable_safe_overflow */ false,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = result_tx.send(Err(format!("reopen failed: {e:?}")));
                        return;
                    }
                };
                let publisher = match svc.create_publisher(64) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = result_tx.send(Err(format!("publisher failed: {e:?}")));
                        return;
                    }
                };
                // Attach a never-draining subscriber so the publisher
                // observes back-pressure on overflow. Without it,
                // iceoryx2 drops samples on the floor at send time and
                // the overflow flag never engages — same gotcha as the
                // overflow-on companion test.
                let _subscriber = match svc.create_subscriber() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = result_tx.send(Err(format!("subscriber failed: {e:?}")));
                        return;
                    }
                };
                for i in 0..depth {
                    match publisher.loan_slice_uninit(8) {
                        Ok(sample) => {
                            let sample = sample.write_from_slice(&[0u8; 8]);
                            if let Err(e) = sample.send() {
                                let _ =
                                    result_tx.send(Err(format!("pre-fill send {i} failed: {e:?}")));
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = result_tx.send(Err(format!("pre-fill loan {i} failed: {e:?}")));
                            return;
                        }
                    }
                }
                // Buffer is now at `depth`. Signal main and attempt
                // the (depth+1)th send — this is what the main thread
                // observes for back-pressure.
                let _ = filled_tx.send(());
                let res = (|| -> std::result::Result<(), String> {
                    let sample = publisher
                        .loan_slice_uninit(8)
                        .map_err(|e| format!("loan failed: {e:?}"))?;
                    let sample = sample.write_from_slice(&[0u8; 8]);
                    sample.send().map_err(|e| format!("send failed: {e:?}"))?;
                    Ok(())
                })();
                let _ = result_tx.send(res);
            })
            .expect("spawn worker");

        // Wait for the worker to finish pre-filling the buffer.
        filled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should finish pre-filling within 2s");

        // The (depth+1)th send is the test signal. Back-pressure
        // honors either of:
        //   - iceoryx2 Block strategy: send blocks, worker doesn't
        //     send a result → timeout fires (test passes; worker is
        //     leaked, dropped at process exit).
        //   - iceoryx2 DiscardSample / explicit PublisherSendError:
        //     worker delivers Err on result_tx (test passes — explicit
        //     back-pressure signal).
        // Silent Ok would mean overflow wasn't actually disabled.
        match result_rx.recv_timeout(Duration::from_millis(400)) {
            Ok(Ok(())) => {
                panic!(
                    "(depth+1)th publisher.send() completed successfully with \
                     enable_safe_overflow(false) — back-pressure contract \
                     violated: producer must block or surface an error, not \
                     silently succeed."
                );
            }
            Ok(Err(_back_pressure_signal)) => {
                // Explicit error from iceoryx2 — test passes.
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Worker is blocking inside send() — test passes.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("worker thread panicked unexpectedly");
            }
        }
    }

    /// `Iceoryx2Service` stores the configured ring depth and exposes it
    /// via [`Iceoryx2Service::max_queued_messages`]. Reverting the
    /// field-storage path (e.g. ignoring the argument and hardcoding 16)
    /// trips this test.
    #[test]
    fn data_service_records_configured_max_queued_messages() {
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_service(&unique_service_name("mqm_recorded"), 2, 42, true)
            .expect("open data service");
        assert_eq!(
            service.max_queued_messages(),
            42,
            "service should record the depth it was opened with"
        );
    }

    /// End-to-end smoke test for the 200 Hz two-stage pipeline shape that
    /// motivated this engine fix (the MAVLink stress test in PR #836:
    /// UdpSource → Decoder → Encoder → UdpSink at 200 Hz, where each
    /// stage's drain window was tight against scheduler jitter).
    ///
    /// Producer publishes at 200 Hz to service S1; a relay thread drains
    /// S1 and republishes to S2 while periodically pausing to simulate
    /// downstream jitter; a consumer drains S2 and counts. We run the
    /// pipeline twice:
    ///
    /// 1. **Shallow rings (depth 4)** — during a 50 ms relay pause, the
    ///    producer emits ~10 messages at 200 Hz. The S1 ring (depth 4)
    ///    overflows; ~6 messages per pause are overwritten and lost.
    /// 2. **Deep rings (depth 64, matching MavlinkMessage's declared
    ///    `max_queued_messages`)** — the same 10-message accumulation
    ///    fits comfortably; zero loss.
    ///
    /// Reverting the engine's `subscriber_max_buffer_size` wiring to a
    /// hardcoded constant would either make both runs lose the same way
    /// (low constant) or both runs preserve everything (high constant) —
    /// the asymmetric outcome locks the per-service plumbing.
    ///
    /// Runtime ~1.5 s. Two thread pools per run.
    #[test]
    fn sustained_200hz_two_stage_relay_preserves_messages_only_with_deep_rings() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::time::{Duration, Instant};

        fn run_relay(
            s1_depth: usize,
            s2_depth: usize,
            total: u32,
            hz: u32,
            relay_pause_every: u32,
            relay_pause_ms: u64,
        ) -> (u32, u32) {
            let interval = Duration::from_micros(1_000_000 / hz as u64);
            // iceoryx2 Publishers/Subscribers are `!Send` (they hold Rc
            // internally), so each thread constructs its own ports from
            // the shared Node + service-name string.
            let node = Iceoryx2Node::new().expect("create iceoryx2 node");
            let s1_name = Arc::new(unique_service_name("relay_s1"));
            let s2_name = Arc::new(unique_service_name("relay_s2"));

            let sent_counter = Arc::new(AtomicU32::new(0));
            // Startup barrier — iceoryx2 doesn't queue messages for late
            // subscribers, so the producer must wait until the relay
            // subscriber and consumer subscriber are both attached
            // before publishing the first sample. 3 participants:
            // producer, relay, consumer.
            let startup = Arc::new(std::sync::Barrier::new(3));
            // Two-phase shutdown: relay stops first (drain S1 → S2),
            // then consumer stops (drain S2). One shared flag would
            // race: the consumer's "drain and exit" can finish before
            // the relay republishes the last message.
            let relay_stop = Arc::new(AtomicBool::new(false));
            let consumer_stop = Arc::new(AtomicBool::new(false));

            let node_p = node.clone();
            let s1_p = s1_name.clone();
            let sent_clone = sent_counter.clone();
            let startup_p = startup.clone();
            let producer = std::thread::spawn(move || {
                let svc = node_p
                    .open_or_create_service(&s1_p, 2, s1_depth, true)
                    .expect("producer s1 open");
                let publisher = svc.create_publisher(64).expect("publisher");
                startup_p.wait();
                let start = Instant::now();
                for i in 0..total {
                    let mut payload = vec![0u8; FRAME_HEADER_SIZE + 4];
                    payload[FRAME_HEADER_SIZE..].copy_from_slice(&i.to_le_bytes());
                    let sample = publisher.loan_slice_uninit(payload.len()).expect("loan");
                    let sample = sample.write_from_slice(&payload);
                    sample.send().expect("send");
                    sent_clone.fetch_add(1, Ordering::Relaxed);

                    let target = start + interval * (i + 1);
                    let now = Instant::now();
                    if target > now {
                        std::thread::sleep(target - now);
                    }
                }
            });

            let node_r = node.clone();
            let s1_r = s1_name.clone();
            let s2_r = s2_name.clone();
            let relay_stop_t = relay_stop.clone();
            let startup_r = startup.clone();
            let relay = std::thread::spawn(move || {
                let svc_in = node_r
                    .open_or_create_service(&s1_r, 2, s1_depth, true)
                    .expect("relay s1 open");
                let svc_out = node_r
                    .open_or_create_service(&s2_r, 2, s2_depth, true)
                    .expect("relay s2 open");
                let subscriber = svc_in.create_subscriber().expect("relay sub");
                let publisher = svc_out.create_publisher(64).expect("relay pub");
                startup_r.wait();
                let mut count: u32 = 0;
                let relay_one =
                    |subscriber: &iceoryx2::port::subscriber::Subscriber<
                        ipc::Service,
                        [u8],
                        (),
                    >,
                     publisher: &iceoryx2::port::publisher::Publisher<ipc::Service, [u8], ()>|
                     -> bool {
                        match subscriber.receive() {
                            Ok(Some(sample)) => {
                                let bytes = sample.payload().to_vec();
                                let s = publisher
                                    .loan_slice_uninit(bytes.len())
                                    .expect("relay loan");
                                let s = s.write_from_slice(&bytes);
                                s.send().expect("relay send");
                                true
                            }
                            _ => false,
                        }
                    };
                loop {
                    if relay_stop_t.load(Ordering::Relaxed) {
                        // Drain anything still pending so the very last
                        // producer message makes it to s2.
                        while relay_one(&subscriber, &publisher) {}
                        break;
                    }
                    if relay_one(&subscriber, &publisher) {
                        count += 1;
                        if relay_pause_every > 0 && count % relay_pause_every == 0 {
                            std::thread::sleep(Duration::from_millis(relay_pause_ms));
                        }
                    } else {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
            });

            let node_c = node.clone();
            let s2_c = s2_name.clone();
            let consumer_stop_t = consumer_stop.clone();
            let startup_c = startup.clone();
            let consumer_handle = std::thread::spawn(move || {
                let svc = node_c
                    .open_or_create_service(&s2_c, 2, s2_depth, true)
                    .expect("consumer s2 open");
                let subscriber = svc.create_subscriber().expect("consumer sub");
                startup_c.wait();
                let mut received: u32 = 0;
                loop {
                    if consumer_stop_t.load(Ordering::Relaxed) {
                        while let Ok(Some(_)) = subscriber.receive() {
                            received += 1;
                        }
                        break;
                    }
                    match subscriber.receive() {
                        Ok(Some(_)) => received += 1,
                        Ok(None) => std::thread::sleep(Duration::from_micros(200)),
                        Err(_) => break,
                    }
                }
                received
            });

            // Phased shutdown: producer → settle → relay finishes flushing
            // S1 into S2 → settle → consumer drains S2. Using one shared
            // flag races on the very last message.
            producer.join().expect("producer thread");
            std::thread::sleep(Duration::from_millis(500));
            relay_stop.store(true, Ordering::Relaxed);
            relay.join().expect("relay thread");
            std::thread::sleep(Duration::from_millis(100));
            consumer_stop.store(true, Ordering::Relaxed);
            let received = consumer_handle.join().expect("consumer thread");

            (sent_counter.load(Ordering::Relaxed), received)
        }

        let total: u32 = 100; // 0.5 s at 200 Hz
        let hz: u32 = 200;
        let pause_every: u32 = 25;
        let pause_ms: u64 = 50;

        let (sent_shallow, recv_shallow) = run_relay(4, 4, total, hz, pause_every, pause_ms);
        let (sent_deep, recv_deep) = run_relay(64, 64, total, hz, pause_every, pause_ms);

        assert_eq!(sent_shallow, total, "producer should send every message");
        assert_eq!(sent_deep, total, "producer should send every message");

        assert!(
            recv_shallow < sent_shallow,
            "depth-4 rings should lose messages at 200 Hz under {pause_ms} ms relay pauses (every {pause_every} msgs): sent {sent_shallow}, recv {recv_shallow}"
        );
        assert_eq!(
            recv_deep, sent_deep,
            "depth-64 rings (MavlinkMessage's declared depth) should preserve every message at 200 Hz under the same jitter: sent {sent_deep}, recv {recv_deep}"
        );
    }

    /// End-to-end overwrite behavior: a depth-N ring published with N+1
    /// unread samples drops the oldest in favor of the newest. Locks the
    /// fact that the `subscriber_max_buffer_size` we pass through actually
    /// reaches iceoryx2 — if the wiring is dropped (hardcoded to 16 again),
    /// publishing 17+ messages would only start dropping at the much
    /// larger default and this test would fail.
    #[test]
    fn data_service_honors_small_ring_depth_with_overwrite() {
        let depth: usize = 4;
        let send_count: usize = depth + 3; // 3 extra overwrites
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_service(&unique_service_name("ring_overwrite"), 2, depth, true)
            .expect("open data service");

        let max_payload = 64usize;
        let publisher = service
            .create_publisher(max_payload)
            .expect("create publisher");
        let subscriber = service.create_subscriber().expect("create subscriber");

        for i in 0..send_count {
            let mut payload = vec![0u8; FRAME_HEADER_SIZE + 1];
            payload[FRAME_HEADER_SIZE] = i as u8;
            let sample = publisher
                .loan_slice_uninit(payload.len())
                .expect("loan slot");
            let sample = sample.write_from_slice(&payload);
            sample.send().expect("send must succeed even on overwrite");
        }

        // Drain everything currently in the subscriber's queue.
        let mut received: Vec<u8> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            match subscriber.receive() {
                Ok(Some(sample)) => {
                    received.push(sample.payload()[FRAME_HEADER_SIZE]);
                }
                Ok(None) => break,
                Err(e) => panic!("subscriber.receive() failed: {e:?}"),
            }
        }

        assert!(
            received.len() <= depth,
            "ring depth {depth} must cap the subscriber-side queue; received {} samples",
            received.len()
        );
        assert!(
            !received.is_empty(),
            "subscriber should have received at least one of the published samples"
        );
        // The newest-sent payload must be among what the subscriber drained —
        // "latest wins" semantics: even after overwrites the freshest sample
        // is preserved.
        let newest_sent = (send_count - 1) as u8;
        assert!(
            received.contains(&newest_sent),
            "newest-sent sample (payload byte {newest_sent}) should survive ring overwrites, drained: {received:?}"
        );
    }
}
