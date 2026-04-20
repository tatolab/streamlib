// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::pubsub::{topics, Event, EventListener, RuntimeEvent, PUBSUB};
use crate::core::Result;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Control flow for shutdown-aware loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopControl {
    Continue,
    Break,
}

struct ShutdownListener {
    shutdown_flag: Arc<AtomicBool>,
}

impl EventListener for ShutdownListener {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        tracing::info!("ShutdownListener received event: {:?}", event);
        // Check if this is a shutdown event
        if let Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown) = event {
            tracing::info!("Shutdown event received in loop listener, setting flag");
            self.shutdown_flag.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Run a loop that automatically exits on shutdown events.
pub fn shutdown_aware_loop<F, E>(mut f: F) -> std::result::Result<(), E>
where
    F: FnMut() -> std::result::Result<LoopControl, E>,
{
    // Create shutdown flag
    let shutdown_flag = Arc::new(AtomicBool::new(false));

    // Create listener that sets the flag
    let listener = ShutdownListener {
        shutdown_flag: Arc::clone(&shutdown_flag),
    };

    // Subscribe to runtime global events (includes shutdown)
    // IMPORTANT: We must keep the Arc alive for the duration of the loop!
    // The event bus stores only weak references, so if we drop the Arc, the listener is lost.
    let listener_arc: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(listener));
    PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener_arc));

    tracing::info!(
        "Shutdown-aware loop started, subscribed to {}",
        topics::RUNTIME_GLOBAL
    );

    // Main loop
    loop {
        // Check shutdown flag (non-blocking, ~2ns)
        if shutdown_flag.load(Ordering::Relaxed) {
            tracing::info!("Shutdown event received, exiting loop");
            return Ok(());
        }

        // Execute user logic
        match f()? {
            LoopControl::Continue => continue,
            LoopControl::Break => {
                tracing::trace!("Loop exited via LoopControl::Break");
                return Ok(());
            }
        }
    }

    // Subscription auto-drops here, unsubscribing from event bus
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pubsub::PUBSUB;
    use serial_test::serial;

    #[test]
    fn test_loop_control_break() {
        let mut count = 0;

        let result = shutdown_aware_loop(|| {
            count += 1;
            if count >= 5 {
                return Ok(LoopControl::Break);
            }
            Ok::<LoopControl, ()>(LoopControl::Continue)
        });

        assert!(result.is_ok());
        assert_eq!(count, 5);
    }

    #[test]
    #[serial]
    fn test_shutdown_event_exits_loop() {
        use crate::iceoryx2::Iceoryx2Node;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::sync::Arc;
        use std::time::Duration;

        // Ensure PUBSUB has an iceoryx2 backend. Use a process-unique runtime_id
        // so iceoryx2's persistent service state under /tmp/iceoryx2/ doesn't
        // collide with stale state left by crashed prior cargo-test invocations
        // (which surfaced as PublishSubscribeOpenError(ServiceInCorruptedState)).
        // If PUBSUB was already initialized by another test in this process,
        // init() is a no-op (OnceLock), and the existing runtime_id is used.
        if let Ok(node) = Iceoryx2Node::new() {
            let runtime_id = format!("test-loop-control-{}", uuid::Uuid::new_v4());
            PUBSUB.init(&runtime_id, node);
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let (done_tx, done_rx) = mpsc::channel::<std::result::Result<(), ()>>();

        std::thread::spawn(move || {
            let result = shutdown_aware_loop(|| {
                counter_clone.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10));
                Ok::<LoopControl, ()>(LoopControl::Continue)
            });
            done_tx.send(result).ok();
        });

        // Give the iceoryx2 subscriber thread time to open the service and
        // start polling before we send the shutdown event.
        std::thread::sleep(Duration::from_millis(150));

        // Publish shutdown event
        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);

        // Wait for loop to exit with a hard timeout so the test fails clearly
        // rather than hanging indefinitely when PUBSUB is not functional.
        match done_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => assert!(result.is_ok(), "Loop returned an error"),
            Err(_) => panic!(
                "test_shutdown_event_exits_loop: loop did not exit within 5 s \
                 after shutdown event — PUBSUB may be uninitialized or the \
                 iceoryx2 subscriber thread failed to open its service"
            ),
        }

        // Loop should have run at least once but stopped after shutdown
        let final_count = counter.load(Ordering::Relaxed);
        assert!(final_count > 0, "Loop should have run at least once");
        assert!(
            final_count < 100,
            "Loop should have stopped after shutdown event"
        );
    }

    #[test]
    fn test_error_propagation() {
        let result = shutdown_aware_loop(|| Err::<LoopControl, &str>("test error"));

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "test error");
    }
}
