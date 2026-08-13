// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::{Mutex, RwLock};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, LazyLock, Weak};

use super::events::{Event, EventListener, topics};

/// Process-wide pub/sub handle.
pub static PUBSUB: LazyLock<PubSub> = LazyLock::new(PubSub::new);

/// Stated once so the release log and the debug assertion cannot drift apart.
const TEMPORARY_ARC_SUBSCRIBE_DIAGNOSIS: &str = "the listener will be dropped immediately and never receive events. Store the Arc \
     in a variable that outlives the subscription.";

/// Deliveries a publisher may run ahead of the dispatcher before it blocks.
///
/// Bounded so a runaway publisher cannot grow the queue without limit. Reaching
/// it means the dispatcher is starved, which for a control-plane bus carrying
/// lifecycle events means a listener is violating its no-blocking contract —
/// back-pressuring the publisher is the honest response, and it is visible,
/// where dropping would not be.
const MAX_QUEUED_DELIVERIES: usize = 4096;

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

/// Work the dispatcher thread drains in order.
enum PubSubDispatch {
    /// One event and the listeners it was addressed to when it was published.
    ///
    /// Recipients are resolved at publish time, not delivery time, so a listener
    /// receives exactly the events published after it subscribed — never one
    /// that was already in flight when it arrived.
    Deliver {
        topic: String,
        event: Event,
        recipients: Vec<Arc<Mutex<dyn EventListener>>>,
    },
    /// Acknowledges once everything queued before it has been delivered.
    Barrier(SyncSender<()>),
}

/// In-process pub/sub for control-plane events.
///
/// Subscribing is synchronous: a registration is visible to the next publish on
/// any thread the moment [`PubSub::subscribe`] returns, so an event caused after
/// subscribing is delivered. There is no service to open, no connection to
/// establish, and no window in which a subscription exists but cannot receive.
///
/// Publishing is a queue-and-return. Every event goes through one FIFO, so all
/// listeners observe one order — the order events were published in — rather
/// than an order that depends on which thread happened to publish. It also means
/// no listener ever runs on an engine thread: the engine publishes from inside
/// its own graph write lock, and running a callback there would put a listener
/// underneath a lock it knows nothing about.
///
/// Control plane only, and in-process by construction: every publisher and every
/// listener lives in the app process, and an out-of-process observer reads the
/// control plane's `/ws/events` rather than this. Cross-process data movement is
/// the iceoryx2 channel plane, which this does not touch.
pub struct PubSub {
    subscriptions: RwLock<Vec<PubSubTopicSubscription>>,
    dispatch_sender: SyncSender<PubSubDispatch>,
    dispatcher: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Default for PubSub {
    fn default() -> Self {
        Self::new()
    }
}

impl PubSub {
    pub fn new() -> Self {
        let (dispatch_sender, dispatch_receiver) = sync_channel(MAX_QUEUED_DELIVERIES);
        let dispatcher = std::thread::Builder::new()
            .name("pubsub-dispatch".to_string())
            .spawn(move || run_dispatch_loop(dispatch_receiver))
            .inspect_err(|e| tracing::error!("Failed to spawn the pubsub dispatcher: {}", e))
            .ok();

        Self {
            subscriptions: RwLock::new(Vec::new()),
            dispatch_sender,
            dispatcher: Mutex::new(dispatcher),
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

    /// Queue an event for every listener subscribed to `topic` or to
    /// [`topics::ALL`].
    ///
    /// Returns once the event is queued, not once it is delivered. Ordering is
    /// the guarantee: events are delivered in the order they were published
    /// here, and every listener observes that same order.
    pub fn publish(&self, topic: &str, event: &Event) {
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

        if found_dead_subscription {
            self.subscriptions
                .write()
                .retain(|subscription| subscription.listener_weak.strong_count() > 0);
        }

        let recipient_count = recipients.len();
        if !recipients.is_empty()
            && self
                .dispatch_sender
                .send(PubSubDispatch::Deliver {
                    topic: topic.to_string(),
                    event: event.clone(),
                    recipients,
                })
                .is_err()
        {
            tracing::error!(
                "Dropping [{}] on topic '{}': the pubsub dispatcher is gone",
                event.log_name(),
                topic
            );
            return;
        }

        tracing::debug!(
            "Queued [{}] for topic [{}] ({} listener(s))",
            event.log_name(),
            topic,
            recipient_count
        );
    }

    /// Block until everything queued before this call has been delivered.
    ///
    /// A barrier through the same FIFO, so it needs no timing assumption: it
    /// cannot be acknowledged before the deliveries ahead of it have run.
    pub fn flush(&self) {
        let (delivered, wait_for_delivery) = sync_channel(1);
        if self
            .dispatch_sender
            .send(PubSubDispatch::Barrier(delivered))
            .is_ok()
        {
            let _ = wait_for_delivery.recv();
        }
    }
}

impl Drop for PubSub {
    fn drop(&mut self) {
        // Replacing the sender closes the channel, which ends the loop; joining
        // keeps a test's dispatcher from outliving the bus it belonged to.
        let (closed_sender, _) = sync_channel(1);
        let _ = std::mem::replace(&mut self.dispatch_sender, closed_sender);
        if let Some(dispatcher) = self.dispatcher.lock().take() {
            let _ = dispatcher.join();
        }
    }
}

/// Deliver queued events in order until the bus is dropped.
fn run_dispatch_loop(dispatch_receiver: Receiver<PubSubDispatch>) {
    while let Ok(dispatch) = dispatch_receiver.recv() {
        match dispatch {
            PubSubDispatch::Deliver {
                topic,
                event,
                recipients,
            } => {
                for listener in &recipients {
                    if let Err(e) = listener.lock().on_event(&event) {
                        tracing::warn!(
                            "Listener on topic '{}' failed to handle [{}]: {}",
                            topic,
                            event.log_name(),
                            e
                        );
                    }
                }
            }
            PubSubDispatch::Barrier(delivered) => {
                let _ = delivered.send(());
            }
        }
    }
}
