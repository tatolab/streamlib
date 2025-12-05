// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSRunningApplication};
use objc2_foundation::{NSObject, NSObjectProtocol};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::pubsub::{Event, RuntimeEvent, PUBSUB};
use crate::core::Result;

/// Global shutdown callback that applicationWillTerminate can invoke
static SHUTDOWN_CALLBACK: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

define_class!(
    /// Custom NSApplicationDelegate that handles graceful shutdown on Cmd+Q or system shutdown
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StreamlibAppDelegate"]
    struct StreamlibAppDelegate;

    impl StreamlibAppDelegate {
        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: *const NSObject) {
            // NOTE: Don't log ANYTHING here - ALL I/O (stdout/stderr/stdin) is shutting down
            // Even eprintln!() will panic with SIGABRT during shutdown

            // Call the shutdown callback on a background thread to avoid deadlocks
            // (processor threads may dispatch to main thread, so we can't block main thread)
            if let Some(callback) = SHUTDOWN_CALLBACK.lock().as_ref() {
                let callback = Arc::clone(callback);

                let handle = std::thread::spawn(move || {
                    callback();
                });

                // Wait for shutdown to complete (blocking main thread is OK here since we're terminating)
                let _ = handle.join();
            }
        }
    }

    unsafe impl NSObjectProtocol for StreamlibAppDelegate {}

    unsafe impl NSApplicationDelegate for StreamlibAppDelegate {}
);

/// Install the macOS shutdown handler
///
/// This sets up the shutdown callback that applicationWillTerminate will invoke.
/// When called, it publishes a RuntimeShutdown event to the global PUBSUB,
/// which the executor listens to for graceful shutdown coordination.
pub fn install_macos_shutdown_handler() {
    let shutdown_callback = Arc::new(|| {
        // Publish shutdown event to PUBSUB
        // The executor subscribes to this and handles stopping all processors
        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        PUBSUB.publish(&shutdown_event.topic(), &shutdown_event);
        tracing::info!("macOS shutdown: Published RuntimeShutdown event to PUBSUB");

        // Give the executor time to handle shutdown
        // TODO: Better approach would be to wait for executor to signal completion
        std::thread::sleep(std::time::Duration::from_secs(2));

        tracing::info!("macOS shutdown: Completed");
    });

    *SHUTDOWN_CALLBACK.lock() = Some(shutdown_callback);
}

/// Set up NSApplication for standalone macOS apps.
///
/// Call this once before creating any windows. Idempotent - safe to call multiple times.
/// Sets activation policy, creates menu with Quit item, installs shutdown delegate.
pub fn setup_macos_app() {
    use objc2::sel;
    use objc2_app_kit::{NSApplicationActivationPolicy, NSMenu, NSMenuItem};
    use objc2_foundation::{NSProcessInfo, NSString};
    use std::sync::atomic::{AtomicBool, Ordering};

    static SETUP_DONE: AtomicBool = AtomicBool::new(false);
    if SETUP_DONE.swap(true, Ordering::SeqCst) {
        return; // Already set up
    }

    let mtm = MainThreadMarker::new().expect("Must be on main thread");
    let app = NSApplication::sharedApplication(mtm);

    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // Create main menu with Quit item (required for Cmd+Q to work)
    let menubar = NSMenu::new(mtm);
    let app_menu_item = NSMenuItem::new(mtm);
    menubar.addItem(&app_menu_item);
    app.setMainMenu(Some(&menubar));

    let app_menu = NSMenu::new(mtm);

    let process_info = NSProcessInfo::processInfo();
    let app_name = process_info.processName();
    let quit_title = NSString::from_str(&format!("Quit {}", app_name));

    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &quit_title,
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    app_menu.addItem(&quit_item);
    app_menu_item.setSubmenu(Some(&app_menu));

    // Set our delegate for shutdown handling
    let delegate: Retained<StreamlibAppDelegate> =
        unsafe { msg_send![StreamlibAppDelegate::alloc(mtm), init] };
    let delegate_protocol: &ProtocolObject<dyn NSApplicationDelegate> =
        ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(delegate_protocol));

    tracing::info!("macOS: App configured (activation policy, menu, delegate)");
}

/// Verify that the macOS platform is ready for processor initialization.
///
/// This function ensures the NSApplication has completed its launch sequence
/// by calling `finishLaunching()` and then verifying via Apple's APIs that
/// the application is actually in a ready state.
///
/// Must be called from the main thread after `setup_macos_app()`.
///
/// Returns `Ok(())` when verified ready, or `Err` if verification fails/times out.
pub fn ensure_macos_platform_ready() -> Result<()> {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    let mtm = MainThreadMarker::new().ok_or_else(|| {
        crate::core::StreamError::Runtime(
            "ensure_macos_platform_ready must be called from main thread".to_string(),
        )
    })?;

    let app = NSApplication::sharedApplication(mtm);

    // Call finishLaunching to complete the app initialization sequence
    app.finishLaunching();

    // Pump events briefly to allow the system to process the launch
    // This is necessary because finishLaunching() triggers async system work
    let pump_start = Instant::now();
    let pump_duration = Duration::from_millis(50);

    while pump_start.elapsed() < pump_duration {
        let date = NSDate::dateWithTimeIntervalSinceNow(0.01);
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&date),
                NSDefaultRunLoopMode,
                true,
            )
        };
        if let Some(event) = event {
            app.sendEvent(&event);
            app.updateWindows();
        }
    }

    // Now verify the platform is actually ready using Apple's APIs
    let timeout = Duration::from_secs(5);
    let start = Instant::now();

    loop {
        // Check NSRunningApplication.isFinishedLaunching - this is the authoritative
        // signal that applicationDidFinishLaunching has been processed
        let current_app = NSRunningApplication::currentApplication();
        let is_finished_launching = current_app.isFinishedLaunching();

        if is_finished_launching {
            tracing::info!(
                "macOS: Platform verified ready (isFinishedLaunching=true) in {:?}",
                start.elapsed()
            );
            return Ok(());
        }

        // Timeout check
        if start.elapsed() > timeout {
            return Err(crate::core::StreamError::Runtime(format!(
                "macOS platform readiness timeout after {:?}: isFinishedLaunching={}",
                timeout, is_finished_launching
            )));
        }

        // Pump more events while waiting - the system needs run loop time
        // to process the launch sequence
        let date = NSDate::dateWithTimeIntervalSinceNow(0.01);
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&date),
                NSDefaultRunLoopMode,
                true,
            )
        };
        if let Some(event) = event {
            app.sendEvent(&event);
            app.updateWindows();
        }
    }
}

/// Check if the macOS platform is currently ready (non-blocking).
///
/// Returns `true` if `NSRunningApplication.isFinishedLaunching` is true.
/// This can be called from any thread.
#[allow(dead_code)]
pub fn is_macos_platform_ready() -> bool {
    NSRunningApplication::currentApplication().isFinishedLaunching()
}

/// Run the NSApplication event loop (blocking).
///
/// Call `setup_macos_app()` and `ensure_macos_platform_ready()` first.
/// This blocks until the app terminates.
pub fn run_macos_event_loop() {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    let mtm = MainThreadMarker::new().expect("Must be on main thread");
    let app = NSApplication::sharedApplication(mtm);

    // Note: finishLaunching() should have already been called by ensure_macos_platform_ready()
    // but calling it again is safe (idempotent)

    tracing::info!("macOS: Event loop starting");

    loop {
        let date = NSDate::dateWithTimeIntervalSinceNow(0.1);

        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&date),
                NSDefaultRunLoopMode,
                true,
            )
        };

        if let Some(event) = event {
            app.sendEvent(&event);
            app.updateWindows();
        }
    }
}
