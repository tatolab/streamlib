//! Runtime extensions for macOS/iOS
//!
//! On macOS, StreamRuntime automatically configures itself to handle
//! NSApplication events. Just import this module and use StreamRuntime normally.

use streamlib_core::StreamRuntime;
use objc2::{MainThreadMarker, rc::Retained, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask};
use objc2_foundation::NSDate;
use std::time::Duration;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

/// Create a macOS-configured StreamRuntime
///
/// This is a drop-in replacement for `StreamRuntime::new()` that automatically
/// configures the NSApplication event loop for you.
///
/// # Example
///
/// ```ignore
/// use streamlib_apple::runtime_ext::new_runtime;
///
/// let mut runtime = new_runtime(60.0);
/// runtime.add_processor(Box::new(renderer));
/// runtime.add_processor(Box::new(display));
/// runtime.connect(renderer.output_mut(), display.input_mut())?;
///
/// // Just run - event loop is auto-configured!
/// runtime.run().await?;
/// ```
pub fn new_runtime(fps: f64) -> StreamRuntime {
    let mut runtime = StreamRuntime::new(fps);
    configure_macos_event_loop(&mut runtime);
    runtime
}

/// Configure macOS event loop on an existing runtime
///
/// This is called automatically by `new_runtime()`, but you can call it manually
/// if you create the runtime using `StreamRuntime::new()` directly.
fn configure_macos_event_loop(runtime: &mut StreamRuntime) {
    let running = Arc::new(AtomicBool::new(true));
    let running_event_loop = Arc::clone(&running);

    // Create event loop closure
    let event_loop = Box::new(move || {
        Box::pin(async move {
            // Process events on main thread
            unsafe {
                let mtm = MainThreadMarker::new().expect("Must be on main thread");
                let app = NSApplication::sharedApplication(mtm);

                // Main event loop
                while running_event_loop.load(Ordering::Relaxed) {
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

                    // Sleep briefly (~60 FPS event processing)
                    std::thread::sleep(Duration::from_millis(16));
                }
            }

            Ok(())
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
    }) as streamlib_core::runtime::EventLoopFn;

    runtime.set_event_loop(event_loop);
}
