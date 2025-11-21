//! Integration test for RuntimeContext main thread dispatch utilities
//!
//! This test creates a minimal processor that uses the RuntimeContext
//! to dispatch work to the main thread, validating that the mechanism
//! works in a real runtime environment.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use streamlib::core::{
    bus::{InputPort, OutputPort},
    traits::{Processor, StreamElement},
    AudioFrame, ProcessMode, RuntimeContext,
};
use streamlib::{Result, StreamRuntime};

/// Test processor that dispatches work to main thread during processing
struct MainThreadTestProcessor {
    ctx: Option<RuntimeContext>,
    async_executed: Arc<AtomicBool>,
    blocking_result: Arc<Mutex<Option<u64>>>,
    process_count: Arc<AtomicU64>,
}

impl MainThreadTestProcessor {
    fn new(
        async_executed: Arc<AtomicBool>,
        blocking_result: Arc<Mutex<Option<u64>>>,
        process_count: Arc<AtomicU64>,
    ) -> Self {
        Self {
            ctx: None,
            async_executed,
            blocking_result,
            process_count,
        }
    }
}

impl Processor for MainThreadTestProcessor {
    fn name(&self) -> &str {
        "MainThreadTestProcessor"
    }

    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Store context for use in process()
        self.ctx = Some(ctx.clone());

        // Test async dispatch during setup
        let async_flag = Arc::clone(&self.async_executed);
        ctx.run_on_main_async(move || {
            async_flag.store(true, Ordering::SeqCst);
        });

        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        let count = self.process_count.fetch_add(1, Ordering::SeqCst);

        // Only run test on first process call
        if count == 0 {
            if let Some(ref ctx) = self.ctx {
                // Test blocking dispatch from worker thread
                let result = ctx.run_on_main_blocking(|| {
                    // Simulate computation on main thread
                    42u64 + 8u64
                });

                *self.blocking_result.lock().unwrap() = Some(result);
            }
        }

        // Stop after a few iterations
        if count >= 3 {
            std::thread::sleep(Duration::from_millis(10));
        }

        Ok(())
    }

    fn mode(&self) -> ProcessMode {
        ProcessMode::Pull
    }

    fn inputs(&self) -> Vec<InputPort> {
        vec![]
    }

    fn outputs(&self) -> Vec<OutputPort> {
        vec![]
    }
}

impl StreamElement for MainThreadTestProcessor {}

#[test]
#[cfg(target_os = "macos")]
fn test_context_main_thread_dispatch_integration() {
    // Shared state to verify dispatch worked
    let async_executed = Arc::new(AtomicBool::new(false));
    let blocking_result = Arc::new(Mutex::new(None));
    let process_count = Arc::new(AtomicU64::new(0));

    let processor = MainThreadTestProcessor::new(
        Arc::clone(&async_executed),
        Arc::clone(&blocking_result),
        Arc::clone(&process_count),
    );

    // Create runtime and add processor
    let mut runtime = StreamRuntime::new();
    runtime
        .add_processor(processor)
        .expect("Failed to add processor");

    // Start runtime (spawns worker threads, calls setup())
    runtime.start().expect("Failed to start runtime");

    // Give dispatch queue time to process async work from setup()
    std::thread::sleep(Duration::from_millis(200));

    // Verify async dispatch from setup() executed
    assert!(
        async_executed.load(Ordering::SeqCst),
        "Async dispatch from setup() should have executed on main thread"
    );

    // Run runtime for a short time (starts event loop on main thread)
    // This allows process() to be called, which tests blocking dispatch
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        // Runtime will stop automatically when test ends
    });

    // Give runtime time to process
    std::thread::sleep(Duration::from_millis(400));

    // Verify blocking dispatch from process() executed and returned correct value
    let result = blocking_result.lock().unwrap();
    assert_eq!(
        *result,
        Some(50),
        "Blocking dispatch from process() should return 42 + 8 = 50"
    );

    // Verify process() was called multiple times
    assert!(
        process_count.load(Ordering::SeqCst) >= 1,
        "Process should have been called at least once"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn test_multiple_processors_can_use_main_thread_dispatch() {
    // Test that multiple processors can independently use main thread dispatch
    let counter1 = Arc::new(AtomicU64::new(0));
    let counter2 = Arc::new(AtomicU64::new(0));

    let async_flag1 = Arc::new(AtomicBool::new(false));
    let async_flag2 = Arc::new(AtomicBool::new(false));

    let blocking_result1 = Arc::new(Mutex::new(None));
    let blocking_result2 = Arc::new(Mutex::new(None));

    let processor1 = MainThreadTestProcessor::new(
        Arc::clone(&async_flag1),
        Arc::clone(&blocking_result1),
        Arc::clone(&counter1),
    );

    let processor2 = MainThreadTestProcessor::new(
        Arc::clone(&async_flag2),
        Arc::clone(&blocking_result2),
        Arc::clone(&counter2),
    );

    let mut runtime = StreamRuntime::new();
    runtime
        .add_processor(processor1)
        .expect("Failed to add processor 1");
    runtime
        .add_processor(processor2)
        .expect("Failed to add processor 2");

    runtime.start().expect("Failed to start runtime");

    std::thread::sleep(Duration::from_millis(500));

    // Both processors should have executed async dispatch
    assert!(
        async_flag1.load(Ordering::SeqCst),
        "Processor 1 async should execute"
    );
    assert!(
        async_flag2.load(Ordering::SeqCst),
        "Processor 2 async should execute"
    );

    // Both should have computed blocking results
    assert_eq!(*blocking_result1.lock().unwrap(), Some(50));
    assert_eq!(*blocking_result2.lock().unwrap(), Some(50));
}
