// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Integration test for pubsub module — verifies PubSub API behavior
// without requiring iceoryx2 shared memory infrastructure.
//
// Full message delivery tests require a running StreamRuntime (which
// initializes the iceoryx2 node) and are covered by example programs.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use streamlib::core::pubsub::{
    topics, Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState,
    ProcessorEvent, PubSub,
};

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
    fn on_event(&mut self, _event: &Event) -> streamlib::core::error::Result<()> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn test_pre_init_publish_is_noop() {
    // Before init(), publish should silently drop events (no crash)
    let bus = PubSub::new();
    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    bus.publish(&event.topic(), &event);
    // No assertion needed — we just verify it doesn't panic
}

#[test]
fn test_pre_init_subscribe_buffers() {
    // Before init(), subscribe should buffer (not crash)
    let bus = PubSub::new();
    let concrete = Arc::new(Mutex::new(CountingListener::new()));
    let listener: Arc<Mutex<dyn EventListener>> = concrete.clone();
    bus.subscribe(topics::KEYBOARD, listener);
    // Subscription is buffered, no events delivered yet
    assert_eq!(concrete.lock().count(), 0);
}

#[test]
fn test_keyboard_event_topic_routing() {
    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    assert_eq!(event.topic(), topics::KEYBOARD);
}

#[test]
fn test_mouse_event_topic_routing() {
    let event = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);
    assert_eq!(event.topic(), topics::MOUSE);
}

#[test]
fn test_processor_event_topic_routing() {
    let processor_id = "audio-mixer";
    let topic = topics::processor(processor_id);
    let event = Event::processor(processor_id, ProcessorEvent::Started);
    assert_eq!(event.topic(), topic);
}

#[test]
fn test_event_msgpack_serialization() {
    // Verify events survive MessagePack round-trip (used by iceoryx2 transport)
    let events = vec![
        Event::RuntimeGlobal(streamlib::core::pubsub::RuntimeEvent::RuntimeStarted),
        Event::RuntimeGlobal(streamlib::core::pubsub::RuntimeEvent::GraphDidChange),
        Event::processor("test-proc", ProcessorEvent::Started),
        Event::custom("my-topic", serde_json::json!({"key": "value"})),
        Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed),
        Event::mouse(MouseButton::Right, (50.0, 75.0), MouseState::Released),
    ];

    for event in events {
        let bytes = rmp_serde::to_vec_named(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(event.topic(), deserialized.topic());
        assert_eq!(event.log_name(), deserialized.log_name());
    }
}
