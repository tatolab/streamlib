// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Integration test for pubsub module - runs independently of broken unit tests
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use streamlib::core::pubsub::{
    topics, Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState,
    ProcessorEvent, PubSub,
};

/// Wait for rayon thread pool to complete pending tasks
fn wait_for_rayon() {
    std::thread::sleep(std::time::Duration::from_millis(50));
}

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
fn test_keyboard_event_routing() {
    let bus = PubSub::new();
    let concrete_listener = Arc::new(Mutex::new(CountingListener::new()));
    let listener: Arc<Mutex<dyn EventListener>> = concrete_listener.clone();

    bus.subscribe(topics::KEYBOARD, listener);

    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);

    assert_eq!(event.topic(), topics::KEYBOARD);
    bus.publish(&event.topic(), &event);

    wait_for_rayon();
    assert_eq!(concrete_listener.lock().count(), 1);
}

#[test]
fn test_mouse_event_routing() {
    let bus = PubSub::new();
    let concrete_listener = Arc::new(Mutex::new(CountingListener::new()));
    let listener: Arc<Mutex<dyn EventListener>> = concrete_listener.clone();

    bus.subscribe(topics::MOUSE, listener);

    let event = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);

    assert_eq!(event.topic(), topics::MOUSE);
    bus.publish(&event.topic(), &event);

    wait_for_rayon();
    assert_eq!(concrete_listener.lock().count(), 1);
}

#[test]
fn test_processor_event_routing() {
    let bus = PubSub::new();
    let concrete_listener = Arc::new(Mutex::new(CountingListener::new()));
    let listener: Arc<Mutex<dyn EventListener>> = concrete_listener.clone();

    let processor_id = "audio-mixer";
    let topic = topics::processor(processor_id);

    bus.subscribe(&topic, listener);

    let event = Event::processor(processor_id, ProcessorEvent::Started);

    assert_eq!(event.topic(), topic);
    bus.publish(&event.topic(), &event);

    wait_for_rayon();
    assert_eq!(concrete_listener.lock().count(), 1);
}

#[test]
fn test_multiple_subscribers_all_receive() {
    let bus = PubSub::new();

    let concrete1 = Arc::new(Mutex::new(CountingListener::new()));
    let concrete2 = Arc::new(Mutex::new(CountingListener::new()));
    let concrete3 = Arc::new(Mutex::new(CountingListener::new()));

    let listener1: Arc<Mutex<dyn EventListener>> = concrete1.clone();
    let listener2: Arc<Mutex<dyn EventListener>> = concrete2.clone();
    let listener3: Arc<Mutex<dyn EventListener>> = concrete3.clone();

    bus.subscribe("broadcast", listener1);
    bus.subscribe("broadcast", listener2);
    bus.subscribe("broadcast", listener3);

    let event = Event::custom("broadcast", serde_json::json!({"value": 42}));
    bus.publish(&event.topic(), &event);

    wait_for_rayon();
    assert_eq!(concrete1.lock().count(), 1);
    assert_eq!(concrete2.lock().count(), 1);
    assert_eq!(concrete3.lock().count(), 1);
}

#[test]
fn test_topic_isolation() {
    let bus = PubSub::new();

    let concrete_audio = Arc::new(Mutex::new(CountingListener::new()));
    let concrete_video = Arc::new(Mutex::new(CountingListener::new()));

    let audio_listener: Arc<Mutex<dyn EventListener>> = concrete_audio.clone();
    let video_listener: Arc<Mutex<dyn EventListener>> = concrete_video.clone();

    bus.subscribe(&topics::processor("audio"), audio_listener);
    bus.subscribe(&topics::processor("video"), video_listener);

    let audio_event = Event::processor("audio", ProcessorEvent::Started);
    bus.publish(&audio_event.topic(), &audio_event);

    wait_for_rayon();
    assert_eq!(concrete_audio.lock().count(), 1);
    assert_eq!(concrete_video.lock().count(), 0);
}
