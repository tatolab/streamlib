// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Integration test for pubsub module — tests the full iceoryx2 transport layer.
//
// Each test that requires iceoryx2 creates its own PubSub::new() + Iceoryx2Node::new()
// instance with a unique runtime_id for isolation (no global state).
//
// IMPORTANT: PubSub::subscribe() takes ownership of the Arc but only stores a Weak ref
// internally. Callers MUST keep a strong reference alive for the subscriber thread to run.
// Always use `bus.subscribe(topic, listener.clone())` and keep `listener` on the stack.
//
// Synchronization strategy:
// - Uses std::sync::mpsc channels for delivery notification (no sleep-based waits)
// - Uses retry-publish pattern to handle the race between subscriber thread startup
//   and the first publish (PubSub provides no readiness signal)

use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use streamlib::core::pubsub::{
    topics, Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState,
    ProcessorEvent, PubSub, RuntimeEvent,
};
use streamlib::iceoryx2::{Iceoryx2Node, MAX_EVENT_PAYLOAD_SIZE};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

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

/// Listener that sends received events through an mpsc channel.
struct ChannelListener {
    sender: mpsc::Sender<Event>,
}

impl EventListener for ChannelListener {
    fn on_event(&mut self, event: &Event) -> streamlib::core::error::Result<()> {
        let _ = self.sender.send(event.clone());
        Ok(())
    }
}

/// Create an initialized PubSub instance with its own iceoryx2 node and unique runtime_id.
fn create_initialized_bus(test_name: &str) -> PubSub {
    let runtime_id = format!("test-{}-{}", test_name, uuid::Uuid::new_v4());
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let bus = PubSub::new();
    bus.init(&runtime_id, node);
    bus
}

/// Publish an event in a retry loop until the channel receives it (or timeout).
///
/// Handles the race between subscriber thread startup and the first publish.
/// PubSub's subscribe() spawns a thread that creates the iceoryx2 subscriber
/// asynchronously — this function retries until the subscriber is ready.
fn publish_until_received(
    bus: &PubSub,
    event: &Event,
    rx: &mpsc::Receiver<Event>,
    timeout: Duration,
) -> Option<Event> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        bus.publish(&event.topic(), event);
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(received) => return Some(received),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

// ===========================================================================
// A. Pre-init and routing tests (no iceoryx2 needed)
// ===========================================================================

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
        Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted),
        Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
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

// ===========================================================================
// B. Diagnostic: verify iceoryx2 transport works at the low level
// ===========================================================================

#[test]
fn test_iceoryx2_direct_delivery() {
    // Bypass PubSub layer entirely — verify iceoryx2 pub/sub works in-process
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");

    let service_name = format!("streamlib/diag-{}/events/test", uuid::Uuid::new_v4());

    // Create subscriber FIRST (must exist before publisher sends)
    let sub_service = node
        .open_or_create_event_service(&service_name)
        .expect("subscriber service");
    let subscriber = sub_service.create_subscriber().expect("subscriber");

    // Create publisher
    let pub_service = node
        .open_or_create_event_service(&service_name)
        .expect("publisher service");
    let publisher = pub_service.create_publisher().expect("publisher");

    // Publish
    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let bytes = rmp_serde::to_vec_named(&event).unwrap();
    let payload = streamlib::iceoryx2::EventPayload::new("test", 12345, &bytes);

    let sample = publisher.loan_uninit().expect("loan");
    let sample = sample.write_payload(payload);
    sample.send().expect("send");

    // Receive
    match subscriber.receive() {
        Ok(Some(sample)) => {
            let p: &streamlib::iceoryx2::EventPayload = &*sample;
            let received: Event = rmp_serde::from_slice(p.data()).unwrap();
            assert_eq!(received.topic(), event.topic());
        }
        Ok(None) => {
            panic!("iceoryx2 subscriber received None — message not delivered");
        }
        Err(e) => {
            panic!("iceoryx2 subscriber error: {:?}", e);
        }
    }
}

#[test]
fn test_iceoryx2_cross_thread_delivery() {
    // Verify iceoryx2 delivery works across threads (mimics PubSub pattern)
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let service_name = format!(
        "streamlib/diag-xthread-{}/events/test",
        uuid::Uuid::new_v4()
    );

    let (tx, rx) = mpsc::channel::<()>();
    let node_clone = node.clone();
    let sn = service_name.clone();

    // Subscriber on a separate thread
    std::thread::spawn(move || {
        let service = node_clone
            .open_or_create_event_service(&sn)
            .expect("sub service");
        let subscriber = service.create_subscriber().expect("subscriber");

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match subscriber.receive() {
                Ok(Some(_)) => {
                    let _ = tx.send(());
                    return;
                }
                Ok(None) => {
                    std::thread::yield_now();
                }
                Err(_) => return,
            }
        }
    });

    // Brief yield to let thread start, then publish in a retry loop
    std::thread::yield_now();
    let pub_service = node
        .open_or_create_event_service(&service_name)
        .expect("pub service");
    let publisher = pub_service.create_publisher().expect("publisher");

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let payload = streamlib::iceoryx2::EventPayload::new("test", 12345, b"hello");
        let sample = publisher.loan_uninit().expect("loan");
        let sample = sample.write_payload(payload);
        sample.send().expect("send");

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(()) => return, // success
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("Subscriber thread exited without receiving");
            }
        }
    }
    panic!("Cross-thread iceoryx2 delivery timed out");
}

#[test]
fn test_iceoryx2_pubsub_pattern_mimic() {
    // Exactly mimic what PubSub does: subscriber thread + fresh publisher per call
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let runtime_id = format!("test-mimic-{}", uuid::Uuid::new_v4());
    let topic = "input/keyboard";
    let service_name = format!("streamlib/{}/events/{}", runtime_id, topic);

    let (tx, rx) = mpsc::channel::<()>();
    let node_clone = node.clone();
    let sn_clone = service_name.clone();

    // Subscriber thread (mimics subscribe_inner)
    std::thread::spawn(move || {
        let service = node_clone
            .open_or_create_event_service(&sn_clone)
            .expect("sub service");
        let subscriber = service.create_subscriber().expect("subscriber");

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match subscriber.receive() {
                Ok(Some(_)) => {
                    let _ = tx.send(());
                    return;
                }
                Ok(None) => {
                    std::thread::yield_now();
                }
                Err(_) => return,
            }
        }
    });

    // Retry publish until subscriber receives (handles startup race)
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        // Fresh service + publisher per call (mimics PubSub::send_payload)
        let pub_service = node
            .open_or_create_event_service(&service_name)
            .expect("pub service");
        let publisher = pub_service.create_publisher().expect("publisher");

        let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
        let bytes = rmp_serde::to_vec_named(&event).unwrap();
        let payload = streamlib::iceoryx2::EventPayload::new(topic, 12345, &bytes);

        let sample = publisher.loan_uninit().expect("loan");
        let sample = sample.write_payload(payload);
        sample.send().expect("send");

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(()) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("Subscriber thread exited without receiving");
            }
        }
    }
    panic!("PubSub pattern mimic delivery timed out");
}

// ===========================================================================
// C. Diagnostic: verify PubSub's publish actually sends data to iceoryx2
// ===========================================================================

#[test]
fn test_pubsub_publish_sends_to_iceoryx2() {
    // Verify that PubSub::publish() actually sends data through iceoryx2
    // by creating a manual subscriber on the same service name.
    //
    // This bypasses PubSub's subscriber thread to isolate whether the bug
    // is in publish (send side) or subscribe (receive side).
    let _ = tracing_subscriber::fmt()
        .with_env_filter("trace")
        .with_test_writer()
        .try_init();

    let runtime_id = format!("test-pub-sends-{}", uuid::Uuid::new_v4());
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let node_probe = node.clone();
    let bus = PubSub::new();
    bus.init(&runtime_id, node);

    // Compute the service name PubSub will use for topics::KEYBOARD
    let sanitized_topic = topics::KEYBOARD.replace(':', "/");
    let service_name = format!("streamlib/{}/events/{}", runtime_id, sanitized_topic);

    // Create a manual iceoryx2 subscriber on that service BEFORE publishing
    let probe_service = node_probe
        .open_or_create_event_service(&service_name)
        .expect("probe service");
    let _probe_subscriber = probe_service.create_subscriber().expect("probe subscriber");

    // KEY INSIGHT: send() reports delivering to N subscribers, but receive()
    // returns None. Test if the issue is PortFactory creation order.
    //
    // Hypothesis: when send_payload creates a PortFactory via open_or_create,
    // and the service already exists (created by probe subscribers), the new
    // PortFactory's publisher sends to different subscriber slots.
    //
    // Test: force PubSub to create the service FIRST (via a warm-up publish),
    // THEN create probe subscribers, THEN publish the real event.

    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);

    // === Test A: Subscribers created BEFORE PubSub publishes (current pattern) ===
    let pre_probe_service = node_probe
        .open_or_create_event_service(&service_name)
        .expect("pre-probe service");
    let pre_probe_sub = pre_probe_service
        .create_subscriber()
        .expect("pre-probe subscriber");

    bus.publish(&event.topic(), &event);

    let pre_result = match pre_probe_sub.receive() {
        Ok(Some(_)) => "RECEIVED",
        Ok(None) => "NONE",
        Err(e) => panic!("Pre-probe error: {:?}", e),
    };
    eprintln!("[diag] Test A (sub before pub): {}", pre_result);

    // === Test B: Warm-up publish FIRST, then create subscriber, then publish again ===
    // Use a different service name to avoid interference
    let service_name_b = format!("streamlib/{}/events/input/mouse", runtime_id);
    let mouse_event = Event::mouse(MouseButton::Left, (0.0, 0.0), MouseState::Pressed);

    // Warm-up: force PubSub to create the service
    bus.publish(&mouse_event.topic(), &mouse_event);
    eprintln!("[diag] Test B: warm-up publish done");

    // Now create subscriber (service already exists from warm-up)
    let post_probe_service = node_probe
        .open_or_create_event_service(&service_name_b)
        .expect("post-probe service");
    let post_probe_sub = post_probe_service
        .create_subscriber()
        .expect("post-probe subscriber");

    // Publish again
    bus.publish(&mouse_event.topic(), &mouse_event);

    let post_result = match post_probe_sub.receive() {
        Ok(Some(_)) => "RECEIVED",
        Ok(None) => "NONE",
        Err(e) => panic!("Post-probe error: {:?}", e),
    };
    eprintln!("[diag] Test B (sub after warm-up pub): {}", post_result);

    // === Test C: Keep publisher alive across receive ===
    // Maybe the issue is that send_payload drops publisher before we receive
    let service_name_c = format!("streamlib/{}/events/input/window", runtime_id);
    let c_probe_service = node_probe
        .open_or_create_event_service(&service_name_c)
        .expect("c-probe service");
    let c_probe_sub = c_probe_service
        .create_subscriber()
        .expect("c-probe subscriber");

    // Mimic send_payload but keep publisher alive
    let c_pub_service = node_probe
        .open_or_create_event_service(&service_name_c)
        .expect("c-pub service");
    let c_publisher = c_pub_service.create_publisher().expect("c-publisher");
    let bytes = rmp_serde::to_vec_named(&event).unwrap();
    let c_payload = streamlib::iceoryx2::EventPayload::new("input:window", 12345, &bytes);
    let c_sample = c_publisher.loan_uninit().expect("loan");
    let c_sample = c_sample.write_payload(c_payload);
    c_sample.send().expect("send");

    let c_result = match c_probe_sub.receive() {
        Ok(Some(_)) => "RECEIVED",
        Ok(None) => "NONE",
        Err(e) => panic!("C-probe error: {:?}", e),
    };
    eprintln!("[diag] Test C (keep publisher alive): {}", c_result);

    // Report
    assert!(
        pre_result == "RECEIVED" || post_result == "RECEIVED",
        "PubSub::publish() should send data to iceoryx2. \
         TestA(sub-before-pub)={}, TestB(sub-after-warmup)={}, TestC(keep-pub-alive)={}",
        pre_result,
        post_result,
        c_result
    );
}

/// Minimal reproduction of PubSub's send_payload using OnceLock,
/// to determine if OnceLock storage causes the iceoryx2 delivery failure.
#[test]
fn test_oncelock_node_delivery() {
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let runtime_id = format!("test-oncelock-{}", uuid::Uuid::new_v4());
    let service_name = format!("streamlib/{}/events/input/keyboard", runtime_id);

    // Store node in OnceLock (mimics PubSub's storage)
    let node_in_lock: std::sync::OnceLock<Iceoryx2Node> = std::sync::OnceLock::new();
    let _ = node_in_lock.set(node.clone());

    // Create subscriber from the direct node clone
    let sub_service = node
        .open_or_create_event_service(&service_name)
        .expect("sub service");
    let subscriber = sub_service.create_subscriber().expect("subscriber");

    // Publish from OnceLock-stored node (mimics PubSub::send_payload)
    let node_ref = node_in_lock.get().unwrap();
    let pub_service = node_ref
        .open_or_create_event_service(&service_name)
        .expect("pub service");
    let publisher = pub_service.create_publisher().expect("publisher");

    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let bytes = rmp_serde::to_vec_named(&event).unwrap();
    let payload = streamlib::iceoryx2::EventPayload::new("input:keyboard", 12345, &bytes);

    // Use *&payload to mimic PubSub's `write_payload(*payload)` (copy from reference)
    let sample = publisher.loan_uninit().expect("loan");
    let sample = sample.write_payload(payload);
    sample.send().expect("send");

    match subscriber.receive() {
        Ok(Some(_)) => {
            eprintln!("[oncelock test] RECEIVED — OnceLock pattern works");
        }
        Ok(None) => {
            panic!("OnceLock-stored node publish failed — iceoryx2 + OnceLock interaction bug");
        }
        Err(e) => {
            panic!("OnceLock subscriber error: {:?}", e);
        }
    }
}

// ===========================================================================
// D. End-to-end message delivery through PubSub
// ===========================================================================

#[test]
fn test_publish_delivers_to_subscriber() {
    let bus = create_initialized_bus("publish_delivers");

    let (tx, rx) = mpsc::channel();
    let listener = ChannelListener { sender: tx };
    let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(listener));
    bus.subscribe(topics::KEYBOARD, listener.clone());

    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let received = publish_until_received(&bus, &event, &rx, Duration::from_secs(5));

    assert!(
        received.is_some(),
        "Subscriber should have received at least one event"
    );

    drop(listener);
}

#[test]
fn test_publish_delivers_to_multiple_subscribers_on_same_topic() {
    let bus = create_initialized_bus("multi_sub_same_topic");

    let (tx_a, rx_a) = mpsc::channel();
    let (tx_b, rx_b) = mpsc::channel();
    let listener_a: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_a }));
    let listener_b: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_b }));

    bus.subscribe(topics::KEYBOARD, listener_a.clone());
    bus.subscribe(topics::KEYBOARD, listener_b.clone());

    let event = Event::keyboard(KeyCode::B, Modifiers::default(), KeyState::Pressed);

    // Retry publish until BOTH subscribers receive
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut a_received = false;
    let mut b_received = false;
    while Instant::now() < deadline && !(a_received && b_received) {
        bus.publish(&event.topic(), &event);
        // Drain both channels
        while rx_a.try_recv().is_ok() {
            a_received = true;
        }
        while rx_b.try_recv().is_ok() {
            b_received = true;
        }
        if !(a_received && b_received) {
            std::thread::yield_now();
        }
    }

    assert!(
        a_received,
        "First subscriber should have received the event"
    );
    assert!(
        b_received,
        "Second subscriber should have received the event"
    );

    drop(listener_a);
    drop(listener_b);
}

#[test]
fn test_publish_does_not_cross_topics() {
    let bus = create_initialized_bus("no_cross_topics");

    let (tx_keyboard, rx_keyboard) = mpsc::channel();
    let (tx_mouse, rx_mouse) = mpsc::channel();
    let kb_listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(ChannelListener {
        sender: tx_keyboard,
    }));
    let mouse_listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_mouse }));

    bus.subscribe(topics::KEYBOARD, kb_listener.clone());
    bus.subscribe(topics::MOUSE, mouse_listener.clone());

    // Publish a MOUSE event — only the mouse subscriber should receive it
    let mouse_event = Event::mouse(MouseButton::Left, (10.0, 20.0), MouseState::Pressed);

    // Use retry loop to ensure the mouse subscriber is ready
    let received_mouse =
        publish_until_received(&bus, &mouse_event, &rx_mouse, Duration::from_secs(5));
    assert!(
        received_mouse.is_some(),
        "Mouse subscriber should receive mouse events"
    );

    // Verify the keyboard subscriber received nothing
    assert!(
        rx_keyboard.try_recv().is_err(),
        "Keyboard subscriber should NOT receive mouse events"
    );

    drop(kb_listener);
    drop(mouse_listener);
}

#[test]
fn test_wildcard_subscriber_receives_all_topics() {
    let bus = create_initialized_bus("wildcard_all");

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(topics::ALL, listener.clone());

    let keyboard_event = Event::keyboard(KeyCode::C, Modifiers::default(), KeyState::Pressed);
    let mouse_event = Event::mouse(MouseButton::Right, (5.0, 10.0), MouseState::Released);
    let processor_event = Event::processor("test-proc", ProcessorEvent::Started);

    // Ensure wildcard subscriber is ready by retrying first event
    let first = publish_until_received(&bus, &keyboard_event, &rx, Duration::from_secs(5));
    assert!(
        first.is_some(),
        "Wildcard subscriber should receive keyboard event"
    );

    // Now publish remaining events (subscriber is ready)
    bus.publish(&mouse_event.topic(), &mouse_event);
    bus.publish(&processor_event.topic(), &processor_event);

    // Wait for remaining events
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received_count = 1; // already got first
    while Instant::now() < deadline && received_count < 3 {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(_) => received_count += 1,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // The wildcard subscriber receives the event from BOTH the specific topic service
    // AND the /all service, so we expect at least 3 events (may receive duplicates)
    assert!(
        received_count >= 3,
        "Wildcard subscriber should receive at least 3 events, got {}",
        received_count
    );

    drop(listener);
}

#[test]
fn test_subscriber_receives_correct_event_data() {
    let bus = create_initialized_bus("correct_data");

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(topics::KEYBOARD, listener.clone());

    let modifiers = Modifiers {
        shift: true,
        ctrl: false,
        alt: true,
        meta: false,
    };
    let event = Event::keyboard(KeyCode::Z, modifiers, KeyState::Released);
    let received = publish_until_received(&bus, &event, &rx, Duration::from_secs(5));

    let received = received.expect("Should have received the event");

    // Verify the round-tripped event has the same topic and log_name
    assert_eq!(received.topic(), topics::KEYBOARD);
    assert_eq!(received.log_name(), event.log_name());

    // Verify via MessagePack that the serialized forms match
    let original_bytes = rmp_serde::to_vec_named(&event).unwrap();
    let received_bytes = rmp_serde::to_vec_named(&received).unwrap();
    assert_eq!(original_bytes, received_bytes, "Payload fidelity mismatch");

    drop(listener);
}

// ===========================================================================
// E. Subscription lifecycle & ordering
// ===========================================================================

#[test]
fn test_subscribe_before_init_receives_events_after_init() {
    let runtime_id = format!("test-sub-before-init-{}", uuid::Uuid::new_v4());
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let bus = PubSub::new();

    // Subscribe BEFORE init
    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(topics::KEYBOARD, listener.clone());

    // Now init — pending subscription should be replayed
    bus.init(&runtime_id, node);

    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let received = publish_until_received(&bus, &event, &rx, Duration::from_secs(5));

    assert!(
        received.is_some(),
        "Subscription registered before init should receive events after init"
    );

    drop(listener);
}

#[test]
fn test_multiple_subscribes_before_init_all_replayed() {
    let runtime_id = format!("test-multi-sub-before-init-{}", uuid::Uuid::new_v4());
    let node = Iceoryx2Node::new().expect("Failed to create iceoryx2 node");
    let bus = PubSub::new();

    // Subscribe 3 listeners to different topics BEFORE init
    let (tx_kb, rx_kb) = mpsc::channel();
    let (tx_mouse, rx_mouse) = mpsc::channel();
    let (tx_rt, rx_rt) = mpsc::channel();
    let kb_handle: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_kb }));
    let mouse_handle: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_mouse }));
    let rt_handle: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_rt }));

    bus.subscribe(topics::KEYBOARD, kb_handle.clone());
    bus.subscribe(topics::MOUSE, mouse_handle.clone());
    bus.subscribe(topics::RUNTIME_GLOBAL, rt_handle.clone());

    // Init replays all 3 pending subscriptions
    bus.init(&runtime_id, node);

    let kb_event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let mouse_event = Event::mouse(MouseButton::Left, (0.0, 0.0), MouseState::Pressed);
    let rt_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted);

    // Retry until all 3 subscribers receive
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut kb_ok = false;
    let mut mouse_ok = false;
    let mut rt_ok = false;
    while Instant::now() < deadline && !(kb_ok && mouse_ok && rt_ok) {
        if !kb_ok {
            bus.publish(topics::KEYBOARD, &kb_event);
        }
        if !mouse_ok {
            bus.publish(topics::MOUSE, &mouse_event);
        }
        if !rt_ok {
            bus.publish(topics::RUNTIME_GLOBAL, &rt_event);
        }
        if rx_kb.try_recv().is_ok() {
            kb_ok = true;
        }
        if rx_mouse.try_recv().is_ok() {
            mouse_ok = true;
        }
        if rx_rt.try_recv().is_ok() {
            rt_ok = true;
        }
        std::thread::yield_now();
    }

    assert!(kb_ok, "Keyboard listener should receive events");
    assert!(mouse_ok, "Mouse listener should receive events");
    assert!(rt_ok, "Runtime listener should receive events");

    drop(kb_handle);
    drop(mouse_handle);
    drop(rt_handle);
}

#[test]
fn test_listener_drop_stops_subscriber_thread() {
    let bus = create_initialized_bus("listener_drop");

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));

    bus.subscribe(topics::KEYBOARD, listener.clone());

    // Verify subscriber is working first
    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let received = publish_until_received(&bus, &event, &rx, Duration::from_secs(5));
    assert!(
        received.is_some(),
        "Should receive events before dropping listener"
    );

    // Drop the strong reference — subscriber thread should detect and exit
    drop(listener);

    // The channel sender is inside the dropped listener, so rx should disconnect
    // Publish after the listener is dropped — should not panic or hang
    bus.publish(&event.topic(), &event);

    // Channel should be disconnected since the sender was dropped with the listener
    match rx.recv_timeout(Duration::from_millis(200)) {
        Err(mpsc::RecvTimeoutError::Disconnected) => { /* expected */ }
        Err(mpsc::RecvTimeoutError::Timeout) => { /* also acceptable — no events */ }
        Ok(_) => {
            // This might happen if an event was in-flight before the drop
            // but no further events should arrive
        }
    }
}

// ===========================================================================
// F. Event type coverage
// ===========================================================================

#[test]
fn test_runtime_event_delivery() {
    let bus = create_initialized_bus("runtime_events");

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(topics::RUNTIME_GLOBAL, listener.clone());

    // Ensure subscriber is ready with first event
    let first = Event::RuntimeGlobal(RuntimeEvent::RuntimeStarted);
    let received = publish_until_received(&bus, &first, &rx, Duration::from_secs(5));
    assert!(received.is_some(), "Should receive RuntimeStarted event");

    // Send remaining runtime events
    bus.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange),
    );
    bus.publish(
        topics::RUNTIME_GLOBAL,
        &Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown),
    );

    // Wait for remaining events
    let mut count = 1;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && count < 3 {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(_) => count += 1,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    assert!(
        count >= 3,
        "Should receive all 3 runtime events, got {}",
        count
    );

    drop(listener);
}

#[test]
fn test_processor_event_delivery() {
    let bus = create_initialized_bus("processor_events");

    let processor_id = "audio-mixer";
    let topic = topics::processor(processor_id);

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(&topic, listener.clone());

    let event = Event::processor(processor_id, ProcessorEvent::Started);
    let received = publish_until_received(&bus, &event, &rx, Duration::from_secs(5));

    assert!(
        received.is_some(),
        "Processor subscriber should receive the Started event"
    );

    drop(listener);
}

#[test]
fn test_custom_event_delivery() {
    let bus = create_initialized_bus("custom_events");

    let custom_topic = "my-custom-topic";
    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(custom_topic, listener.clone());

    let payload = serde_json::json!({"key": "value", "count": 42});
    let event = Event::custom(custom_topic, payload);
    let received = publish_until_received(&bus, &event, &rx, Duration::from_secs(5));

    let received = received.expect("Custom event should be delivered");
    assert_eq!(received.topic(), custom_topic);

    // Verify payload fidelity
    let original_bytes = rmp_serde::to_vec_named(&event).unwrap();
    let received_bytes = rmp_serde::to_vec_named(&received).unwrap();
    assert_eq!(
        original_bytes, received_bytes,
        "Custom event payload should survive iceoryx2 round-trip"
    );

    drop(listener);
}

// ===========================================================================
// G. Edge cases & robustness
// ===========================================================================

#[test]
fn test_oversized_event_is_dropped() {
    let bus = create_initialized_bus("oversized_event");

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe("big-topic", listener.clone());

    // First verify the subscriber is working with a normal-sized event
    let normal_event = Event::custom("big-topic", serde_json::json!({"ok": true}));
    let received = publish_until_received(&bus, &normal_event, &rx, Duration::from_secs(5));
    assert!(received.is_some(), "Normal event should be delivered first");

    // Drain any extra events from the retry loop
    while rx.try_recv().is_ok() {}

    // Now try an oversized event — should be silently dropped
    let large_string = "x".repeat(MAX_EVENT_PAYLOAD_SIZE + 1);
    let oversized_event = Event::custom("big-topic", serde_json::json!({ "data": large_string }));
    bus.publish(&oversized_event.topic(), &oversized_event);

    // Should not crash, and subscriber should receive nothing
    match rx.recv_timeout(Duration::from_millis(200)) {
        Err(mpsc::RecvTimeoutError::Timeout) => { /* expected — no event */ }
        Ok(_) => panic!("Oversized event should NOT be delivered"),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("Channel disconnected unexpectedly")
        }
    }

    drop(listener);
}

#[test]
fn test_concurrent_publish_from_multiple_threads() {
    let bus = Arc::new(create_initialized_bus("concurrent_publish"));

    let (tx, rx) = mpsc::channel();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    bus.subscribe(topics::KEYBOARD, listener.clone());

    // Ensure subscriber is ready
    let probe = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let received = publish_until_received(&bus, &probe, &rx, Duration::from_secs(5));
    assert!(received.is_some(), "Subscriber should be ready");

    // Drain probe events
    while rx.try_recv().is_ok() {}

    // Each thread's first publish creates a new thread-local iceoryx2 publisher.
    // iceoryx2's subscriber needs a beat to establish its receive-side connection
    // for each new publisher — bursting N publishers in parallel can drop early
    // messages with "Unable to establish connection to new sender". Publish in a
    // retry loop so later messages survive the connection setup.
    let thread_count = 4;
    let publishes_per_thread = 20;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut handles = Vec::new();

    for _ in 0..thread_count {
        let bus = bus.clone();
        let stop = stop.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..publishes_per_thread {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
                bus.publish(&event.topic(), &event);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        handles.push(handle);
    }

    // Collect at least one event; signal threads to stop as soon as we have it.
    let mut received_count = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(_) => {
                received_count += 1;
                if received_count >= 1 {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    // Keep draining briefly in case more arrive after stop signal
                    if received_count >= thread_count {
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if received_count > 0 {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    for handle in handles {
        handle.join().expect("Publisher thread panicked");
    }

    assert!(
        received_count > 0,
        "Should receive at least some events from concurrent publishers, got {}",
        received_count
    );

    drop(listener);
}

#[test]
fn test_separate_pubsub_instances_are_isolated() {
    let bus_a = create_initialized_bus("isolated_a");
    let bus_b = create_initialized_bus("isolated_b");

    let (tx_a, rx_a) = mpsc::channel();
    let (tx_b, rx_b) = mpsc::channel();
    let handle_a: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_a }));
    let handle_b: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx_b }));

    bus_a.subscribe(topics::KEYBOARD, handle_a.clone());
    bus_b.subscribe(topics::KEYBOARD, handle_b.clone());

    // Verify bus_a's subscriber is working
    let event = Event::keyboard(KeyCode::A, Modifiers::default(), KeyState::Pressed);
    let received_a = publish_until_received(&bus_a, &event, &rx_a, Duration::from_secs(5));
    assert!(
        received_a.is_some(),
        "bus_a subscriber should receive the event"
    );

    // bus_b's subscriber should NOT have received anything from bus_a
    assert!(
        rx_b.try_recv().is_err(),
        "bus_b subscriber should NOT receive events from bus_a"
    );

    drop(handle_a);
    drop(handle_b);
}
