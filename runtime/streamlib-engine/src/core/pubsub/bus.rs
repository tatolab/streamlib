// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
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

/// Attempts [`PubSub::subscribe`]'s subscriber thread makes to open its
/// iceoryx2 service, and the pause between them.
///
/// iceoryx2 can transiently report `ServiceInCorruptedState` while a concurrent
/// node scans dead-node state; it settles within a few tens of milliseconds.
const SERVICE_OPEN_ATTEMPTS: u32 = 10;
const SERVICE_OPEN_RETRY_PAUSE: Duration = Duration::from_millis(20);

/// How long a caller should give a subscription to go live.
///
/// Derived from the retry budget above — [`SERVICE_OPEN_ATTEMPTS`] ×
/// [`SERVICE_OPEN_RETRY_PAUSE`] is a ~200 ms floor before subscriber creation —
/// with an order of magnitude of headroom for a loaded machine, and short enough
/// that a subscription which is never coming up fails rather than hangs.
pub const DEFAULT_SUBSCRIPTION_LIVE_BUDGET: Duration = Duration::from_secs(2);

/// Reports when a subscription registered by [`PubSub::subscribe`] has become
/// live, or why it never got there.
///
/// `subscribe` returning means only that a subscriber thread was spawned. That
/// thread still has to open its iceoryx2 service and create its subscriber, and
/// iceoryx2 does not replay samples published before a subscriber existed — so
/// anything published in between is lost, with nothing on either side to say so.
/// A caller that publishes, or hands a socket to a client that can cause a
/// publish, waits on this first.
///
/// What it promises is that the subscriber exists, so no sample is dropped for
/// want of one. It is not a promise that the very next sample arrives: a
/// publisher created *after* this fires still has its own connection to
/// establish, and iceoryx2 can drop its first sends until that completes.
#[must_use = "a subscription is not live when subscribe returns — wait on this, \
              or bind it to `_` to record that missing early events is acceptable"]
pub struct PubSubSubscriptionLiveSignal {
    topic: String,
    became_live: Receiver<Result<()>>,
}

impl PubSubSubscriptionLiveSignal {
    /// Block until the subscription is live, giving up after `timeout`.
    pub fn wait_until_subscription_is_live(self, timeout: Duration) -> Result<()> {
        match self.became_live.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(RecvTimeoutError::Timeout) => Err(Error::Runtime(format!(
                "subscription to topic '{}' was not live within {timeout:?}",
                self.topic
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(Error::Runtime(format!(
                "subscriber thread for topic '{}' ended before its subscription went live",
                self.topic
            ))),
        }
    }

    /// Await the subscription going live from a tokio task, giving up after
    /// `timeout`.
    ///
    /// The wait is blocking, so it is held off the async worker here rather than
    /// at each call site — the same arrangement `RuntimeOperations::tap_async`
    /// uses for its own subscribe outcome.
    pub async fn wait_until_subscription_is_live_async(self, timeout: Duration) -> Result<()> {
        let topic = self.topic.clone();
        tokio::task::spawn_blocking(move || self.wait_until_subscription_is_live(timeout))
            .await
            .unwrap_or_else(|join_error| {
                Err(Error::Runtime(format!(
                    "subscription wait for topic '{topic}' failed to join: {join_error}"
                )))
            })
    }
}

/// A listener registered with a topic, and the sender its subscriber thread
/// reports liveness through.
struct PubSubSubscriptionRegistration {
    topic: String,
    listener: Arc<Mutex<dyn EventListener>>,
    became_live_sender: SyncSender<Result<()>>,
}

/// iceoryx2-backed pub/sub for runtime events.
pub struct PubSub {
    // Set once via init()
    runtime_id: OnceLock<String>,
    node: OnceLock<Iceoryx2Node>,
    // Subscriptions registered before init() — replayed when init() is called
    pending_subscriptions: Mutex<Vec<PubSubSubscriptionRegistration>>,
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
    pub fn init(&self, runtime_id: &str, node: Iceoryx2Node) {
        // Publishing `runtime_id` and draining the buffer under one lock, which
        // `subscribe` also takes across its own check: otherwise a subscribe
        // that read "not initialized" could push after this drain and never be
        // replayed, leaving its caller to wait out a whole budget for a
        // subscription nothing will ever start.
        let pending = {
            let mut pending_subscriptions = self.pending_subscriptions.lock();
            let _ = self.runtime_id.set(runtime_id.to_string());
            let _ = self.node.set(node);
            std::mem::take(&mut *pending_subscriptions)
        };

        tracing::info!("PUBSUB initialized for runtime '{}'", runtime_id);

        for subscription in pending {
            tracing::debug!(
                "Replaying pending subscription for topic '{}'",
                subscription.topic
            );
            self.subscribe_inner(subscription);
        }
    }

    /// Subscribe a listener to a topic, returning the signal that reports when
    /// the subscription is live.
    ///
    /// The subscriber thread holds only a Weak reference to the listener. The
    /// caller MUST keep the Arc alive for the lifetime of the subscription;
    /// when the Arc is dropped, the subscriber thread exits automatically.
    ///
    /// Returning does not mean the subscription can receive anything yet — see
    /// [`PubSubSubscriptionLiveSignal`]. Ignoring the signal is legitimate for a
    /// caller that cannot lose an event by missing early ones (one that polls a
    /// latch as well, say), and wrong for anything else.
    pub fn subscribe(
        &self,
        topic: &str,
        listener: Arc<Mutex<dyn EventListener>>,
    ) -> PubSubSubscriptionLiveSignal {
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

        // Depth 1: exactly one outcome is ever sent, and the subscriber thread
        // must never block handing it over — a caller that gave up on its
        // timeout has already dropped the receiver.
        let (became_live_sender, became_live) = sync_channel(1);
        let signal = PubSubSubscriptionLiveSignal {
            topic: topic.to_string(),
            became_live,
        };
        let registration = PubSubSubscriptionRegistration {
            topic: topic.to_string(),
            listener,
            became_live_sender,
        };

        // Held across the check so `init` cannot drain the buffer between it
        // and the push below.
        let mut pending_subscriptions = self.pending_subscriptions.lock();
        if self.runtime_id.get().is_none() {
            // Not yet initialized — buffer for replay
            tracing::debug!(
                "PUBSUB not initialized, buffering subscription for '{}'",
                topic
            );
            pending_subscriptions.push(registration);
            return signal;
        }
        drop(pending_subscriptions);

        self.subscribe_inner(registration);
        signal
    }

    fn subscribe_inner(&self, registration: PubSubSubscriptionRegistration) {
        let PubSubSubscriptionRegistration {
            topic,
            listener,
            became_live_sender,
        } = registration;

        let runtime_id = self.runtime_id.get().unwrap().clone();
        let node = self.node.get().unwrap().clone();
        let weak_listener = Arc::downgrade(&listener);
        let topic_owned = topic.clone();

        let service_name = topic_to_service_name(&runtime_id, &topic);
        let service_name_for_log = service_name.clone();

        // Spawn a dedicated OS thread for polling.
        // iceoryx2 Subscriber uses Rc internally (!Send), so it must be
        // created and used on the same thread.
        //
        // A failed spawn drops the closure, and with it the sender it owns, so
        // the outcome has to be reported through a sender that never entered
        // the closure.
        let became_live_sender_if_thread_never_starts = became_live_sender.clone();
        let builder = std::thread::Builder::new().name(format!("pubsub-{}", topic));
        if let Err(e) = builder.spawn(move || {
            // Retry `open_or_create` — iceoryx2 can transiently report
            // `ServiceInCorruptedState` when a concurrent node (e.g. another
            // streamlib process or another test binary on the same machine)
            // is scanning/cleaning dead-node state under `/tmp/iceoryx2/`.
            let mut service = None;
            for attempt in 0..SERVICE_OPEN_ATTEMPTS {
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
                        std::thread::sleep(SERVICE_OPEN_RETRY_PAUSE);
                    }
                }
            }
            let Some(service) = service else {
                tracing::error!(
                    "Giving up after {} attempts to create event service '{}'",
                    SERVICE_OPEN_ATTEMPTS,
                    service_name
                );
                let _ = became_live_sender.send(Err(Error::Runtime(format!(
                    "event service '{service_name}' could not be opened in \
                     {SERVICE_OPEN_ATTEMPTS} attempts"
                ))));
                return;
            };

            let subscriber = match service.create_subscriber() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to create subscriber for '{}': {}", service_name, e);
                    let _ = became_live_sender.send(Err(Error::Runtime(format!(
                        "subscriber for event service '{service_name}' could not be created: {e}"
                    ))));
                    return;
                }
            };

            // Live from here and not one statement earlier: the subscriber
            // exists, so no sample is dropped for want of one. Reporting before
            // this point would report "subscribe was called", which is the
            // state that loses events.
            let _ = became_live_sender.send(Ok(()));

            subscriber_poll_loop(&subscriber, &weak_listener, &topic_owned);
        }) {
            tracing::error!(
                "Failed to spawn subscriber thread for '{}': {}",
                service_name_for_log,
                e
            );
            let _ = became_live_sender_if_thread_never_starts.send(Err(Error::Runtime(format!(
                "subscriber thread for topic '{topic}' could not be spawned: {e}"
            ))));
        } else {
            tracing::debug!(
                "Listener subscribed to topic '{}' (service: {})",
                topic,
                service_name_for_log
            );
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
