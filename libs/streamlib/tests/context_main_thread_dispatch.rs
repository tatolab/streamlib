//! Integration test for RuntimeContext main thread dispatch utilities
//!
//! This test creates a minimal processor that uses the RuntimeContext
//! to dispatch work to the main thread, validating that the mechanism
//! works in a real runtime environment.
//!
//! NOTE: This test is temporarily disabled because it uses the deprecated
//! `Processor` trait API. It will be re-enabled when the Phase 1 unified
//! API is in place.
//!
//! TODO: Rewrite using #[derive(StreamProcessor)] macro once Phase 1 is complete.

// Tests are ignored - the code below uses deprecated APIs and won't compile.
// Kept as documentation of desired test behavior.

#[test]
#[ignore = "Uses deprecated Processor trait API - needs rewrite for Phase 1 macro-based processors"]
#[cfg(target_os = "macos")]
fn test_context_main_thread_dispatch_integration() {
    // This test should verify:
    // 1. A processor can use RuntimeContext::run_on_main_async() during setup()
    // 2. A processor can use RuntimeContext::run_on_main_blocking() during process()
    // 3. Both async and blocking dispatch work correctly in a real runtime
}

#[test]
#[ignore = "Uses deprecated Processor trait API - needs rewrite for Phase 1 macro-based processors"]
#[cfg(target_os = "macos")]
fn test_multiple_processors_can_use_main_thread_dispatch() {
    // This test should verify:
    // 1. Multiple processors can independently use main thread dispatch
    // 2. Async and blocking dispatch work correctly with concurrent processors
}
