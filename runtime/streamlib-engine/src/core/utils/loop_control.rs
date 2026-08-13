// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::Result;
use crate::core::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

    // The Arc must outlive the loop: the bus stores only a Weak, so dropping it
    // unsubscribes.
    let listener_arc: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(listener));
    PUBSUB.subscribe(topics::RUNTIME_GLOBAL, Arc::clone(&listener_arc));

    tracing::info!(
        "Shutdown-aware loop started, subscribed to {}",
        topics::RUNTIME_GLOBAL
    );

    // Main loop
    loop {
        // The latch is polled as well as the event because a request latched
        // before this loop started leaves no event to receive.
        if shutdown_flag.load(Ordering::Relaxed)
            || crate::core::runtime::is_runtime_shutdown_requested()
        {
            tracing::info!("Shutdown observed, exiting loop");
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

    /// A request latched before the loop subscribes leaves no event to receive,
    /// so the loop must read the latch too. The callback breaks itself after a
    /// bounded number of iterations so a mental-revert (dropping the
    /// `is_runtime_shutdown_requested()` term) fails the `iterations == 0`
    /// assertion instead of spinning until a harness timeout.
    #[test]
    #[serial]
    fn latched_shutdown_request_exits_the_loop_without_an_event() {
        let _latch_cleared_even_on_unwind =
            crate::core::runtime::RuntimeShutdownRequestLatchClearedOnDrop::clear_now_and_on_drop();
        crate::core::runtime::request_runtime_shutdown("unit test")
            .expect("the host arm never fails");

        const ITERATIONS_BEFORE_THE_CALLBACK_BREAKS_ITSELF: usize = 4;
        let mut iterations = 0;
        let result = shutdown_aware_loop(|| {
            iterations += 1;
            if iterations >= ITERATIONS_BEFORE_THE_CALLBACK_BREAKS_ITSELF {
                return Ok::<LoopControl, ()>(LoopControl::Break);
            }
            Ok::<LoopControl, ()>(LoopControl::Continue)
        });

        assert!(result.is_ok());
        assert_eq!(
            iterations, 0,
            "the latch is checked before the first user callback"
        );
    }

    #[test]
    #[serial]
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let (done_tx, done_rx) = mpsc::channel::<std::result::Result<(), ()>>();
        let (entered_loop_tx, entered_loop_rx) = mpsc::channel::<()>();

        std::thread::spawn(move || {
            let result = shutdown_aware_loop(|| {
                // The callback runs only after `shutdown_aware_loop` has
                // subscribed, so the first invocation is the handshake this
                // thread owes the publisher below.
                if counter_clone.fetch_add(1, Ordering::Relaxed) == 0 {
                    let _ = entered_loop_tx.send(());
                }
                std::thread::sleep(Duration::from_millis(10));
                Ok::<LoopControl, ()>(LoopControl::Continue)
            });
            done_tx.send(result).ok();
        });

        // Synchronising on the loop having started, not on a duration: the
        // subscription is live the moment `shutdown_aware_loop` registers it, so
        // the entry handshake is the only thing left to wait for.
        entered_loop_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the loop thread entered its callback");

        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);

        // Bounded so a regression fails by name rather than hanging the suite.
        match done_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => assert!(result.is_ok(), "Loop returned an error"),
            Err(_) => panic!(
                "test_shutdown_event_exits_loop: loop did not exit within 5 s \
                 after the shutdown event"
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
    #[serial]
    fn test_error_propagation() {
        let result = shutdown_aware_loop(|| Err::<LoopControl, &str>("test error"));

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "test error");
    }
}
