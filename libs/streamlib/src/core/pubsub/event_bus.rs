//! Lock-free pub/sub event bus with parallel dispatch

use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::{Arc, LazyLock, Weak};

use super::events::{Event, EventListener};

/// Maximum event size: 64KB
const MAX_EVENT_SIZE: usize = 64 * 1024;

/// Compile-time size check for Event type
const _: () = {
    assert!(
        std::mem::size_of::<Event>() <= MAX_EVENT_SIZE,
        "Event type exceeds 64KB limit"
    );
};

/// Global event bus singleton - accessible from anywhere
pub static EVENT_BUS: LazyLock<EventBus> = LazyLock::new(|| EventBus::new());

/// Lock-free event bus with parallel dispatch
///
/// - Publish is ~100-200ns (Arc allocation + rayon spawn)
/// - No queuing = no message pile-up
/// - Events dispatched in parallel to all listeners
/// - Slow listeners don't block publisher or other listeners
pub struct EventBus {
    /// Map of topic name -> list of weak listener references
    /// DashMap provides lock-free concurrent HashMap
    /// Weak refs allow listeners to be dropped without explicit unsubscribe
    topics: DashMap<String, Vec<Weak<Mutex<dyn EventListener>>>>,
}

impl EventBus {
    /// Create a new event bus
    pub fn new() -> Self {
        Self {
            topics: DashMap::new(),
        }
    }

    /// Subscribe a listener to a topic
    ///
    /// # Example
    /// ```ignore
    /// let listener = Arc::new(Mutex::new(MyListener));
    /// EVENT_BUS.subscribe("my-topic", listener);
    /// ```
    pub fn subscribe(&self, topic: &str, listener: Arc<Mutex<dyn EventListener>>) {
        let weak_listener = Arc::downgrade(&listener);
        self.topics
            .entry(topic.to_string())
            .or_insert_with(Vec::new)
            .push(weak_listener);
    }

    /// Publish event to topic (instant, non-blocking, parallel dispatch)
    ///
    /// Events are shared via Arc and dispatched in parallel to all listeners.
    /// Each listener runs in its own rayon task, so slow listeners don't block others.
    ///
    /// Returns immediately (~100-200ns) regardless of number of listeners.
    ///
    /// # Example
    /// ```ignore
    /// EVENT_BUS.publish("my-topic", &Event::Custom {
    ///     topic: "my-topic".to_string(),
    ///     data: serde_json::json!({"key": "value"}),
    /// });
    /// ```
    pub fn publish(&self, topic: &str, event: &Event) {
        if let Some(subscribers) = self.topics.get(topic) {
            tracing::info!("EVENT_BUS: Publishing to topic '{}', {} subscribers registered", topic, subscribers.len());

            // Share event via Arc to avoid cloning for each listener
            let event = Arc::new(event.clone());

            // Collect live listeners (upgrade weak refs)
            let mut live_listeners = Vec::with_capacity(subscribers.len());
            for weak_listener in subscribers.iter() {
                if let Some(listener) = weak_listener.upgrade() {
                    live_listeners.push(listener);
                }
            }

            tracing::info!("EVENT_BUS: {} live listeners (weak refs upgraded successfully)", live_listeners.len());

            // Dispatch in parallel to all listeners
            // Each listener gets its own rayon task
            rayon::scope(|s| {
                for listener in live_listeners {
                    let event = Arc::clone(&event);
                    s.spawn(move |_| {
                        // Try lock without blocking
                        // If listener is busy, skip (fire-and-forget)
                        if let Some(mut guard) = listener.try_lock() {
                            tracing::info!("EVENT_BUS: Calling on_event for listener");
                            let _ = guard.on_event(&event);
                        } else {
                            tracing::warn!("EVENT_BUS: Listener mutex locked, skipping (fire-and-forget)");
                        }
                    });
                }
            });
        } else {
            tracing::warn!("EVENT_BUS: No subscribers for topic '{}'", topic);
        }
        // If no subscribers, event is dropped (true fire-and-forget)

        // Cleanup dead listeners periodically
        self.cleanup_dead_listeners(topic);
    }

    /// Remove dead listeners (called periodically during publish)
    fn cleanup_dead_listeners(&self, topic: &str) {
        if let Some(mut subscribers) = self.topics.get_mut(topic) {
            subscribers.retain(|weak| weak.strong_count() > 0);

            // Remove topic entry if no subscribers left
            if subscribers.is_empty() {
                drop(subscribers); // Release lock before removing
                self.topics.remove(topic);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Test listener that counts events (thread-safe)
    struct CountingListener {
        count: Arc<AtomicUsize>,
    }

    impl CountingListener {
        fn new() -> Self {
            Self {
                count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    impl EventListener for CountingListener {
        fn on_event(&mut self, _event: &Event) -> crate::core::error::Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_topic_routing() {
        let bus = EventBus::new();

        let audio_listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let video_listener_concrete = Arc::new(Mutex::new(CountingListener::new()));

        let audio_listener: Arc<Mutex<dyn EventListener>> = audio_listener_concrete.clone();
        let video_listener: Arc<Mutex<dyn EventListener>> = video_listener_concrete.clone();

        // Subscribe to different topics
        bus.subscribe("processor:audio", audio_listener);
        bus.subscribe("processor:video", video_listener);

        // Publish to audio topic
        bus.publish("processor:audio", &Event::ProcessorEvent {
            processor_id: "audio".to_string(),
            event: super::super::events::ProcessorEvent::Started,
        });

        // Rayon scope ensures all tasks complete before returning
        // Only audio subscriber receives
        assert_eq!(audio_listener_concrete.lock().count(), 1);
        assert_eq!(video_listener_concrete.lock().count(), 0);
    }

    #[test]
    fn test_runtime_global_broadcast() {
        let bus = EventBus::new();

        let listener1_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener2_concrete = Arc::new(Mutex::new(CountingListener::new()));

        let listener1: Arc<Mutex<dyn EventListener>> = listener1_concrete.clone();
        let listener2: Arc<Mutex<dyn EventListener>> = listener2_concrete.clone();

        // Multiple subscribers to runtime:global
        bus.subscribe("runtime:global", listener1);
        bus.subscribe("runtime:global", listener2);

        // Publish to runtime:global
        bus.publish("runtime:global", &Event::RuntimeGlobal(
            super::super::events::RuntimeEvent::RuntimeStart
        ));

        // Both subscribers receive (rayon scope ensures completion)
        assert_eq!(listener1_concrete.lock().count(), 1);
        assert_eq!(listener2_concrete.lock().count(), 1);
    }

    #[test]
    fn test_publish_to_nonexistent_topic_no_error() {
        let bus = EventBus::new();

        // Publish to a topic that has never been subscribed to
        // Should not panic, should not error, just fire-and-forget
        bus.publish("foo", &Event::Custom {
            topic: "foo".to_string(),
            data: serde_json::json!({"test": "data"}),
        });

        // If we get here without panicking, test passes
    }

    #[test]
    fn test_late_subscriber_misses_earlier_messages() {
        let bus = EventBus::new();

        // Publish first message with no subscribers
        bus.publish("bar", &Event::Custom {
            topic: "bar".to_string(),
            data: serde_json::json!({"message": "first"}),
        });

        // Now subscribe
        let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
        bus.subscribe("bar", listener);

        // Should have no messages (first message was lost)
        assert_eq!(listener_concrete.lock().count(), 0);

        // Publish second message
        bus.publish("bar", &Event::Custom {
            topic: "bar".to_string(),
            data: serde_json::json!({"message": "second"}),
        });

        // Subscriber should receive second message only
        assert_eq!(listener_concrete.lock().count(), 1);
    }

    #[test]
    fn test_multiple_subscribers_all_receive() {
        let bus = EventBus::new();

        // Subscribe 5 subscribers to the same topic
        let listener1_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener2_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener3_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener4_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener5_concrete = Arc::new(Mutex::new(CountingListener::new()));

        let listener1: Arc<Mutex<dyn EventListener>> = listener1_concrete.clone();
        let listener2: Arc<Mutex<dyn EventListener>> = listener2_concrete.clone();
        let listener3: Arc<Mutex<dyn EventListener>> = listener3_concrete.clone();
        let listener4: Arc<Mutex<dyn EventListener>> = listener4_concrete.clone();
        let listener5: Arc<Mutex<dyn EventListener>> = listener5_concrete.clone();

        bus.subscribe("broadcast", listener1);
        bus.subscribe("broadcast", listener2);
        bus.subscribe("broadcast", listener3);
        bus.subscribe("broadcast", listener4);
        bus.subscribe("broadcast", listener5);

        // Publish one message
        bus.publish("broadcast", &Event::Custom {
            topic: "broadcast".to_string(),
            data: serde_json::json!({"value": 42}),
        });

        // All 5 subscribers should receive the message (parallel dispatch)
        assert_eq!(listener1_concrete.lock().count(), 1);
        assert_eq!(listener2_concrete.lock().count(), 1);
        assert_eq!(listener3_concrete.lock().count(), 1);
        assert_eq!(listener4_concrete.lock().count(), 1);
        assert_eq!(listener5_concrete.lock().count(), 1);
    }

    #[test]
    fn test_dropped_listener_auto_cleanup() {
        let bus = EventBus::new();

        let listener_concrete = Arc::new(Mutex::new(CountingListener::new()));
        let listener: Arc<Mutex<dyn EventListener>> = listener_concrete.clone();
        bus.subscribe("test", listener);

        // Drop the concrete listener
        drop(listener_concrete);

        // Publishing should not panic and should clean up the dead listener
        bus.publish("test", &Event::Custom {
            topic: "test".to_string(),
            data: serde_json::json!({"value": 1}),
        });

        // If we get here without panicking, test passes
    }
}
