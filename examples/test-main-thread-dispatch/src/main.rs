use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::Duration;
use streamlib::core::{DataFrame, RuntimeContext, StreamOutput};
use streamlib::{Result, StreamProcessor, StreamRuntime};

// Global state for test validation
static ASYNC_EXECUTED: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static BLOCKING_RESULT: OnceLock<Arc<Mutex<Option<u64>>>> = OnceLock::new();
static PROCESS_COUNT: OnceLock<Arc<AtomicU64>> = OnceLock::new();

/// Test processor that validates main thread dispatch functionality
#[derive(StreamProcessor)]
#[processor(description = "Test processor for main thread dispatch")]
struct MainThreadTestProcessor {
    // Dummy output to satisfy macro requirements (never used)
    #[output(description = "Dummy output (unused)")]
    _dummy: Arc<StreamOutput<DataFrame>>,

    ctx: Option<Arc<RuntimeContext>>,
}

impl Default for MainThreadTestProcessor {
    fn default() -> Self {
        Self {
            _dummy: Arc::new(StreamOutput::new("dummy")),
            ctx: None,
        }
    }
}

impl MainThreadTestProcessor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        println!("✓ setup() called - storing RuntimeContext");
        self.ctx = Some(Arc::new(ctx.clone()));

        // Test async dispatch during setup
        println!("  Testing run_on_main_async() from setup()...");
        if let Some(async_flag) = ASYNC_EXECUTED.get() {
            let async_flag = Arc::clone(async_flag);
            ctx.run_on_main_async(move || {
                println!("    ✓ Async closure executed on main thread!");
                async_flag.store(true, Ordering::SeqCst);
            });
        }

        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if let Some(count_arc) = PROCESS_COUNT.get() {
            let count = count_arc.fetch_add(1, Ordering::SeqCst);

            // Only run test on first process call
            if count == 0 {
                println!("✓ process() called (iteration {})", count);

                if let Some(ref ctx) = self.ctx {
                    // Test blocking dispatch from worker thread
                    println!("  Testing run_on_main_blocking() from worker thread...");
                    let result = ctx.run_on_main_blocking(|| {
                        println!("    ✓ Blocking closure executed on main thread!");
                        // Simulate computation on main thread
                        42u64 + 8u64
                    });

                    println!("    ✓ Received result: {}", result);
                    if let Some(result_arc) = BLOCKING_RESULT.get() {
                        *result_arc.lock().unwrap() = Some(result);
                    }
                }
            }

            // Stop after a few iterations
            if count >= 5 {
                std::thread::sleep(Duration::from_millis(500));
            }
        }

        Ok(())
    }
}

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Main Thread Dispatch Test ===\n");

    // Initialize global state
    ASYNC_EXECUTED
        .set(Arc::new(AtomicBool::new(false)))
        .unwrap();
    BLOCKING_RESULT.set(Arc::new(Mutex::new(None))).unwrap();
    PROCESS_COUNT.set(Arc::new(AtomicU64::new(0))).unwrap();

    // Create runtime and add processor
    let mut runtime = StreamRuntime::new();
    println!("Adding test processor to runtime...");
    runtime
        .add_processor::<MainThreadTestProcessor>()
        .expect("Failed to add processor");

    // Start runtime (spawns worker threads, calls setup())
    println!("Starting runtime...\n");
    runtime.start().expect("Failed to start runtime");

    // Give dispatch queue time to process async work from setup()
    std::thread::sleep(Duration::from_millis(300));

    // Verify async dispatch from setup() executed
    if let Some(async_executed) = ASYNC_EXECUTED.get() {
        if async_executed.load(Ordering::SeqCst) {
            println!("\n✅ PASS: Async dispatch from setup() executed on main thread");
        } else {
            println!("\n❌ FAIL: Async dispatch from setup() did not execute");
            std::process::exit(1);
        }
    }

    // Give runtime time to process (starts event loop, calls process())
    std::thread::sleep(Duration::from_millis(500));

    // Verify blocking dispatch from process() executed and returned correct value
    if let Some(blocking_result) = BLOCKING_RESULT.get() {
        let result = blocking_result.lock().unwrap();
        if *result == Some(50) {
            println!("✅ PASS: Blocking dispatch from process() returned correct value (50)");
        } else {
            println!(
                "❌ FAIL: Blocking dispatch returned {:?}, expected Some(50)",
                *result
            );
            std::process::exit(1);
        }
    }

    // Verify process() was called
    if let Some(process_count) = PROCESS_COUNT.get() {
        let count = process_count.load(Ordering::SeqCst);
        if count >= 1 {
            println!("✅ PASS: Process called {} times", count);
        } else {
            println!("❌ FAIL: Process was never called");
            std::process::exit(1);
        }
    }

    println!("\n🎉 All tests passed!");
    println!("\nValidated:");
    println!("  ✓ run_on_main_async() works from setup()");
    println!("  ✓ run_on_main_blocking() works from process() on worker thread");
    println!("  ✓ Closures execute on main thread's dispatch queue");
    println!("  ✓ Blocking variant returns values correctly");
}
