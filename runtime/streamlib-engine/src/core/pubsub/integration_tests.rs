// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The event bus's delivery contract, each test on its own `PubSub` instance
//! so nothing crosses into the process-wide `PUBSUB`.
//!
//! Every wait is on a delivered event with a timeout, never on a duration: a
//! test that stops receiving fails rather than hangs, and one that receives
//! never sleeps. "Nothing else arrived" is proven by publishing a sentinel
//! after the events under test — a listener's queue is FIFO, so the sentinel
//! arriving means everything before it already has.

use super::bus::{EVENTS_QUEUED_PER_SUBSCRIPTION, PubSub};
use super::events::{
    Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState, RuntimeEvent,
    topics,
};
use parking_lot::Mutex;
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::Duration;

/// How long a test waits for an event it expects before calling it lost.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(5);

struct ForwardingListener {
    forward_to_test: mpsc::Sender<Event>,
}

impl EventListener for ForwardingListener {
    fn on_event(&mut self, event: &Event) -> crate::core::error::Result<()> {
        let _ = self.forward_to_test.send(event.clone());
        Ok(())
    }
}

fn forwarding_listener() -> (Arc<Mutex<dyn EventListener>>, mpsc::Receiver<Event>) {
    let (forward_to_test, received) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ForwardingListener { forward_to_test }));
    (listener, received)
}

fn numbered_event(topic: &str, number: usize) -> Event {
    Event::custom(topic, serde_json::json!({ "number": number }))
}

fn number_of(event: &Event) -> usize {
    match event {
        Event::Custom { data, .. } => data["number"]
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .unwrap_or_else(|| panic!("not a numbered event: {event:?}")),
        other => panic!("not a numbered event: {other:?}"),
    }
}

fn sentinel() -> Event {
    Event::RuntimeGlobal(RuntimeEvent::RuntimeStopped)
}

/// Every event the listener received before the sentinel, which must arrive.
fn received_before_the_sentinel(received: &mpsc::Receiver<Event>) -> Vec<Event> {
    let mut before = Vec::new();
    loop {
        let event = received
            .recv_timeout(DELIVERY_DEADLINE)
            .expect("the sentinel published last was never delivered");
        if event == sentinel() {
            return before;
        }
        before.push(event);
    }
}

#[test]
fn an_event_published_the_instant_subscribe_returns_is_delivered() {
    let bus = PubSub::new();
    let (listener, received) = forwarding_listener();
    bus.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener))
        .expect("subscribe");

    let event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStopping);
    bus.publish(&event.topic(), &event);

    assert_eq!(received.recv_timeout(DELIVERY_DEADLINE), Ok(event));
}

/// The bus needs no runtime, no transport and no initialization step.
///
/// Mental-revert: dropping publishes made before a runtime exists (the old
/// `init` gate) loses this event.
#[test]
fn a_bus_no_runtime_has_touched_delivers_what_is_published_to_it() {
    let bus = PubSub::new();
    let (listener, received) = forwarding_listener();
    bus.subscribe(topics::KEYBOARD, Arc::clone(&listener))
        .expect("subscribe");

    let event = Event::keyboard(KeyCode::Z, Modifiers::default(), KeyState::Released);
    bus.publish(topics::KEYBOARD, &event);

    assert_eq!(received.recv_timeout(DELIVERY_DEADLINE), Ok(event));
}

#[test]
fn a_listener_hears_nothing_published_to_another_topic() {
    let bus = PubSub::new();
    let (keyboard_listener, keyboard_received) = forwarding_listener();
    let (mouse_listener, mouse_received) = forwarding_listener();
    bus.subscribe(topics::KEYBOARD, Arc::clone(&keyboard_listener))
        .expect("subscribe");
    bus.subscribe(topics::MOUSE, Arc::clone(&mouse_listener))
        .expect("subscribe");

    let mouse_event = Event::mouse(MouseButton::Left, (10.0, 20.0), MouseState::Pressed);
    bus.publish(topics::MOUSE, &mouse_event);
    bus.publish(topics::KEYBOARD, &sentinel());

    assert_eq!(
        mouse_received.recv_timeout(DELIVERY_DEADLINE),
        Ok(mouse_event)
    );
    assert!(received_before_the_sentinel(&keyboard_received).is_empty());
}

/// Mental-revert: delivering once for the topic and again for the wildcard —
/// the old transport's two sends — hands this listener every event twice.
#[test]
fn a_wildcard_listener_hears_every_topic_exactly_once() {
    let bus = PubSub::new();
    let (listener, received) = forwarding_listener();
    bus.subscribe(topics::ALL, Arc::clone(&listener))
        .expect("subscribe");

    let published = vec![
        Event::keyboard(KeyCode::C, Modifiers::default(), KeyState::Pressed),
        Event::mouse(MouseButton::Right, (5.0, 10.0), MouseState::Released),
        Event::custom("a-topic-nobody-named", serde_json::json!({ "ok": true })),
    ];
    for event in &published {
        bus.publish(&event.topic(), event);
    }
    bus.publish(topics::RUNTIME_GLOBAL, &sentinel());

    assert_eq!(received_before_the_sentinel(&received), published);
}

/// The iceoryx2 transport refused its ninth subscriber.
#[test]
fn a_hundred_listeners_on_one_topic_each_receive_the_event() {
    let bus = PubSub::new();
    let listeners: Vec<_> = (0..100).map(|_| forwarding_listener()).collect();
    for (listener, _) in &listeners {
        bus.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(listener))
            .expect("subscribe");
    }

    let event = Event::RuntimeGlobal(RuntimeEvent::GraphDidChange);
    bus.publish(topics::RUNTIME_GLOBAL, &event);

    for (index, (_, received)) in listeners.iter().enumerate() {
        assert_eq!(
            received.recv_timeout(DELIVERY_DEADLINE),
            Ok(event.clone()),
            "listener {index} missed the event"
        );
    }
}

/// The iceoryx2 transport refused a seventeenth publishing thread, and could
/// lose a fresh thread's first events while its publisher connected.
#[test]
fn thirty_two_threads_publishing_at_once_lose_nothing() {
    const PUBLISHING_THREADS: usize = 32;
    let bus = Arc::new(PubSub::new());
    let (listener, received) = forwarding_listener();
    bus.subscribe("burst", Arc::clone(&listener))
        .expect("subscribe");

    let start_together = Arc::new(Barrier::new(PUBLISHING_THREADS));
    let publishers: Vec<_> = (0..PUBLISHING_THREADS)
        .map(|number| {
            let bus = Arc::clone(&bus);
            let start_together = Arc::clone(&start_together);
            std::thread::spawn(move || {
                start_together.wait();
                bus.publish("burst", &numbered_event("burst", number));
            })
        })
        .collect();
    for publisher in publishers {
        publisher.join().expect("publisher thread");
    }

    let mut numbers: Vec<usize> = (0..PUBLISHING_THREADS)
        .map(|_| {
            number_of(
                &received
                    .recv_timeout(DELIVERY_DEADLINE)
                    .expect("an event from one of the publishing threads was lost"),
            )
        })
        .collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (0..PUBLISHING_THREADS).collect::<Vec<_>>());
}

#[test]
fn a_listener_receives_events_in_the_order_they_were_published() {
    let bus = PubSub::new();
    let (listener, received) = forwarding_listener();
    bus.subscribe("ordered", Arc::clone(&listener))
        .expect("subscribe");

    for number in 0..500 {
        bus.publish("ordered", &numbered_event("ordered", number));
    }
    bus.publish("ordered", &sentinel());

    let numbers: Vec<usize> = received_before_the_sentinel(&received)
        .iter()
        .map(number_of)
        .collect();
    assert_eq!(numbers, (0..500).collect::<Vec<_>>());
}

/// Events from many threads interleave arbitrarily, but every listener sees
/// the one interleaving the bus admitted them in.
///
/// Mental-revert: enqueueing to each listener outside the one lock lets two
/// publishers land in opposite orders on two listeners.
#[test]
fn every_listener_sees_events_from_many_threads_in_one_and_the_same_order() {
    const PUBLISHING_THREADS: usize = 8;
    const EVENTS_PER_THREAD: usize = 100;
    let bus = Arc::new(PubSub::new());
    let (first_listener, first_received) = forwarding_listener();
    let (second_listener, second_received) = forwarding_listener();
    bus.subscribe("interleaved", Arc::clone(&first_listener))
        .expect("subscribe");
    bus.subscribe("interleaved", Arc::clone(&second_listener))
        .expect("subscribe");

    let start_together = Arc::new(Barrier::new(PUBLISHING_THREADS));
    let publishers: Vec<_> = (0..PUBLISHING_THREADS)
        .map(|thread| {
            let bus = Arc::clone(&bus);
            let start_together = Arc::clone(&start_together);
            std::thread::spawn(move || {
                start_together.wait();
                for sequence in 0..EVENTS_PER_THREAD {
                    let number = thread * EVENTS_PER_THREAD + sequence;
                    bus.publish("interleaved", &numbered_event("interleaved", number));
                }
            })
        })
        .collect();
    for publisher in publishers {
        publisher.join().expect("publisher thread");
    }
    bus.publish("interleaved", &sentinel());

    let first_order = received_before_the_sentinel(&first_received);
    let second_order = received_before_the_sentinel(&second_received);
    assert_eq!(first_order.len(), PUBLISHING_THREADS * EVENTS_PER_THREAD);
    assert_eq!(first_order, second_order);
}

/// The iceoryx2 transport refused a serialized event past 8 KiB.
#[test]
fn an_event_past_the_old_eight_kibibyte_ceiling_arrives_whole() {
    let bus = PubSub::new();
    let (listener, received) = forwarding_listener();
    bus.subscribe("large", Arc::clone(&listener))
        .expect("subscribe");

    let event = Event::custom(
        "large",
        serde_json::json!({ "payload": "x".repeat(64 * 1024) }),
    );
    bus.publish("large", &event);

    assert_eq!(received.recv_timeout(DELIVERY_DEADLINE), Ok(event));
}

struct ListenerHeldInsideItsFirstEvent {
    entered_first_event: mpsc::Sender<()>,
    release_first_event: Option<mpsc::Receiver<()>>,
    forward_to_test: mpsc::Sender<Event>,
}

impl EventListener for ListenerHeldInsideItsFirstEvent {
    fn on_event(&mut self, event: &Event) -> crate::core::error::Result<()> {
        if let Some(release) = self.release_first_event.take() {
            let _ = self.entered_first_event.send(());
            let _ = release.recv();
        }
        let _ = self.forward_to_test.send(event.clone());
        Ok(())
    }
}

/// A publish never waits on a listener: past a full queue the event is
/// dropped and counted, and what was queued still arrives in order.
///
/// Mental-revert: a blocking send parks the publishing thread on the held
/// listener and the publishing deadline below fails.
#[test]
fn a_full_listener_queue_drops_and_counts_rather_than_blocking_the_publisher() {
    const DROPPED_PAST_THE_FULL_QUEUE: usize = 3;
    let bus = Arc::new(PubSub::new());
    let (entered_first_event, first_event_entered) = mpsc::channel();
    let (release_first_event, first_event_release) = mpsc::channel();
    let (forward_to_test, received) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ListenerHeldInsideItsFirstEvent {
            entered_first_event,
            release_first_event: Some(first_event_release),
            forward_to_test,
        }));
    bus.subscribe("held", Arc::clone(&listener))
        .expect("subscribe");

    bus.publish("held", &numbered_event("held", 0));
    first_event_entered
        .recv_timeout(DELIVERY_DEADLINE)
        .expect("the listener never took its first event");

    let publishing_done = {
        let bus = Arc::clone(&bus);
        let (publishing_done, publishing_done_received) = mpsc::channel();
        std::thread::spawn(move || {
            for number in 1..=EVENTS_QUEUED_PER_SUBSCRIPTION + DROPPED_PAST_THE_FULL_QUEUE {
                bus.publish("held", &numbered_event("held", number));
            }
            let _ = publishing_done.send(());
        });
        publishing_done_received
    };
    publishing_done
        .recv_timeout(DELIVERY_DEADLINE)
        .expect("publishing blocked on a listener that was not taking events");
    assert_eq!(
        bus.events_dropped_at_full_subscription_queues(),
        DROPPED_PAST_THE_FULL_QUEUE as u64
    );

    release_first_event.send(()).expect("release the listener");
    let numbers: Vec<usize> = (0..=EVENTS_QUEUED_PER_SUBSCRIPTION)
        .map(|_| {
            number_of(
                &received
                    .recv_timeout(DELIVERY_DEADLINE)
                    .expect("a queued event was lost"),
            )
        })
        .collect();
    assert_eq!(
        numbers,
        (0..=EVENTS_QUEUED_PER_SUBSCRIPTION).collect::<Vec<_>>()
    );
}

struct RepublishingListener {
    bus: Arc<PubSub>,
}

impl EventListener for RepublishingListener {
    fn on_event(&mut self, event: &Event) -> crate::core::error::Result<()> {
        self.bus.publish(
            "downstream",
            &numbered_event("downstream", number_of(event)),
        );
        Ok(())
    }
}

/// Mental-revert: calling listeners inside `publish`, under the bus lock,
/// deadlocks the moment a listener publishes.
#[test]
fn a_listener_may_publish_from_inside_its_own_callback() {
    let bus = Arc::new(PubSub::new());
    let republishing: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(RepublishingListener {
        bus: Arc::clone(&bus),
    }));
    let (downstream_listener, downstream_received) = forwarding_listener();
    bus.subscribe("upstream", Arc::clone(&republishing))
        .expect("subscribe");
    bus.subscribe("downstream", Arc::clone(&downstream_listener))
        .expect("subscribe");

    bus.publish("upstream", &numbered_event("upstream", 7));

    let downstream_event = downstream_received
        .recv_timeout(DELIVERY_DEADLINE)
        .expect("the listener's own publish never arrived");
    assert_eq!(number_of(&downstream_event), 7);
}

/// A dropped listener's subscription goes at the next publish, which also
/// ends its delivery thread.
///
/// Mental-revert: pruning only once a send finds the delivery thread gone
/// still holds this subscription after the publish.
#[test]
fn a_dropped_listeners_subscription_goes_at_the_next_publish() {
    let bus = PubSub::new();
    let (kept_listener, kept_received) = forwarding_listener();
    let (dropped_listener, _) = forwarding_listener();
    bus.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&kept_listener))
        .expect("subscribe");
    bus.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&dropped_listener))
        .expect("subscribe");
    assert_eq!(bus.subscriptions_held(), 2);

    drop(dropped_listener);
    bus.publish(topics::RUNTIME_GLOBAL, &sentinel());

    assert_eq!(bus.subscriptions_held(), 1);
    assert!(received_before_the_sentinel(&kept_received).is_empty());
}

#[test]
fn a_dropped_listeners_subscription_goes_at_the_next_subscribe() {
    let bus = PubSub::new();
    let (dropped_listener, _) = forwarding_listener();
    bus.subscribe(topics::MOUSE, Arc::clone(&dropped_listener))
        .expect("subscribe");
    drop(dropped_listener);

    let (kept_listener, _) = forwarding_listener();
    bus.subscribe(topics::MOUSE, Arc::clone(&kept_listener))
        .expect("subscribe");

    assert_eq!(bus.subscriptions_held(), 1);
}

#[test]
fn two_buses_share_no_events() {
    let first_bus = PubSub::new();
    let second_bus = PubSub::new();
    let (first_listener, first_received) = forwarding_listener();
    let (second_listener, second_received) = forwarding_listener();
    first_bus
        .subscribe(topics::KEYBOARD, Arc::clone(&first_listener))
        .expect("subscribe");
    second_bus
        .subscribe(topics::KEYBOARD, Arc::clone(&second_listener))
        .expect("subscribe");

    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    first_bus.publish(topics::KEYBOARD, &event);
    second_bus.publish(topics::KEYBOARD, &sentinel());

    assert_eq!(first_received.recv_timeout(DELIVERY_DEADLINE), Ok(event));
    assert!(received_before_the_sentinel(&second_received).is_empty());
}
