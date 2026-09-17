// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, LazyLock, Weak};

use super::events::{Event, EventListener, topics};
use crate::core::error::{Error, Result};

/// Process-wide event bus.
pub static PUBSUB: LazyLock<PubSub> = LazyLock::new(PubSub::new);

/// Events one subscription holds undelivered; a publish past this is dropped and counted.
pub const EVENTS_QUEUED_PER_SUBSCRIPTION: usize = 1024;

/// In-process fan-out of runtime events to the listeners subscribed to their topic.
pub struct PubSub {
    subscriptions: Mutex<Vec<EventSubscription>>,
    events_dropped_at_full_subscription_queues: AtomicU64,
}

struct EventSubscription {
    topic: String,
    listener: Weak<Mutex<dyn EventListener>>,
    queued_events_for_listener: SyncSender<Arc<Event>>,
    events_dropped_at_this_full_queue: u64,
}

impl EventSubscription {
    fn listener_is_alive(&self) -> bool {
        self.listener.strong_count() > 0
    }

    fn hears(&self, published_topic: &str) -> bool {
        self.topic == published_topic || self.topic == topics::ALL
    }
}

impl Default for PubSub {
    fn default() -> Self {
        Self::new()
    }
}

impl PubSub {
    pub fn new() -> Self {
        Self {
            subscriptions: Mutex::new(Vec::new()),
            events_dropped_at_full_subscription_queues: AtomicU64::new(0),
        }
    }

    /// Subscribe a listener to a topic; every event published after this returns reaches it.
    ///
    /// The bus holds only a `Weak` reference to the listener, so the caller
    /// keeps the `Arc` alive for as long as it wants events — a temporary
    /// `Arc::new(..)` passed straight in hears nothing. Once the last strong
    /// reference is gone the subscription is removed at the next publish or
    /// subscribe, and its delivery thread ends.
    pub fn subscribe(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) -> Result<()> {
        // strong_count == 1 means this parameter is the only reference and
        // will be dropped when this call returns.
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

        let (queued_events_for_listener, queued_events_from_bus) =
            sync_channel(EVENTS_QUEUED_PER_SUBSCRIPTION);
        let listener = Arc::downgrade(&listener);
        let listener_for_delivery = Weak::clone(&listener);
        let topic_for_delivery = topic.to_string();
        std::thread::Builder::new()
            .name(format!("pubsub-{topic}"))
            .spawn(move || {
                deliver_queued_events_to_listener(
                    queued_events_from_bus,
                    &listener_for_delivery,
                    &topic_for_delivery,
                )
            })
            .map_err(|spawn_failure| {
                Error::Runtime(format!(
                    "failed to spawn the delivery thread for a subscription to '{topic}': \
                     {spawn_failure}"
                ))
            })?;

        let mut subscriptions = self.subscriptions.lock();
        subscriptions.retain(EventSubscription::listener_is_alive);
        subscriptions.push(EventSubscription {
            topic: topic.to_string(),
            listener,
            queued_events_for_listener,
            events_dropped_at_this_full_queue: 0,
        });
        Ok(())
    }

    /// Publish an event to every listener of `topic` and every listener of [`topics::ALL`].
    ///
    /// Never blocks: a listener whose queue is full loses this event, and the
    /// loss is counted and logged.
    pub fn publish(&self, topic: &str, event: &Event) {
        let mut shared_event: Option<Arc<Event>> = None;
        let mut full_queues: Vec<(String, u64)> = Vec::new();
        {
            let mut subscriptions = self.subscriptions.lock();
            subscriptions.retain_mut(|subscription| {
                if !subscription.listener_is_alive() {
                    return false;
                }
                if !subscription.hears(topic) {
                    return true;
                }
                let event_for_subscription =
                    Arc::clone(shared_event.get_or_insert_with(|| Arc::new(event.clone())));
                match subscription
                    .queued_events_for_listener
                    .try_send(event_for_subscription)
                {
                    Ok(()) => true,
                    Err(TrySendError::Disconnected(_)) => false,
                    Err(TrySendError::Full(_)) => {
                        subscription.events_dropped_at_this_full_queue += 1;
                        self.events_dropped_at_full_subscription_queues
                            .fetch_add(1, Ordering::Relaxed);
                        full_queues.push((
                            subscription.topic.clone(),
                            subscription.events_dropped_at_this_full_queue,
                        ));
                        true
                    }
                }
            });
        }

        for (subscribed_topic, dropped_so_far) in full_queues {
            if dropped_so_far.is_power_of_two() {
                tracing::warn!(
                    "a listener subscribed to '{subscribed_topic}' has {EVENTS_QUEUED_PER_SUBSCRIPTION} \
                     events undelivered, so {} was dropped; {dropped_so_far} dropped for it so far",
                    event.log_name(),
                );
            }
        }

        tracing::debug!("Published [{}] to topic [{}]", event.log_name(), topic);
    }

    /// Every event this bus has dropped because a listener's queue was full.
    pub fn events_dropped_at_full_subscription_queues(&self) -> u64 {
        self.events_dropped_at_full_subscription_queues
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn subscriptions_held(&self) -> usize {
        self.subscriptions.lock().len()
    }
}

/// Hand each queued event to the listener, in order, until the bus drops the
/// subscription or the listener is gone.
fn deliver_queued_events_to_listener(
    queued_events_from_bus: Receiver<Arc<Event>>,
    listener: &Weak<Mutex<dyn EventListener>>,
    topic: &str,
) {
    for event in queued_events_from_bus {
        let Some(listener) = listener.upgrade() else {
            return;
        };
        if let Err(failure) = listener.lock().on_event(&event) {
            tracing::warn!(
                "a listener subscribed to '{topic}' failed on {}: {failure}",
                event.log_name()
            );
        }
    }
}
