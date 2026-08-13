// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::{Mutex, RwLock};
use std::sync::{Arc, LazyLock, Weak};

use super::events::{Event, EventListener, topics};

/// Process-wide pub/sub handle.
pub static PUBSUB: LazyLock<PubSub> = LazyLock::new(PubSub::new);

/// Stated once so the release log and the debug assertion cannot drift apart.
const TEMPORARY_ARC_SUBSCRIBE_DIAGNOSIS: &str = "the listener will be dropped immediately and never receive events. Store the Arc \
     in a variable that outlives the subscription.";

/// One listener's registration: the topic it asked for, and a weak handle to it.
///
/// Weak, so dropping the caller's `Arc` unsubscribes with no bookkeeping — the
/// entry is pruned by the next publish that finds it dead.
struct PubSubTopicSubscription {
    topic: String,
    listener_weak: Weak<Mutex<dyn EventListener>>,
}

impl PubSubTopicSubscription {
    /// Whether an event published to `published_topic` reaches this listener.
    ///
    /// A wildcard subscriber matches everything, every other subscriber matches
    /// its own topic, and either way a subscription is visited once — a wildcard
    /// listener does not also receive a second copy through the specific topic.
    fn receives(&self, published_topic: &str) -> bool {
        self.topic == topics::ALL || self.topic == published_topic
    }
}

/// In-process pub/sub for control-plane events.
///
/// Subscribing is synchronous: a registration is visible to the next publish on
/// any thread the moment [`PubSub::subscribe`] returns, so an event caused after
/// subscribing is delivered. There is no service to open, no connection to
/// establish, and no window in which a subscription exists but cannot receive.
///
/// Control plane only, and in-process by construction: every publisher and every
/// listener lives in the app process, and an out-of-process observer reads the
/// control plane's `/ws/events` rather than this. Cross-process data movement is
/// the iceoryx2 channel plane, which this does not touch.
pub struct PubSub {
    subscriptions: RwLock<Vec<PubSubTopicSubscription>>,
}

impl Default for PubSub {
    fn default() -> Self {
        Self::new()
    }
}

impl PubSub {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(Vec::new()),
        }
    }

    /// Subscribe a listener to a topic.
    ///
    /// The registry holds only a `Weak`, so the caller MUST keep the `Arc` alive
    /// for the lifetime of the subscription; dropping it unsubscribes.
    pub fn subscribe(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) {
        // Caller must keep a strong Arc — the registry stores only a Weak.
        // strong_count == 1 means this parameter is the only reference and will
        // be dropped when this call returns.
        if Arc::strong_count(&listener) <= 1 {
            tracing::error!(
                "PUBSUB.subscribe() called with a temporary Arc for topic '{}' — {}",
                topic,
                TEMPORARY_ARC_SUBSCRIBE_DIAGNOSIS,
            );
            debug_assert!(
                false,
                "PUBSUB.subscribe() called with a temporary Arc for topic '{}' — {}",
                topic, TEMPORARY_ARC_SUBSCRIBE_DIAGNOSIS,
            );
        }

        self.subscriptions.write().push(PubSubTopicSubscription {
            topic: topic.to_string(),
            listener_weak: Arc::downgrade(&listener),
        });

        tracing::debug!("Listener subscribed to topic '{}'", topic);
    }

    /// How many registrations the bus is holding, live or not yet pruned.
    #[cfg(test)]
    pub(crate) fn registration_count(&self) -> usize {
        self.subscriptions.read().len()
    }

    /// Publish an event to every listener subscribed to `topic` or to
    /// [`topics::ALL`], on the calling thread.
    pub fn publish(&self, topic: &str, event: &Event) {
        // Take strong handles under the read lock and release it before
        // dispatching: `on_event` may subscribe, and holding the lock across a
        // callback would deadlock the publisher against its own listener.
        let mut recipients = Vec::new();
        let mut found_dead_subscription = false;
        {
            let subscriptions = self.subscriptions.read();
            for subscription in subscriptions.iter() {
                // Liveness is checked on every entry, not only the matching
                // ones: a subscription on a topic nothing publishes to would
                // otherwise never be visited, and so never pruned.
                match (
                    subscription.receives(topic),
                    subscription.listener_weak.upgrade(),
                ) {
                    (true, Some(listener)) => recipients.push(listener),
                    (false, Some(_)) => {}
                    (_, None) => found_dead_subscription = true,
                }
            }
        }

        for listener in &recipients {
            if let Err(e) = listener.lock().on_event(event) {
                tracing::warn!(
                    "Listener on topic '{}' failed to handle [{}]: {}",
                    topic,
                    event.log_name(),
                    e
                );
            }
        }

        if found_dead_subscription {
            self.subscriptions
                .write()
                .retain(|subscription| subscription.listener_weak.strong_count() > 0);
        }

        tracing::debug!(
            "Published [{}] to topic [{}] ({} listener(s))",
            event.log_name(),
            topic,
            recipients.len()
        );
    }
}
