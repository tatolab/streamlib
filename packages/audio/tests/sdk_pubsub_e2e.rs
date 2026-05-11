// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end verification that the SDK pubsub facade actually delivers
//! events through iceoryx2 — not just that the path resolves.
//!
//! Mirrors the engine-side `pubsub::integration_tests` pattern but
//! routes every call through `streamlib::sdk::pubsub::*`, so a
//! regression in the SDK re-export (vtable / linkage / `LazyLock`
//! handling across the crate boundary) fails this test rather than
//! silently dropping events at runtime.
//!
//! Lives in its own test binary (cargo compiles each `tests/*.rs`
//! file as a separate `--test` target). The global `PUBSUB.init` is
//! per-process; isolating from the pre-init checks in
//! `sdk_pubsub_resolves.rs` requires running in a fresh process,
//! which `cargo test` provides automatically.

use parking_lot::Mutex;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use streamlib::sdk::iceoryx2::Iceoryx2Node;
use streamlib::sdk::pubsub::{topics, Event, EventListener, RuntimeEvent, PUBSUB};

struct ChannelListener {
    sender: mpsc::Sender<Event>,
}

impl EventListener for ChannelListener {
    fn on_event(&mut self, event: &Event) -> streamlib::sdk::error::Result<()> {
        let _ = self.sender.send(event.clone());
        Ok(())
    }
}

/// Unique runtime_id per test process — avoids iceoryx2 service-name
/// collisions with other test binaries running concurrently in CI.
fn unique_runtime_id() -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sdk-pubsub-e2e-{}-{}", pid, nanos)
}

#[test]
fn runtime_shutdown_published_via_sdk_path_is_delivered() {
    // Initialize the global PUBSUB through the SDK re-export. The
    // existing pubsub integration tests construct ad-hoc `PubSub`
    // instances against `super::bus::PubSub`, which is not part of
    // the SDK surface — consumer code only ever sees the global. If
    // the SDK re-export doesn't preserve the `LazyLock<PubSub>`
    // identity (e.g. duplicated through a different statics
    // boundary), `init()` would race or noop and the subscribe path
    // would silently drop the event.
    let node = Iceoryx2Node::new().expect("create iceoryx2 node");
    PUBSUB.init(&unique_runtime_id(), node);

    // Subscribe a listener through the SDK path.
    let (tx, rx) = mpsc::channel::<Event>();
    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(ChannelListener { sender: tx }));
    PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener));

    // Give the subscriber thread a moment to spin up its iceoryx2
    // subscriber — `subscribe` returns immediately while the
    // subscriber thread is still in `open_or_create_event_service`.
    // Per docs/learnings/pubsub-lazy-init-silent-noop.md, ~150ms is
    // the documented ballpark; the retry loop below handles the
    // remaining race.
    std::thread::sleep(Duration::from_millis(150));

    // Publish through the SDK path in a retry loop until the
    // subscriber receives (or we time out).
    let event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
    let deadline = Instant::now() + Duration::from_secs(5);
    let received = loop {
        PUBSUB.publish(&event.topic(), &event);
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(received) => break Some(received),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    break None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
        }
    };

    let received = received.expect(
        "subscriber did not receive RuntimeShutdown within 5s — \
         SDK pubsub re-export may have broken iceoryx2 delivery",
    );

    // Confirm the round-trip preserved the event payload — not just
    // any event, but the specific RuntimeShutdown we published.
    assert_eq!(received.topic(), topics::RUNTIME_GLOBAL);
    match received {
        Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown) => {}
        other => panic!("expected RuntimeShutdown, got {:?}", other.log_name()),
    }
}
