use crate::core::StreamRuntime;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventType};
use objc2_foundation::NSPoint;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

pub fn configure_macos_event_loop(runtime: &mut StreamRuntime) {
    let running = Arc::new(AtomicBool::new(true));
    let running_loop = Arc::clone(&running);

    // Install Ctrl+C handler to stop event loop
    ctrlc::set_handler(move || {
        running_loop.store(false, Ordering::SeqCst);

        // Stop NSApplication on main thread
        use dispatch2::DispatchQueue;
        DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                unsafe {
                    let app = NSApplication::sharedApplication(mtm);
                    app.stop(None);

                    // Post a dummy event to wake up the event loop
                    // This is needed because stop() doesn't immediately exit the run loop
                    if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                        NSEventType::ApplicationDefined,
                        NSPoint::new(0.0, 0.0),
                        objc2_app_kit::NSEventModifierFlags::empty(),
                        0.0,
                        0,
                        None,
                        0,
                        0,
                        0,
                    ) {
                        app.postEvent_atStart(&event, true);
                    }
                }
            }
        });
    }).expect("Failed to set Ctrl+C handler");

    let event_loop = Box::new(move || {
        let mtm = MainThreadMarker::new().expect("Must be on main thread");
        let app = unsafe { NSApplication::sharedApplication(mtm) };

        unsafe {
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        }

        tracing::info!("macOS: Starting NSApplication event loop (blocking)");

        // This blocks until app.stop() is called
        unsafe {
            app.run();
        }

        tracing::info!("macOS: NSApplication event loop stopped");

        Ok(())
    }) as crate::core::runtime::EventLoopFn;

    runtime.set_event_loop(event_loop);
}
