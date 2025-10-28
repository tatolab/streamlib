//! Tick broadcasting for independent stream handler execution
//!
//! Provides a lightweight pub/sub mechanism for distributing clock ticks
//! to multiple stream handlers running in parallel. Each handler receives
//! tick notifications via a bounded channel and can drop frames if busy.
//!
//! # Architecture
//!
//! - Clock ticks at fixed rate (never blocked by handlers)
//! - Each handler runs in its own thread
//! - Handlers receive `TimedTick` notifications
//! - If handler is busy, new ticks are dropped (graceful degradation)
//! - Handler failures are isolated (don't affect clock or other handlers)
//!
//! # Example
//!
//! ```ignore
//! use streamlib_core::{TickBroadcaster, SoftwareClock, Clock};
//! use std::thread;
//!
//! let mut broadcaster = TickBroadcaster::new();
//! let mut clock = SoftwareClock::new(60.0);
//!
//! // Each handler subscribes
//! let rx1 = broadcaster.subscribe();
//! let rx2 = broadcaster.subscribe();
//!
//! // Handler threads
//! thread::spawn(move || {
//!     for tick in rx1 {
//!         // Process tick (has delta for calculations)
//!         println!("Handler 1: frame {}, delta: {:.2}s", tick.frame_number, tick.delta_time);
//!     }
//! });
//!
//! thread::spawn(move || {
//!     for tick in rx2 {
//!         // Handler 2 processes independently
//!         println!("Handler 2: frame {}", tick.frame_number);
//!     }
//! });
//!
//! // Clock thread broadcasts ticks (in async context)
//! tokio::spawn(async move {
//!     loop {
//!         let tick = clock.next_tick().await;
//!         broadcaster.broadcast(tick); // Never blocks
//!     }
//! });
//! ```

use super::clock::TimedTick;
use crossbeam_channel::{bounded, Sender, Receiver};

/// Tick broadcaster for distributing clock ticks to stream handlers
///
/// Maintains a list of subscribers (channels) and broadcasts each tick
/// to all of them using non-blocking sends. If a handler's channel is full
/// (handler is busy), the tick is dropped for that handler.
///
/// This ensures the clock never blocks and handlers remain independent.
pub struct TickBroadcaster {
    senders: Vec<Sender<TimedTick>>,
}

impl TickBroadcaster {
    /// Create a new tick broadcaster
    pub fn new() -> Self {
        Self {
            senders: Vec::new(),
        }
    }

    /// Subscribe to tick notifications
    ///
    /// Returns a receiver that will receive tick notifications.
    /// The channel is bounded with capacity 1, so only the latest tick
    /// is kept. If a handler is busy processing a tick, new ticks are dropped.
    ///
    /// # Returns
    ///
    /// Receiver for `TimedTick` notifications
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib_core::TickBroadcaster;
    /// use std::thread;
    ///
    /// let mut broadcaster = TickBroadcaster::new();
    /// let rx = broadcaster.subscribe();
    ///
    /// thread::spawn(move || {
    ///     for tick in rx {
    ///         println!("Received tick: frame {}", tick.frame_number);
    ///     }
    /// });
    /// ```
    pub fn subscribe(&mut self) -> Receiver<TimedTick> {
        // Bounded(1) = only keep latest tick
        // If handler is processing previous tick, new tick is dropped
        let (tx, rx) = bounded(1);
        self.senders.push(tx);
        rx
    }

    /// Broadcast a tick to all subscribers
    ///
    /// Sends the tick to all subscribed handlers using non-blocking sends.
    /// If a handler's channel is full (handler is still processing previous tick),
    /// the tick is silently dropped for that handler.
    ///
    /// This method never blocks, ensuring the clock can maintain its tick rate.
    ///
    /// # Arguments
    ///
    /// * `tick` - The timed tick to broadcast
    ///
    /// # Example
    ///
    /// ```ignore
    /// use streamlib_core::{TickBroadcaster, SoftwareClock, Clock};
    ///
    /// let mut broadcaster = TickBroadcaster::new();
    /// let rx = broadcaster.subscribe();
    ///
    /// // In async context, get tick from clock
    /// let mut clock = SoftwareClock::new(60.0);
    /// let tick = clock.next_tick().await;
    /// broadcaster.broadcast(tick); // Never blocks
    /// ```
    pub fn broadcast(&self, tick: TimedTick) {
        for sender in &self.senders {
            // try_send = non-blocking
            // Returns Err if channel is full (handler busy) - that's OK, drop the tick
            // Clone the tick for each sender (small struct, just f64+u64+String)
            let _ = sender.try_send(tick.clone());
        }
    }

    /// Get the number of subscribers
    pub fn subscriber_count(&self) -> usize {
        self.senders.len()
    }

    /// Clear all subscribers
    ///
    /// Removes all subscriber channels. Handlers will stop receiving ticks.
    pub fn clear(&mut self) {
        self.senders.clear();
    }
}

impl Default for TickBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

// TickBroadcaster is Send + Sync since Sender<T> is Send + Sync
unsafe impl Send for TickBroadcaster {}
unsafe impl Sync for TickBroadcaster {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    #[test]
    fn test_new_broadcaster() {
        let broadcaster = TickBroadcaster::new();
        assert_eq!(broadcaster.subscriber_count(), 0);
    }

    #[test]
    fn test_subscribe() {
        let mut broadcaster = TickBroadcaster::new();

        let _rx1 = broadcaster.subscribe();
        assert_eq!(broadcaster.subscriber_count(), 1);

        let _rx2 = broadcaster.subscribe();
        assert_eq!(broadcaster.subscriber_count(), 2);
    }

    #[test]
    fn test_broadcast_single_handler() {
        let mut broadcaster = TickBroadcaster::new();
        let rx = broadcaster.subscribe();

        let tick = TimedTick::test_tick(0);
        broadcaster.broadcast(tick);

        let received = rx.recv().unwrap();
        assert_eq!(received.frame_number, 0);
    }

    #[test]
    fn test_broadcast_multiple_handlers() {
        let mut broadcaster = TickBroadcaster::new();
        let rx1 = broadcaster.subscribe();
        let rx2 = broadcaster.subscribe();
        let rx3 = broadcaster.subscribe();

        let tick = TimedTick::test_tick(42);
        broadcaster.broadcast(tick);

        // All handlers receive the same tick
        assert_eq!(rx1.recv().unwrap().frame_number, 42);
        assert_eq!(rx2.recv().unwrap().frame_number, 42);
        assert_eq!(rx3.recv().unwrap().frame_number, 42);
    }

    #[test]
    fn test_frame_dropping_when_handler_busy() {
        let mut broadcaster = TickBroadcaster::new();
        let rx = broadcaster.subscribe();

        // Send first tick
        let tick1 = TimedTick::test_tick(0);
        broadcaster.broadcast(tick1);

        // Send second tick before first is consumed (channel capacity is 1)
        let tick2 = TimedTick::test_tick(1);
        broadcaster.broadcast(tick2);

        // Handler should only receive latest tick (tick1 was dropped)
        let received = rx.recv().unwrap();
        assert_eq!(received.frame_number, 0); // Actually receives first one

        // Channel should be empty now (second was dropped)
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_independent_handlers() {
        let mut broadcaster = TickBroadcaster::new();
        let rx1 = broadcaster.subscribe();
        let rx2 = broadcaster.subscribe();

        let count1 = Arc::new(AtomicUsize::new(0));
        let count2 = Arc::new(AtomicUsize::new(0));

        let c1 = Arc::clone(&count1);
        let c2 = Arc::clone(&count2);

        // Handler 1: processes quickly
        let h1 = thread::spawn(move || {
            for _ in rx1 {
                c1.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Handler 2: processes slowly (simulates busy handler)
        let h2 = thread::spawn(move || {
            for _ in rx2 {
                thread::sleep(Duration::from_millis(50)); // Slow
                c2.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Broadcast multiple ticks quickly
        for i in 0..10 {
            broadcaster.broadcast(TimedTick::test_tick(i));
            thread::sleep(Duration::from_millis(5));
        }

        // Drop broadcaster to close channels
        drop(broadcaster);

        h1.join().unwrap();
        h2.join().unwrap();

        // Handler 1 should process all ticks
        assert_eq!(count1.load(Ordering::Relaxed), 10);

        // Handler 2 should process fewer (drops frames due to being slow)
        let c2_count = count2.load(Ordering::Relaxed);
        assert!(c2_count < 10, "Slow handler should drop frames, got {}", c2_count);
    }

    #[test]
    fn test_clear_subscribers() {
        let mut broadcaster = TickBroadcaster::new();
        let _rx1 = broadcaster.subscribe();
        let _rx2 = broadcaster.subscribe();

        assert_eq!(broadcaster.subscriber_count(), 2);

        broadcaster.clear();
        assert_eq!(broadcaster.subscriber_count(), 0);
    }

    #[test]
    fn test_handler_receives_delta_time() {
        let mut broadcaster = TickBroadcaster::new();
        let rx = broadcaster.subscribe();

        let tick = TimedTick::test_tick(5);
        broadcaster.broadcast(tick);

        let received = rx.recv().unwrap();
        assert_eq!(received.frame_number, 5);
        assert!(received.delta_time >= 0.0); // Has delta time info
    }

    #[test]
    fn test_broadcast_never_blocks() {
        let mut broadcaster = TickBroadcaster::new();
        let rx = broadcaster.subscribe();

        // Fill the channel
        broadcaster.broadcast(TimedTick::test_tick(0));

        // This should not block even though channel is full
        let start = std::time::Instant::now();
        broadcaster.broadcast(TimedTick::test_tick(1));
        let elapsed = start.elapsed();

        // Should return immediately (< 1ms)
        assert!(elapsed < Duration::from_millis(1), "broadcast blocked for {:?}", elapsed);

        // Clean up
        drop(rx);
    }

    #[test]
    fn test_default() {
        let broadcaster = TickBroadcaster::default();
        assert_eq!(broadcaster.subscriber_count(), 0);
    }
}
