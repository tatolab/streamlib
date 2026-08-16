// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::sync::{Arc, LazyLock, OnceLock, Weak};
use std::time::Duration;

use super::events::{Event, EventListener, topics};
use crate::core::error::{Error, Result};
use crate::iceoryx2::{EventPayload, Iceoryx2EventService, Iceoryx2Node, MAX_EVENT_PAYLOAD_SIZE};

type EventPublisher =
    iceoryx2::port::publisher::Publisher<iceoryx2::service::ipc::Service, EventPayload, ()>;

thread_local! {
    /// Per-thread cache of iceoryx2 publishers keyed by service name.
    ///
    /// iceoryx2 Publisher uses Rc internally (!Send), so it cannot be stored
    /// in shared state. thread_local satisfies the !Send constraint while
    /// keeping publishers alive so sent samples remain in shared memory
    /// for subscribers to receive.
    static PUBLISHER_CACHE: RefCell<HashMap<String, (Iceoryx2EventService, EventPublisher)>> =
        RefCell::new(HashMap::new());
}

/// Process-wide pub/sub handle.
pub static PUBSUB: LazyLock<PubSub> = LazyLock::new(PubSub::new);

/// How long [`PubSub::subscribe`] waits for one subscriber to register before
/// giving up and returning anyway.
///
/// Generous against the establishment path's own bound (ten `open_or_create`
/// attempts, 20ms apart): elapsing means iceoryx2 is wedged, not merely slow,
/// and blocking the caller forever is worse than a logged loss of events.
/// Bounds a single subscription, not a batch — [`PubSub::init`] replays pending
/// subscriptions serially, so a wedged iceoryx2 costs it this much per pending
/// listener.
const SUBSCRIBER_ESTABLISHMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether a subscriber beat its caller's timeout. Settled by one
/// compare-exchange from each side, so the caller and the subscriber thread can
/// never both believe they won.
const ESTABLISHMENT_PENDING: u8 = 0;
const ESTABLISHMENT_CONFIRMED: u8 = 1;
const ESTABLISHMENT_ABANDONED: u8 = 2;

/// iceoryx2-backed pub/sub for runtime events.
pub struct PubSub {
    // Set once via init()
    runtime_id: OnceLock<String>,
    node: OnceLock<Iceoryx2Node>,
    // Subscriptions registered before init() — replayed when init() is called
    pending_subscriptions: Mutex<Vec<(String, Arc<Mutex<dyn EventListener>>)>>,
}

impl Default for PubSub {
    fn default() -> Self {
        Self::new()
    }
}

impl PubSub {
    pub fn new() -> Self {
        Self {
            runtime_id: OnceLock::new(),
            node: OnceLock::new(),
            pending_subscriptions: Mutex::new(Vec::new()),
        }
    }

    /// Initialize with iceoryx2 backend. Called once from Runner::new().
    ///
    /// Replays any subscriptions that were registered before initialization.
    /// Every pending subscription is attempted even if an earlier one fails —
    /// dropping the rest would strand listeners the caller cannot re-register,
    /// since the replay queue has already been taken.
    pub fn init(&self, runtime_id: &str, node: Iceoryx2Node) -> Result<()> {
        // Publishing the backend and draining the queue under one lock is what
        // makes `subscribe`'s buffer-or-establish decision safe: `node` is set
        // before `runtime_id` (the gate `subscribe` reads), and a subscriber
        // that loses the race sees the initialized state rather than pushing
        // onto a queue nobody will drain again.
        let pending = {
            let mut pending = self.pending_subscriptions.lock();
            let _ = self.node.set(node);
            let _ = self.runtime_id.set(runtime_id.to_string());
            std::mem::take(&mut *pending)
        };

        tracing::info!("PUBSUB initialized for runtime '{}'", runtime_id);

        let mut first_failure = None;
        for (topic, listener) in pending {
            tracing::debug!("Replaying pending subscription for topic '{}'", topic);
            if let Err(e) = self.subscribe_inner(&topic, listener) {
                first_failure.get_or_insert(e);
            }
        }
        match first_failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Subscribe a listener to a topic, returning `Ok` once the subscriber is
    /// registered — so an event published after that is delivered.
    ///
    /// Blocks for as long as establishment takes because the event service
    /// carries no history: a sample sent before the subscriber registers reaches
    /// nobody and cannot be replayed. Before `init()` the subscription is
    /// buffered and this returns `Ok` immediately; establishment — and any
    /// error from it — surfaces from `init()` instead.
    ///
    /// `Err` means the listener is not subscribed and never will be: either the
    /// subscriber could not be created, or it was still coming up after
    /// `SUBSCRIBER_ESTABLISHMENT_TIMEOUT`. Publishing to the topic afterwards
    /// reaches this listener not at all, so a caller that ignores the error is
    /// silently deaf rather than degraded.
    ///
    /// The subscriber thread holds only a Weak reference to the listener.
    /// The caller MUST keep the Arc alive for the lifetime of the subscription.
    /// When the Arc is dropped, the subscriber thread exits automatically.
    ///
    /// ```ignore
    /// // WRONG — Arc dropped immediately, listener never receives events:
    /// PUBSUB.subscribe(topic, Arc::new(Mutex::new(listener)));
    ///
    /// // RIGHT — Arc stored, subscription lives until variable is dropped:
    /// let sub = Arc::new(Mutex::new(listener));
    /// PUBSUB.subscribe(topic, Arc::clone(&sub));
    /// ```
    pub fn subscribe(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) -> Result<()> {
        // Caller must keep a strong Arc — we only store a Weak in the
        // subscriber thread.  strong_count == 1 means this parameter is the
        // only reference and will be dropped when this call returns.
        debug_assert!(
            Arc::strong_count(&listener) > 1,
            "PUBSUB.subscribe() called with a temporary Arc for topic '{}' — \
             the listener will be dropped immediately and never receive events. \
             Store the Arc in a variable that outlives the subscription.",
            topic,
        );
        if Arc::strong_count(&listener) <= 1 {
            tracing::error!(
                "PUBSUB.subscribe() called with a temporary Arc for topic '{}' — \
                 the listener will be dropped immediately and never receive events",
                topic,
            );
        }

        // Decide and buffer under the lock `init` drains behind, so a
        // subscription can never be pushed onto an already-taken queue.
        let listener = {
            let mut pending = self.pending_subscriptions.lock();
            if self.runtime_id.get().is_none() {
                tracing::debug!(
                    "PUBSUB not initialized, buffering subscription for '{}'",
                    topic
                );
                pending.push((topic.to_string(), listener));
                return Ok(());
            }
            listener
        };

        self.subscribe_inner(topic, listener)
    }

    fn subscribe_inner(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) -> Result<()> {
        // `init` publishes `node` before `runtime_id`, and both callers reach
        // here only after observing `runtime_id`, so these are set.
        let (Some(runtime_id), Some(node)) = (self.runtime_id.get(), self.node.get()) else {
            return Err(Error::Runtime(format!(
                "PUBSUB backend missing while subscribing to '{topic}'"
            )));
        };
        let runtime_id = runtime_id.clone();
        let node = node.clone();
        let weak_listener = Arc::downgrade(&listener);
        let topic_owned = topic.to_string();

        let service_name = topic_to_service_name(&runtime_id, topic);
        let service_name_for_log = service_name.clone();

        let (subscriber_ready_sender, subscriber_ready_receiver) = sync_channel(1);

        // The channel wakes the caller; this settles who won when the wake-up
        // and the timeout land together. Exactly one of the two transitions out
        // of PENDING succeeds, which is what lets `Err` mean "not subscribed"
        // with no window where it also means "subscribed a moment too late".
        let establishment = Arc::new(AtomicU8::new(ESTABLISHMENT_PENDING));
        let establishment_for_subscriber = Arc::clone(&establishment);

        // Spawn a dedicated OS thread for polling.
        // iceoryx2 Subscriber uses Rc internally (!Send), so it must be
        // created and used on the same thread.
        let builder = std::thread::Builder::new().name(format!("pubsub-{}", topic));
        if let Err(e) = builder.spawn(move || {
            // Retry `open_or_create` — iceoryx2 can transiently report
            // `ServiceInCorruptedState` when a concurrent node (e.g. another
            // streamlib process or another test binary on the same machine)
            // is scanning/cleaning dead-node state under `/tmp/iceoryx2/`.
            // The state stabilizes within a few tens of milliseconds.
            let mut service = None;
            for attempt in 0..10 {
                match node.open_or_create_event_service(&service_name) {
                    Ok(s) => {
                        service = Some(s);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to create event service '{}' (attempt {}): {}",
                            service_name,
                            attempt + 1,
                            e
                        );
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
            let Some(service) = service else {
                tracing::error!(
                    "Giving up after 10 attempts to create event service '{}'",
                    service_name
                );
                return;
            };

            let subscriber = match service.create_subscriber() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to create subscriber for '{}': {}", service_name, e);
                    return;
                }
            };

            // Registered in the service's dynamic config: every publisher picks
            // this port up on its next `send()`, so the caller may publish now.
            if establishment_for_subscriber
                .compare_exchange(
                    ESTABLISHMENT_PENDING,
                    ESTABLISHMENT_CONFIRMED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                // The caller already gave up and was told it has no
                // subscription. Drop this one rather than leave it polling
                // against a listener nobody believes is subscribed.
                tracing::debug!(
                    "Subscriber for topic '{}' came up after its caller gave up; closing it",
                    topic_owned
                );
                return;
            }
            let _ = subscriber_ready_sender.send(());

            subscriber_poll_loop(&subscriber, &weak_listener, &topic_owned);
        }) {
            return Err(Error::Runtime(format!(
                "Failed to spawn subscriber thread for '{service_name_for_log}': {e}"
            )));
        }

        // The sender is owned by the spawned thread, so a give-up path drops it
        // and disconnects rather than stranding this caller until the timeout.
        match subscriber_ready_receiver.recv_timeout(SUBSCRIBER_ESTABLISHMENT_TIMEOUT) {
            Ok(()) => {
                tracing::debug!(
                    "Listener subscribed to topic '{}' (service: {})",
                    topic,
                    service_name_for_log
                );
                Ok(())
            }
            Err(RecvTimeoutError::Disconnected) => Err(Error::Runtime(format!(
                "Subscriber for '{service_name_for_log}' never came up; \
                 events on topic '{topic}' would be missed"
            ))),
            Err(RecvTimeoutError::Timeout) => {
                if establishment
                    .compare_exchange(
                        ESTABLISHMENT_PENDING,
                        ESTABLISHMENT_ABANDONED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    // It registered in the instant we timed out, so the
                    // subscription is real and reporting failure would be wrong.
                    tracing::debug!(
                        "Subscriber for '{}' registered as the timeout elapsed",
                        service_name_for_log
                    );
                    return Ok(());
                }
                Err(Error::Runtime(format!(
                    "Subscriber for '{service_name_for_log}' was still coming up after \
                     {SUBSCRIBER_ESTABLISHMENT_TIMEOUT:?}; events on topic '{topic}' would be missed"
                )))
            }
        }
    }

    /// Publish event to topic (serializes and sends via iceoryx2).
    ///
    /// Events are dispatched to:
    /// 1. All subscribers of the specific topic
    /// 2. All subscribers of `topics::ALL` (wildcard)
    pub fn publish(&self, topic: &str, event: &Event) {
        let Some(runtime_id) = self.runtime_id.get() else {
            tracing::trace!(
                "PUBSUB not initialized, dropping event: {}",
                event.log_name()
            );
            return;
        };

        // Serialize event to MessagePack
        let bytes = match rmp_serde::to_vec_named(event) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Failed to serialize event: {}", e);
                return;
            }
        };

        if bytes.len() > MAX_EVENT_PAYLOAD_SIZE {
            tracing::warn!(
                "Event too large ({} bytes, max {}): {}",
                bytes.len(),
                MAX_EVENT_PAYLOAD_SIZE,
                event.log_name()
            );
            return;
        }

        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);

        let payload = EventPayload::new(topic, timestamp_ns, &bytes);

        // Send to topic-specific service
        self.send_payload(runtime_id, topic, &payload);

        // Also send to /all aggregate service (if not already wildcard)
        if topic != topics::ALL {
            self.send_payload(runtime_id, topics::ALL, &payload);
        }

        tracing::debug!(
            "Published [{}] to topic [{}] ({} bytes)",
            event.log_name(),
            topic,
            bytes.len()
        );
    }

    fn send_payload(&self, runtime_id: &str, topic: &str, payload: &EventPayload) {
        let service_name = topic_to_service_name(runtime_id, topic);
        let node = self.node.get().unwrap();

        PUBLISHER_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();

            // Get or create a cached publisher for this service name.
            // Publishers must stay alive so sent samples remain in shared memory.
            if !cache.contains_key(&service_name) {
                // Same `ServiceInCorruptedState` retry as in `subscribe_inner`
                // — iceoryx2 dead-node cleanup can transiently flag a fresh
                // service as corrupted when concurrent nodes scan at the same
                // time.
                let mut service = None;
                for attempt in 0..10 {
                    match node.open_or_create_event_service(&service_name) {
                        Ok(s) => {
                            service = Some(s);
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to open event service '{}' (attempt {}): {}",
                                service_name,
                                attempt + 1,
                                e
                            );
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                    }
                }
                let Some(service) = service else {
                    tracing::error!(
                        "Giving up after 10 attempts to open event service '{}'",
                        service_name
                    );
                    return;
                };

                let publisher = match service.create_publisher() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("Failed to create publisher for '{}': {}", service_name, e);
                        return;
                    }
                };

                cache.insert(service_name.clone(), (service, publisher));
            }

            let (_, publisher) = cache.get(&service_name).unwrap();

            match publisher.loan_uninit() {
                Ok(sample) => {
                    let sample = sample.write_payload(*payload);
                    if let Err(e) = sample.send() {
                        tracing::warn!("Failed to send event to '{}': {:?}", service_name, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to loan sample for '{}': {:?}", service_name, e);
                }
            }
        });
    }
}

/// Blocking poll loop for an iceoryx2 event subscriber.
///
/// Runs on a dedicated OS thread, polling the subscriber for new events.
/// Exits when the listener is dropped (weak ref upgrade fails).
fn subscriber_poll_loop(
    subscriber: &iceoryx2::port::subscriber::Subscriber<
        iceoryx2::service::ipc::Service,
        EventPayload,
        (),
    >,
    weak_listener: &Weak<Mutex<dyn EventListener>>,
    topic: &str,
) {
    loop {
        // Drain all available events before sleeping
        let mut received_any = false;
        loop {
            match subscriber.receive() {
                Ok(Some(sample)) => {
                    received_any = true;
                    let payload: &EventPayload = &sample;

                    // Deserialize event from MessagePack
                    let event: Event = match rmp_serde::from_slice(payload.data()) {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to deserialize event on topic '{}': {}",
                                topic,
                                e
                            );
                            continue;
                        }
                    };

                    // Deliver to listener (try_lock to avoid blocking, same as old rayon dispatch)
                    if let Some(listener) = weak_listener.upgrade() {
                        if let Some(mut guard) = listener.try_lock() {
                            let _ = guard.on_event(&event);
                        } else {
                            tracing::trace!(
                                "Listener busy on topic '{}', skipping (fire-and-forget)",
                                topic
                            );
                        }
                    } else {
                        // Listener dropped, exit loop
                        tracing::debug!(
                            "Listener dropped for topic '{}', stopping poll thread",
                            topic
                        );
                        return;
                    }
                }
                Ok(None) => {
                    // No more data in buffer
                    break;
                }
                Err(e) => {
                    tracing::warn!("Event subscriber error on topic '{}': {:?}", topic, e);
                    return;
                }
            }
        }

        // Check if listener is still alive before sleeping
        if weak_listener.strong_count() == 0 {
            tracing::debug!(
                "Listener dropped for topic '{}', stopping poll thread",
                topic
            );
            return;
        }

        // Sleep between polls. Events are infrequent (lifecycle, graph changes),
        // so 5ms polling is more than sufficient.
        std::thread::sleep(std::time::Duration::from_millis(5));

        // Yield if we processed events for responsiveness
        if received_any {
            std::thread::yield_now();
        }
    }
}

/// Map a topic string to an iceoryx2 service name.
fn topic_to_service_name(runtime_id: &str, topic: &str) -> String {
    if topic == topics::ALL {
        format!("streamlib/{}/events/all", runtime_id)
    } else {
        // Replace colons with slashes for iceoryx2 service naming
        let sanitized = topic.replace(':', "/");
        format!("streamlib/{}/events/{}", runtime_id, sanitized)
    }
}
