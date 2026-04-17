// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock, Weak};

use super::events::{topics, Event, EventListener};
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

/// Global pub/sub instance for runtime events.
///
/// Created as an empty shell via LazyLock. Must be initialized via `init()`
/// during `StreamRuntime::new()` before iceoryx2 services are available.
/// Before init, publish is a no-op and subscribe is buffered.
pub static PUBSUB: LazyLock<PubSub> = LazyLock::new(PubSub::new);

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

    /// Initialize with iceoryx2 backend. Called once from StreamRuntime::new().
    ///
    /// Replays any subscriptions that were registered before initialization.
    pub fn init(&self, runtime_id: &str, node: Iceoryx2Node) {
        let _ = self.runtime_id.set(runtime_id.to_string());
        let _ = self.node.set(node);

        tracing::info!("PUBSUB initialized for runtime '{}'", runtime_id);

        // Replay pending subscriptions
        let pending = std::mem::take(&mut *self.pending_subscriptions.lock());
        for (topic, listener) in pending {
            tracing::debug!("Replaying pending subscription for topic '{}'", topic);
            self.subscribe_inner(&topic, listener);
        }
    }

    /// Subscribe a listener to a topic.
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
    pub fn subscribe(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) {
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

        if self.runtime_id.get().is_none() {
            // Not yet initialized — buffer for replay
            tracing::debug!(
                "PUBSUB not initialized, buffering subscription for '{}'",
                topic
            );
            self.pending_subscriptions
                .lock()
                .push((topic.to_string(), listener));
            return;
        }

        self.subscribe_inner(topic, listener);
    }

    fn subscribe_inner(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) {
        let runtime_id = self.runtime_id.get().unwrap().clone();
        let node = self.node.get().unwrap().clone();
        let weak_listener = Arc::downgrade(&listener);
        let topic_owned = topic.to_string();

        let service_name = topic_to_service_name(&runtime_id, topic);
        let service_name_for_log = service_name.clone();

        // Spawn a dedicated OS thread for polling.
        // iceoryx2 Subscriber uses Rc internally (!Send), so it must be
        // created and used on the same thread.
        let builder = std::thread::Builder::new().name(format!("pubsub-{}", topic));
        if let Err(e) = builder.spawn(move || {
            let service = match node.open_or_create_event_service(&service_name) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to create event service '{}': {}", service_name, e);
                    return;
                }
            };

            let subscriber = match service.create_subscriber() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to create subscriber for '{}': {}", service_name, e);
                    return;
                }
            };

            subscriber_poll_loop(&subscriber, &weak_listener, &topic_owned);
        }) {
            tracing::error!(
                "Failed to spawn subscriber thread for '{}': {}",
                service_name_for_log,
                e
            );
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
                let service = match node.open_or_create_event_service(&service_name) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("Failed to open event service '{}': {}", service_name, e);
                        return;
                    }
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
