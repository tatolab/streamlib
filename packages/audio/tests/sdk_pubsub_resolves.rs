// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Locks the SDK pubsub facade path for non-engine, non-SDK consumers.
//!
//! This crate is a domain package (`streamlib-audio`) that depends on
//! `streamlib` (the SDK facade) and NOT on `streamlib-engine`. If
//! `streamlib::sdk::pubsub::*` stops resolving here, packages can no
//! longer publish runtime events (e.g. `RuntimeEvent::RuntimeShutdown`
//! from a display package on window-close) without reaching past the
//! SDK boundary.

use parking_lot::Mutex;
use std::sync::Arc;
use streamlib::sdk::pubsub::{
    topics, Event, EventListener, ProcessorEvent, RuntimeEvent, PUBSUB,
};

struct NoopListener;

impl EventListener for NoopListener {
    fn on_event(&mut self, _event: &Event) -> streamlib::sdk::error::Result<()> {
        Ok(())
    }
}

#[test]
fn sdk_pubsub_surface_is_reachable_from_consumer_crate() {
    // Topic constants resolve.
    assert_eq!(topics::RUNTIME_GLOBAL, "runtime:global");

    // Event helper constructors are reachable through the SDK path.
    let shutdown = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
    assert_eq!(shutdown.topic(), topics::RUNTIME_GLOBAL);

    let proc_started = Event::processor("audio-mixer", ProcessorEvent::Started);
    assert_eq!(proc_started.topic(), topics::processor("audio-mixer"));

    // `PUBSUB` static is reachable; `subscribe` accepts our listener.
    // Pre-init this buffers, which is exactly the documented behavior —
    // we just need the call to compile and run without panic, not to
    // deliver the event.
    let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(NoopListener));
    PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener));
}
