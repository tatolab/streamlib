//! Runtime extensions for macOS/iOS
//!
//! Provides macOS-specific runtime configuration that handles NSApplication
//! event loop on the main thread.

use streamlib_core::StreamRuntime;
use objc2::{MainThreadMarker, rc::Retained, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask};
use objc2_foundation::NSDate;
use std::time::Duration;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

/// Configure macOS event loop on a StreamRuntime
///
/// This is called automatically by `streamlib::StreamRuntime::new()` on macOS,
/// but can be called manually if needed.
///
/// Sets up the NSApplication event loop to run on the main thread while
/// the runtime processes video on worker threads.
pub fn configure_macos_event_loop(runtime: &mut StreamRuntime) {
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
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = streamlib_core::Result<()>> + Send>>
    }) as streamlib_core::runtime::EventLoopFn;

    runtime.set_event_loop(event_loop);
}
