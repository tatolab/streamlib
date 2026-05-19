// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! iceoryx2 Node wrapper for StreamLib runtime.

use std::sync::Arc;

use iceoryx2::node::Node;
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::prelude::*;
use parking_lot::Mutex;

use super::{EventPayload, FRAME_HEADER_SIZE, MAX_FANIN_PER_DESTINATION};
use crate::core::error::{Result, Error};

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
        let node = NodeBuilder::new().create::<ipc::Service>().map_err(|e| {
            Error::Runtime(format!("Failed to create iceoryx2 node: {:?}", e))
        })?;

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
            .map_err(|e| {
                Error::Runtime(format!("Failed to open/create event service: {:?}", e))
            })?;

        Ok(Iceoryx2EventService { inner: service })
    }

    /// Open or create an iceoryx2 Event service for fd-multiplexed wakeups.
    ///
    /// Pairs 1:1 with a destination's pub/sub service (`streamlib/<dest>`) — N upstream
    /// `Notifier`s fan in to one `Listener` whose file descriptor a runner can wait on
    /// via epoll/select instead of busy-polling. Distinct from [`Iceoryx2EventService`]
    /// which is a typed pub/sub for runtime events.
    pub fn open_or_create_notify_service(
        &self,
        service_name: &str,
    ) -> Result<Iceoryx2NotifyService> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .event()
            .max_notifiers(MAX_FANIN_PER_DESTINATION)
            .max_listeners(1)
            .open_or_create()
            .map_err(|e| {
                Error::Runtime(format!("Failed to open/create notify service: {:?}", e))
            })?;

        Ok(Iceoryx2NotifyService { inner: service })
    }

    /// Open or create a publish-subscribe service for `[u8]` slices.
    ///
    /// The service name should follow the format: "streamlib/{source_processor}/{dest_processor}".
    /// `max_queued_messages` caps how many `[u8]` samples any subscriber on this service
    /// can buffer — resolved from the wire schema's `metadata.max_queued_messages` via
    /// [`crate::core::embedded_schemas::max_queued_messages_for_port_spec`], defaulting
    /// to [`crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES`].
    pub fn open_or_create_service(
        &self,
        service_name: &str,
        max_queued_messages: usize,
    ) -> Result<Iceoryx2Service> {
        let node = self.inner.lock();
        let service_name: ServiceName = service_name.try_into().map_err(|e| {
            Error::Configuration(format!("Invalid service name '{}': {:?}", service_name, e))
        })?;

        let service = node
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .max_publishers(MAX_FANIN_PER_DESTINATION)
            .subscriber_max_buffer_size(max_queued_messages)
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
    inner: iceoryx2::service::port_factory::publish_subscribe::PortFactory<
        ipc::Service,
        [u8],
        (),
    >,
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
            .map_err(|e| {
                Error::Runtime(format!("Failed to create event subscriber: {:?}", e))
            })
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

    /// `open_or_create_notify_service` must honor [`MAX_FANIN_PER_DESTINATION`]
    /// in lockstep with the constant — exactly that many notifiers can be
    /// created, and the (cap+1)th must fail. Catches drift if either the
    /// service-builder cap or the constant is changed without the other.
    #[test]
    fn notify_service_max_notifiers_matches_const() {
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_notify_service(&unique_service_name("notify_cap"))
            .expect("open notify service");

        let mut notifiers = Vec::with_capacity(MAX_FANIN_PER_DESTINATION);
        for i in 0..MAX_FANIN_PER_DESTINATION {
            notifiers.push(
                service
                    .create_notifier()
                    .unwrap_or_else(|e| panic!("notifier {i} (under cap) must succeed: {e:?}")),
            );
        }
        assert!(
            service.create_notifier().is_err(),
            "creating notifier {} must fail — service-builder cap drifted from MAX_FANIN_PER_DESTINATION ({})",
            MAX_FANIN_PER_DESTINATION + 1,
            MAX_FANIN_PER_DESTINATION,
        );
    }

    /// `Iceoryx2Service` stores the configured ring depth and exposes it
    /// via [`Iceoryx2Service::max_queued_messages`]. Reverting the
    /// field-storage path (e.g. ignoring the argument and hardcoding 16)
    /// trips this test.
    #[test]
    fn data_service_records_configured_max_queued_messages() {
        let node = Iceoryx2Node::new().expect("create iceoryx2 node");
        let service = node
            .open_or_create_service(&unique_service_name("mqm_recorded"), 42)
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
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::sync::Arc;
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
                    .open_or_create_service(&s1_p, s1_depth)
                    .expect("producer s1 open");
                let publisher = svc.create_publisher(64).expect("publisher");
                startup_p.wait();
                let start = Instant::now();
                for i in 0..total {
                    let mut payload = vec![0u8; FRAME_HEADER_SIZE + 4];
                    payload[FRAME_HEADER_SIZE..]
                        .copy_from_slice(&i.to_le_bytes());
                    let sample = publisher
                        .loan_slice_uninit(payload.len())
                        .expect("loan");
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
                    .open_or_create_service(&s1_r, s1_depth)
                    .expect("relay s1 open");
                let svc_out = node_r
                    .open_or_create_service(&s2_r, s2_depth)
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
                     publisher: &iceoryx2::port::publisher::Publisher<
                        ipc::Service,
                        [u8],
                        (),
                    >|
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
                        if relay_pause_every > 0
                            && count % relay_pause_every == 0
                        {
                            std::thread::sleep(Duration::from_millis(
                                relay_pause_ms,
                            ));
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
                    .open_or_create_service(&s2_c, s2_depth)
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

        let (sent_shallow, recv_shallow) =
            run_relay(4, 4, total, hz, pause_every, pause_ms);
        let (sent_deep, recv_deep) =
            run_relay(64, 64, total, hz, pause_every, pause_ms);

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
            .open_or_create_service(&unique_service_name("ring_overwrite"), depth)
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
