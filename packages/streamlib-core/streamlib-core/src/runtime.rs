//! StreamRuntime - Wires clock, broadcaster, and handlers together
//!
//! This module implements the runtime that manages the complete streaming pipeline:
//! - Clock generates ticks at fixed rate (async tokio task)
//! - TickBroadcaster distributes ticks to all handlers (non-blocking)
//! - Handlers run in OS threads, process ticks independently
//!
//! # Architecture
//!
//! ```text
//! Clock Task (tokio)          TickBroadcaster           Handler Threads (OS)
//!       │                            │                           │
//!       ├─ tick()──────────────────►│                           │
//!       │                            ├─ broadcast()────────────►│ Handler 1
//!       │                            │                           │  └─ process(tick)
//!       │                            ├─ broadcast()────────────►│ Handler 2
//!       │                            │                           │  └─ process(tick)
//!       │                            ├─ broadcast()────────────►│ Handler 3
//!       │                            │                           │  └─ process(tick)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use streamlib_core::{StreamRuntime, Stream, StreamHandler, TimedTick, Result};
//!
//! // Create runtime at 60fps
//! let mut runtime = StreamRuntime::new(60.0);
//!
//! // Add handlers
//! runtime.add_stream(Stream::new(Box::new(camera_handler)));
//! runtime.add_stream(Stream::new(Box::new(blur_handler)));
//! runtime.add_stream(Stream::new(Box::new(display_handler)));
//!
//! // Connect handlers
//! runtime.connect(camera_handler.id(), "video", blur_handler.id(), "video");
//! runtime.connect(blur_handler.id(), "video", display_handler.id(), "video");
//!
//! // Start runtime (spawns clock + handler threads)
//! runtime.start().await?;
//!
//! // Run until stopped
//! runtime.run().await?;
//! ```

use crate::clock::{Clock, SoftwareClock};
use crate::events::TickBroadcaster;
use crate::stream_processor::StreamProcessor;
use anyhow::{Result, anyhow};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Opaque shader ID (for future GPU operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(pub u64);

/// StreamRuntime - Manages clock, handlers, and connections
///
/// The runtime is the central coordinator that:
/// 1. Owns the clock (generates ticks)
/// 2. Owns the broadcaster (distributes ticks)
/// 3. Spawns handler threads (processes ticks)
/// 4. Manages connections between handlers
///
/// # Threading Model
///
/// - **Clock**: Runs in tokio task (async, single thread)
/// - **Handlers**: Each runs in OS thread (true parallelism)
/// - **Broadcaster**: Lock-free channels (crossbeam)
///
/// # Lifecycle
///
/// 1. Create runtime: `StreamRuntime::new(fps)`
/// 2. Add streams: `runtime.add_stream(stream)`
/// 3. Connect ports: `runtime.connect(...)`
/// 4. Start: `runtime.start().await` - spawns threads
/// 5. Run: `runtime.run().await` - blocks until stopped
/// 6. Stop: `runtime.stop().await` - clean shutdown
pub struct StreamRuntime {
    /// Clock for generating ticks (will be moved to tokio task on start)
    clock: Option<Box<dyn Clock + Send>>,

    /// FPS for runtime (stored for status/debugging)
    fps: f64,

    /// Broadcaster for distributing ticks to handlers
    broadcaster: Arc<Mutex<TickBroadcaster>>,

    /// Processors waiting to be started (drained on start)
    pending_processors: Vec<Box<dyn StreamProcessor>>,

    /// Handler threads (spawned on start)
    handler_threads: Vec<JoinHandle<()>>,

    /// Clock task handle (spawned on start)
    clock_task: Option<tokio::task::JoinHandle<()>>,

    /// Running flag
    running: bool,
}

impl StreamRuntime {
    /// Create a new runtime with software clock
    ///
    /// # Arguments
    ///
    /// * `fps` - Target frames per second for clock
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib_core::StreamRuntime;
    ///
    /// let runtime = StreamRuntime::new(60.0);
    /// ```
    pub fn new(fps: f64) -> Self {
        Self {
            clock: Some(Box::new(SoftwareClock::new(fps))),
            fps,
            broadcaster: Arc::new(Mutex::new(TickBroadcaster::new())),
            pending_processors: Vec::new(),
            handler_threads: Vec::new(),
            clock_task: None,
            running: false,
        }
    }

    /// Create runtime with custom clock
    ///
    /// Use this to provide PTP or Genlock clocks instead of software clock.
    ///
    /// # Arguments
    ///
    /// * `clock` - Custom clock implementation
    /// * `fps` - Nominal FPS (for status reporting)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use streamlib_core::{StreamRuntime, PTPClock};
    ///
    /// let ptp_clock = PTPClock::new(60.0);
    /// let runtime = StreamRuntime::with_clock(Box::new(ptp_clock), 60.0);
    /// ```
    pub fn with_clock(clock: Box<dyn Clock + Send>, fps: f64) -> Self {
        Self {
            clock: Some(clock),
            fps,
            broadcaster: Arc::new(Mutex::new(TickBroadcaster::new())),
            pending_processors: Vec::new(),
            handler_threads: Vec::new(),
            clock_task: None,
            running: false,
        }
    }

    /// Add a processor to the runtime
    ///
    /// Processors are held until `start()` is called, then spawned into threads.
    ///
    /// # Arguments
    ///
    /// * `processor` - Boxed processor implementation
    ///
    /// # Example
    ///
    /// ```ignore
    /// use streamlib_core::StreamRuntime;
    ///
    /// let mut runtime = StreamRuntime::new(60.0);
    ///
    /// let processor = MyProcessor::new();
    /// runtime.add_processor(Box::new(processor));
    /// ```
    pub fn add_processor(&mut self, processor: Box<dyn StreamProcessor>) {
        if self.running {
            eprintln!("[Runtime] Warning: Cannot add processor while running");
            return;
        }

        tracing::info!("Added processor");
        self.pending_processors.push(processor);
    }

    pub fn connect<T: crate::ports::PortMessage>(&mut self, output: &mut crate::ports::StreamOutput<T>, input: &mut crate::ports::StreamInput<T>) -> Result<()> {
        if self.running {
            return Err(anyhow!("Cannot connect ports while runtime is running"));
        }

        input.connect(output.buffer().clone());
        Ok(())
    }

    /// Start the runtime
    ///
    /// This spawns:
    /// 1. Clock task (tokio) - generates ticks
    /// 2. Handler threads (OS) - process ticks
    ///
    /// After calling start(), the runtime is running and handlers
    /// begin receiving ticks.
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.add_stream(stream1);
    /// runtime.add_stream(stream2);
    /// runtime.start().await?;
    /// ```
    pub async fn start(&mut self) -> Result<()> {
        if self.running {
            return Err(anyhow!("Runtime already running"));
        }

        let handler_count = self.pending_processors.len();

        tracing::info!("Starting runtime with {} processors at {}fps",
                      handler_count, self.fps);

        // Warn if handler count seems high relative to CPU cores
        let core_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        if handler_count > core_count * 2 {
            tracing::warn!(
                "Runtime has {} handlers but only {} CPU cores available. \
                 This may cause thread scheduling overhead. \
                 Consider using fewer, more complex handlers or a thread pool.",
                handler_count,
                core_count
            );
        }

        self.running = true;

        // 1. Spawn clock task (tokio async)
        self.spawn_clock_task()?;

        // 2. Spawn handler threads (OS threads)
        self.spawn_handler_threads()?;

        tracing::info!("Runtime started successfully");
        Ok(())
    }

    /// Run the runtime until stopped
    ///
    /// This blocks the current task until `stop()` is called or
    /// the runtime is interrupted.
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.start().await?;
    /// runtime.run().await?;  // Blocks here
    /// ```
    pub async fn run(&mut self) -> Result<()> {
        if !self.running {
            return Err(anyhow!("Runtime not running (call start() first)"));
        }

        tracing::info!("Runtime running (press Ctrl+C to stop)");

        // Block until stopped
        // In real implementation, this would wait for shutdown signal
        // For now, just yield control
        tokio::signal::ctrl_c().await
            .map_err(|e| anyhow!("Failed to listen for shutdown signal: {}", e))?;

        tracing::info!("Shutdown signal received");
        self.stop().await?;

        Ok(())
    }

    /// Stop the runtime
    ///
    /// This cleanly shuts down:
    /// 1. Clock task (cancel tokio task)
    /// 2. Handler threads (drop broadcaster to close channels)
    /// 3. Wait for all threads to finish
    ///
    /// # Example
    ///
    /// ```ignore
    /// runtime.stop().await?;
    /// ```
    pub async fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }

        tracing::info!("Stopping runtime...");
        self.running = false;

        // 1. Cancel clock task
        if let Some(task) = self.clock_task.take() {
            task.abort();
            tracing::debug!("Clock task cancelled");
        }

        // 2. Drop broadcaster to close all channels
        // This makes handler threads' `for tick in rx` loops exit
        {
            let mut broadcaster = self.broadcaster.lock().unwrap();
            broadcaster.clear();
        }
        tracing::debug!("Broadcaster channels closed");

        // 3. Wait for handler threads to finish
        let thread_count = self.handler_threads.len();
        for (i, handle) in self.handler_threads.drain(..).enumerate() {
            match handle.join() {
                Ok(_) => tracing::debug!("Handler thread {}/{} joined", i+1, thread_count),
                Err(e) => tracing::error!("Handler thread {}/{} panicked: {:?}", i+1, thread_count, e),
            }
        }

        tracing::info!("Runtime stopped");
        Ok(())
    }

    /// Get runtime status info
    pub fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            running: self.running,
            fps: self.fps,
            handler_count: self.handler_threads.len() + self.pending_processors.len(),
            clock_type: if self.clock.is_some() { "pending" } else { "running" },
        }
    }

    // Private implementation methods

    fn spawn_clock_task(&mut self) -> Result<()> {
        let mut clock = self.clock.take()
            .ok_or_else(|| anyhow!("Clock already started"))?;

        let broadcaster = Arc::clone(&self.broadcaster);

        let task = tokio::spawn(async move {
            tracing::info!("[Clock] Started");

            loop {
                // Generate next tick (async wait)
                let tick = clock.next_tick().await;

                // Broadcast to all handlers (non-blocking)
                {
                    let broadcaster = broadcaster.lock().unwrap();
                    broadcaster.broadcast(tick);
                }

                // Note: No explicit sleep - clock.next_tick() handles timing
            }
        });

        self.clock_task = Some(task);
        Ok(())
    }

    fn spawn_handler_threads(&mut self) -> Result<()> {
        for (index, mut processor) in self.pending_processors.drain(..).enumerate() {
            // Subscribe processor to broadcaster
            let rx = {
                let mut broadcaster = self.broadcaster.lock().unwrap();
                broadcaster.subscribe()
            };

            // Spawn OS thread for this processor
            let handle = std::thread::spawn(move || {
                tracing::info!("[Processor {}] Thread started", index);

                // Call on_start lifecycle hook
                if let Err(e) = processor.on_start() {
                    tracing::error!("[Processor {}] on_start() failed: {}", index, e);
                    return;
                }

                // Process ticks until channel closes
                for tick in rx {
                    if let Err(e) = processor.process(tick) {
                        tracing::error!("[Processor {}] process() error: {}", index, e);
                        // Continue processing (errors are isolated)
                    }
                }

                // Call on_stop lifecycle hook
                if let Err(e) = processor.on_stop() {
                    tracing::error!("[Processor {}] on_stop() failed: {}", index, e);
                }

                tracing::info!("[Processor {}] Thread stopped", index);
            });

            self.handler_threads.push(handle);
        }

        Ok(())
    }
}

/// Runtime status information
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub running: bool,
    pub fps: f64,
    pub handler_count: usize,
    pub clock_type: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TimedTick;
    use crate::stream_processor::StreamProcessor;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct CounterProcessor {
        count: Arc<AtomicU64>,
    }

    impl CounterProcessor {
        fn new(count: Arc<AtomicU64>) -> Self {
            Self { count }
        }
    }

    impl StreamProcessor for CounterProcessor {
        fn process(&mut self, _tick: TimedTick) -> Result<()> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn test_runtime_creation() {
        let runtime = StreamRuntime::new(60.0);
        assert!(!runtime.running);
        assert_eq!(runtime.fps, 60.0);
        assert_eq!(runtime.pending_processors.len(), 0);
    }

    #[test]
    fn test_add_processor() {
        let mut runtime = StreamRuntime::new(60.0);

        let count = Arc::new(AtomicU64::new(0));
        let processor = CounterProcessor::new(count);

        runtime.add_processor(Box::new(processor));
        assert_eq!(runtime.pending_processors.len(), 1);
    }

    #[tokio::test]
    async fn test_runtime_lifecycle() {
        let mut runtime = StreamRuntime::new(100.0);

        let count1 = Arc::new(AtomicU64::new(0));
        let count2 = Arc::new(AtomicU64::new(0));

        runtime.add_processor(Box::new(CounterProcessor::new(Arc::clone(&count1))));
        runtime.add_processor(Box::new(CounterProcessor::new(Arc::clone(&count2))));

        runtime.start().await.unwrap();
        assert!(runtime.running);
        assert_eq!(runtime.handler_threads.len(), 2);

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        runtime.stop().await.unwrap();
        assert!(!runtime.running);

        let c1 = count1.load(Ordering::Relaxed);
        let c2 = count2.load(Ordering::Relaxed);

        assert!(c1 > 0);
        assert!(c2 > 0);
    }

    #[tokio::test]
    async fn test_true_parallelism() {
        use std::sync::Mutex as StdMutex;
        use std::time::Instant;

        struct WorkProcessor {
            work_duration_ms: u64,
            start_times: Arc<StdMutex<Vec<Instant>>>,
        }

        impl StreamProcessor for WorkProcessor {
            fn process(&mut self, tick: TimedTick) -> Result<()> {
                self.start_times.lock().unwrap().push(Instant::now());

                let start = Instant::now();
                let mut sum = 0u64;
                while start.elapsed().as_millis() < self.work_duration_ms as u128 {
                    sum = sum.wrapping_add(tick.frame_number);
                }

                if sum == u64::MAX {
                    println!("Never happens");
                }

                Ok(())
            }
        }

        let mut runtime = StreamRuntime::new(10.0);

        let start_times1 = Arc::new(StdMutex::new(Vec::new()));
        let start_times2 = Arc::new(StdMutex::new(Vec::new()));

        runtime.add_processor(Box::new(WorkProcessor {
            work_duration_ms: 50,
            start_times: Arc::clone(&start_times1),
        }));

        runtime.add_processor(Box::new(WorkProcessor {
            work_duration_ms: 50,
            start_times: Arc::clone(&start_times2),
        }));

        runtime.start().await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(350)).await;

        runtime.stop().await.unwrap();

        let times1 = start_times1.lock().unwrap();
        let times2 = start_times2.lock().unwrap();

        assert!(times1.len() >= 2);
        assert!(times2.len() >= 2);

        if let (Some(&t1), Some(&t2)) = (times1.first(), times2.first()) {
            let diff = if t1 > t2 {
                t1.duration_since(t2)
            } else {
                t2.duration_since(t1)
            };

            assert!(
                diff.as_millis() < 50,
                "Processors should start processing nearly simultaneously"
            );
        }
    }
}
