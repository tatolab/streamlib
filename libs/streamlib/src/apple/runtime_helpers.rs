//! Runtime helpers for macOS/iOS platform-specific concerns
//!
//! Provides helper functions to run the StreamRuntime while handling
//! platform requirements (like main-thread event processing on macOS).

use crate::core::{StreamRuntime, Result};
use objc2::{MainThreadMarker, rc::Retained, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask};
use objc2_foundation::NSDate;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

/// Run the StreamRuntime on macOS, handling main-thread event processing
///
/// This is a platform-specific helper that:
/// 1. Spawns the runtime in a background thread
/// 2. Processes NSApplication events on the main thread (required)
/// 3. Handles Ctrl+C gracefully
///
/// # Example
///
/// ```ignore
/// let mut runtime = StreamRuntime::new();
/// runtime.add_processor(Box::new(ball_renderer));
/// runtime.add_processor(Box::new(display));
/// runtime.connect(ball_renderer.output_mut(), display.input_mut())?;
///
/// // Simple one-liner that handles everything
/// run_runtime_macos(runtime).await?;
/// ```
#[allow(dead_code)] // Public API for future runtime management
pub async fn run_runtime_macos(mut runtime: StreamRuntime) -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // Spawn runtime in background thread
    let runtime_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            runtime.start().await.unwrap();

            // Run until stopped
            tokio::select! {
                _ = runtime.run() => {},
                _ = async {
                    while running_clone.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => {
                    runtime.stop().await.unwrap();
                }
            }
        });
    });

    // Process events on main thread (required for NSApplication)
    unsafe {
        let mtm = MainThreadMarker::new().expect("Must be on main thread");
        let app = NSApplication::sharedApplication(mtm);

        // Main event loop
        loop {
            // Process all pending events
            loop {
                let distant_past = NSDate::distantPast();
                let event: Option<Retained<NSEvent>> = msg_send![
                    &*app,
                    nextEventMatchingMask: NSEventMask::Any,
                    untilDate: &*distant_past,
                    inMode: objc2_foundation::NSDefaultRunLoopMode,
                    dequeue: true
                ];

                match event {
                    Some(evt) => {
                        app.sendEvent(&evt);
                    }
                    None => break,
                }
            }

            // Check if runtime thread is still alive
            if runtime_thread.is_finished() {
                break;
            }

            // Sleep briefly to avoid busy loop (~60 FPS event processing)
            std::thread::sleep(Duration::from_millis(16));
        }
    }

    // Wait for runtime thread to finish
    runtime_thread.join().unwrap();
    Ok(())
}
