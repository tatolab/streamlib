// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tests for the in-process event bus.
//!
//! Every test here is deterministic and free of sleeps, retries and timeouts,
//! and that is the point rather than a nicety: `subscribe` registers a listener
//! synchronously, so an event published after it returns must be delivered by
//! the time `publish` returns. Any reintroduced asynchrony — a transport, a
//! dispatch thread, a buffer — makes these flake immediately, which is the
//! regression they exist to lock.
//!
//! The one bounded wait is the re-entrancy test, where a bound is the only way
//! to turn a deadlock into a named failure instead of a hung suite.
//!
//! Lives in-source (rather than `tests/`) to construct ad-hoc `PubSub`
//! instances, which the public surface does not expose. Each test owns its own
//! bus, so there is no shared state and no ordering between them.

use super::bus::PubSub;
use super::events::{
    Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState, ProcessorEvent,
    RuntimeEvent, topics,
};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Records every event it receives, in order.
#[derive(Default)]
struct RecordingListener {
    received: Arc<Mutex<Vec<Event>>>,
}

impl RecordingListener {
    fn with_shared_log() -> (Self, Arc<Mutex<Vec<Event>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                received: Arc::clone(&received),
            },
            received,
        )
    }
}

impl EventListener for RecordingListener {
    fn on_event(&mut self, event: &Event) -> crate::core::error::Result<()> {
        self.received.lock().push(event.clone());
        Ok(())
    }
}

/// Subscribe a recording listener, returning the strong `Arc` the caller must
/// keep alive and the log it appends to.
fn subscribe_recorder(
    bus: &PubSub,
    topic: &str,
) -> (Arc<Mutex<dyn EventListener>>, Arc<Mutex<Vec<Event>>>) {
    let (listener, received) = RecordingListener::with_shared_log();
    let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(listener));
    bus.subscribe(topic, Arc::clone(&listener));
    (listener, received)
}

fn keyboard_event() -> Event {
    Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed)
}

// ===========================================================================
// A. Delivery — synchronous, with nothing to wait for
// ===========================================================================

/// The contract the bus exists for, and the reason it has no live signal: an
/// event published after `subscribe` returns is already delivered when `publish`
/// returns. No retry, no budget, no poll.
#[test]
fn an_event_published_after_subscribe_is_delivered_before_publish_returns() {
    let bus = PubSub::new();
    let (_listener, received) = subscribe_recorder(&bus, topics::KEYBOARD);

    let event = keyboard_event();
    bus.publish(&event.topic(), &event);

    assert_eq!(received.lock().len(), 1, "delivery is synchronous");
    assert_eq!(received.lock()[0].topic(), topics::KEYBOARD);
}

#[test]
fn every_subscriber_on_a_topic_receives_one_copy() {
    let bus = PubSub::new();
    let (_first, first_received) = subscribe_recorder(&bus, topics::KEYBOARD);
    let (_second, second_received) = subscribe_recorder(&bus, topics::KEYBOARD);

    let event = keyboard_event();
    bus.publish(&event.topic(), &event);

    assert_eq!(first_received.lock().len(), 1);
    assert_eq!(second_received.lock().len(), 1);
}

#[test]
fn an_event_reaches_only_its_own_topic() {
    let bus = PubSub::new();
    let (_keyboard, keyboard_received) = subscribe_recorder(&bus, topics::KEYBOARD);
    let (_mouse, mouse_received) = subscribe_recorder(&bus, topics::MOUSE);

    let event = Event::mouse(MouseButton::Left, (10.0, 20.0), MouseState::Pressed);
    bus.publish(&event.topic(), &event);

    assert_eq!(mouse_received.lock().len(), 1);
    assert!(
        keyboard_received.lock().is_empty(),
        "a keyboard subscriber must not see a mouse event"
    );
}

/// A wildcard subscriber sees every topic, and sees each event exactly once —
/// not twice for having matched both the specific topic and the wildcard.
#[test]
fn a_wildcard_subscriber_receives_every_topic_exactly_once() {
    let bus = PubSub::new();
    let (_listener, received) = subscribe_recorder(&bus, topics::ALL);

    let events = [
        keyboard_event(),
        Event::mouse(MouseButton::Right, (5.0, 10.0), MouseState::Released),
        Event::processor("test-proc", ProcessorEvent::Started),
        Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    ];
    for event in &events {
        bus.publish(&event.topic(), event);
    }

    assert_eq!(received.lock().len(), events.len());
}

#[test]
fn a_delivered_event_is_identical_to_the_one_published() {
    let bus = PubSub::new();
    let (_listener, received) = subscribe_recorder(&bus, topics::KEYBOARD);

    let event = Event::keyboard(
        KeyCode::Z,
        Modifiers {
            shift: true,
            ctrl: false,
            alt: true,
            meta: false,
        },
        KeyState::Released,
    );
    bus.publish(&event.topic(), &event);

    assert_eq!(received.lock()[0], event);
}

#[test]
fn a_custom_event_carries_its_payload_intact() {
    let bus = PubSub::new();
    let (_listener, received) = subscribe_recorder(&bus, "my-custom-topic");

    let event = Event::custom("my-custom-topic", serde_json::json!({"key": "value"}));
    bus.publish(&event.topic(), &event);

    assert_eq!(received.lock()[0], event);
}

// ===========================================================================
// B. Lifecycle — no initialization step, no buffering
// ===========================================================================

/// There is no `init`: a bus delivers from its first instruction, so nothing is
/// ever buffered waiting for a backend that has not come up.
#[test]
fn a_fresh_bus_delivers_with_no_initialization_step() {
    let bus = PubSub::new();
    let (_listener, received) = subscribe_recorder(&bus, topics::RUNTIME_GLOBAL);

    let event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted);
    bus.publish(&event.topic(), &event);

    assert_eq!(received.lock().len(), 1);
}

#[test]
fn publishing_with_no_subscribers_is_a_no_op() {
    let bus = PubSub::new();
    let event = keyboard_event();
    bus.publish(&event.topic(), &event);
}

#[test]
fn dropping_the_listener_unsubscribes_it() {
    let bus = PubSub::new();
    let (listener, received) = subscribe_recorder(&bus, topics::KEYBOARD);

    let event = keyboard_event();
    bus.publish(&event.topic(), &event);
    assert_eq!(received.lock().len(), 1);

    drop(listener);
    bus.publish(&event.topic(), &event);

    assert_eq!(
        received.lock().len(),
        1,
        "a dropped listener receives nothing further"
    );
}

/// Dropping a listener leaves a dead registration behind; the next publish that
/// finds it removes it, so a long-lived bus does not accumulate them.
///
/// The count is asserted, not inferred from delivery: a dead entry is skipped
/// whether or not it is ever pruned, so delivery alone proves nothing about the
/// registry shrinking.
#[test]
fn a_dropped_listeners_registration_is_pruned_by_the_next_publish() {
    let bus = PubSub::new();
    let (listener, _received) = subscribe_recorder(&bus, topics::KEYBOARD);
    let (_survivor, survivor_received) = subscribe_recorder(&bus, topics::KEYBOARD);
    assert_eq!(bus.registration_count(), 2);

    drop(listener);
    let event = keyboard_event();
    bus.publish(&event.topic(), &event);

    assert_eq!(
        bus.registration_count(),
        1,
        "the dead registration is gone, not merely skipped"
    );
    assert_eq!(
        survivor_received.lock().len(),
        1,
        "pruning a dead entry must not disturb a live one"
    );
}

/// A subscriber on a topic nothing publishes to is still pruned — liveness is
/// checked on every registration, not only the ones a publish routes to.
#[test]
fn a_dropped_listener_is_pruned_even_on_a_topic_nothing_publishes_to() {
    let bus = PubSub::new();
    let (quiet_listener, _quiet) = subscribe_recorder(&bus, &topics::processor("never-published"));
    let (_active, _active_received) = subscribe_recorder(&bus, topics::KEYBOARD);
    assert_eq!(bus.registration_count(), 2);

    drop(quiet_listener);
    let event = keyboard_event();
    bus.publish(&event.topic(), &event);

    assert_eq!(
        bus.registration_count(),
        1,
        "a dead registration on an unpublished topic must not accumulate"
    );
}

#[test]
fn two_buses_are_isolated() {
    let first_bus = PubSub::new();
    let second_bus = PubSub::new();
    let (_first, first_received) = subscribe_recorder(&first_bus, topics::KEYBOARD);
    let (_second, second_received) = subscribe_recorder(&second_bus, topics::KEYBOARD);

    let event = keyboard_event();
    first_bus.publish(&event.topic(), &event);

    assert_eq!(first_received.lock().len(), 1);
    assert!(second_received.lock().is_empty());
}

// ===========================================================================
// C. Concurrency and re-entrancy
// ===========================================================================

/// Every publish from every thread is delivered — an exact count, not "at least
/// one". The old transport could only promise the latter.
#[test]
fn concurrent_publishes_are_all_delivered() {
    const PUBLISHER_THREADS: usize = 4;
    const PUBLISHES_PER_THREAD: usize = 25;

    let bus = Arc::new(PubSub::new());
    let delivered = Arc::new(AtomicUsize::new(0));

    struct CountingListener {
        delivered: Arc<AtomicUsize>,
    }
    impl EventListener for CountingListener {
        fn on_event(&mut self, _event: &Event) -> crate::core::error::Result<()> {
            self.delivered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(CountingListener {
        delivered: Arc::clone(&delivered),
    }));
    bus.subscribe(topics::ALL, Arc::clone(&listener));

    let publishers: Vec<_> = (0..PUBLISHER_THREADS)
        .map(|_| {
            let bus = Arc::clone(&bus);
            std::thread::spawn(move || {
                for _ in 0..PUBLISHES_PER_THREAD {
                    let event = keyboard_event();
                    bus.publish(&event.topic(), &event);
                }
            })
        })
        .collect();
    for publisher in publishers {
        publisher.join().expect("publisher thread panicked");
    }

    assert_eq!(
        delivered.load(Ordering::SeqCst),
        PUBLISHER_THREADS * PUBLISHES_PER_THREAD,
        "every publish must be delivered, not merely most of them"
    );
}

/// Dispatch runs on the publishing thread with the listener's lock held, so the
/// bus must not also hold its registry lock across the callback — a listener
/// that subscribes from `on_event` would deadlock against its own publisher.
///
/// Bounded and run off-thread because the failure mode is a hang, and a hang
/// reports nothing: the timeout turns it into a named failure.
#[test]
fn a_listener_that_subscribes_from_on_event_does_not_deadlock() {
    struct SubscribingListener {
        bus: Arc<PubSub>,
        added: Arc<Mutex<Vec<Arc<Mutex<dyn EventListener>>>>>,
    }
    impl EventListener for SubscribingListener {
        fn on_event(&mut self, _event: &Event) -> crate::core::error::Result<()> {
            let (listener, _log) = RecordingListener::with_shared_log();
            let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(listener));
            self.bus.subscribe(topics::MOUSE, Arc::clone(&listener));
            self.added.lock().push(listener);
            Ok(())
        }
    }

    let bus = Arc::new(PubSub::new());
    let added = Arc::new(Mutex::new(Vec::new()));
    let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(SubscribingListener {
        bus: Arc::clone(&bus),
        added: Arc::clone(&added),
    }));
    bus.subscribe(topics::KEYBOARD, Arc::clone(&listener));

    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let event = keyboard_event();
        bus.publish(&event.topic(), &event);
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("publish deadlocked against a listener that subscribed from on_event");
    assert_eq!(added.lock().len(), 1);
}

// ===========================================================================
// D. Event vocabulary
// ===========================================================================

#[test]
fn keyboard_events_route_to_the_keyboard_topic() {
    assert_eq!(keyboard_event().topic(), topics::KEYBOARD);
}

#[test]
fn mouse_events_route_to_the_mouse_topic() {
    let event = Event::mouse(MouseButton::Left, (100.0, 200.0), MouseState::Pressed);
    assert_eq!(event.topic(), topics::MOUSE);
}

#[test]
fn processor_events_route_to_their_processors_topic() {
    let processor_id = "audio-mixer";
    let event = Event::processor(processor_id, ProcessorEvent::Started);
    assert_eq!(event.topic(), topics::processor(processor_id));
}

#[test]
fn a_processor_event_reaches_a_subscriber_on_that_processors_topic() {
    let bus = PubSub::new();
    let processor_id = "audio-mixer";
    let (_listener, received) = subscribe_recorder(&bus, &topics::processor(processor_id));

    let event = Event::processor(processor_id, ProcessorEvent::Started);
    bus.publish(&event.topic(), &event);

    assert_eq!(received.lock()[0], event);
}
