//! Runtime extensions for macOS/iOS
//!
//! Provides macOS-specific runtime configuration that handles NSApplication
//! event loop on the main thread.

use crate::core::StreamRuntime;
use objc2::{MainThreadMarker, rc::Retained, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask};
use objc2_foundation::NSDate;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

// Extern C function for pumping the CFRunLoop (includes GCD queue processing)
extern "C" {
    fn CFRunLoopRunInMode(
        mode: core_foundation::string::CFStringRef,
        seconds: core_foundation::date::CFTimeInterval,
        returnAfterSourceHandled: bool,
    ) -> i32;
}

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

                // Main event loop using CFRunLoop
                // This processes BOTH NSApplication events AND GCD dispatch queue tasks!
                while running_event_loop.load(Ordering::Relaxed) {
                    // Run the runloop for a short time (16ms ≈ 60 FPS)
                    // This will process:
                    // 1. NSApplication events
                    // 2. GCD main queue tasks (including our async window creation!)
                    // 3. Timer sources
                    // 4. Other runloop sources
                    use core_foundation::string::CFString;
                    use core_foundation::base::TCFType;

                    let mode = CFString::new("kCFRunLoopDefaultMode");
                    CFRunLoopRunInMode(mode.as_concrete_TypeRef(), 0.016, true);

                    // Process any pending NSApplication events that weren't handled by the runloop
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
                }
            }

            Ok(())
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = crate::core::Result<()>> + Send>>
    }) as crate::core::runtime::EventLoopFn;

    runtime.set_event_loop(event_loop);
}
