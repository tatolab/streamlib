//! macOS runtime extensions
//!
//! This module provides macOS-specific integration for graceful shutdown handling.
//! The NSApplicationDelegate receives Cmd+Q and system shutdown events and publishes
//! them to the global EVENT_BUS for the executor to handle.
//!
//! NOTE: This module uses the global pub/sub system (EVENT_BUS) which is the standard
//! way to communicate events in streamlib. It does NOT access processor instances,
//! executor internals, or runtime state directly.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{NSObject, NSObjectProtocol};
use parking_lot::Mutex;
use std::sync::Arc;

use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};

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
/// When called, it publishes a RuntimeShutdown event to the global EVENT_BUS,
/// which the executor listens to for graceful shutdown coordination.
pub fn install_macos_shutdown_handler() {
    let shutdown_callback = Arc::new(|| {
        // Publish shutdown event to EVENT_BUS
        // The executor subscribes to this and handles stopping all processors
        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        EVENT_BUS.publish(&shutdown_event.topic(), &shutdown_event);
        tracing::info!("macOS shutdown: Published RuntimeShutdown event to EVENT_BUS");

        // Give the executor time to handle shutdown
        // TODO: Better approach would be to wait for executor to signal completion
        std::thread::sleep(std::time::Duration::from_secs(2));

        tracing::info!("macOS shutdown: Completed");
    });

    *SHUTDOWN_CALLBACK.lock() = Some(shutdown_callback);
}

/// Configure and run the macOS NSApplication event loop (blocking)
///
/// This must be called from the main thread. It sets up the NSApplication with:
/// - A main menu with Quit item (Cmd+Q)
/// - The StreamlibAppDelegate for shutdown handling
/// - A polling event loop that allows signal handlers to run
///
/// This function blocks until the application terminates.
pub fn run_macos_event_loop() {
    use objc2::sel;
    use objc2_app_kit::{
        NSApplicationActivationPolicy, NSEventMask, NSMenu, NSMenuItem,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSProcessInfo, NSString};

    let mtm = MainThreadMarker::new().expect("Must be on main thread");
    let app = NSApplication::sharedApplication(mtm);

    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // Create main menu with Quit item (required for Cmd+Q to work)
    let menubar = NSMenu::new(mtm);
    let app_menu_item = NSMenuItem::new(mtm);
    menubar.addItem(&app_menu_item);
    app.setMainMenu(Some(&menubar));

    let app_menu = NSMenu::new(mtm);

    // Get app name from process info
    let process_info = NSProcessInfo::processInfo();
    let app_name = process_info.processName();
    let quit_title = NSString::from_str(&format!("Quit {}", app_name));

    // Create Quit menu item with Cmd+Q shortcut
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

    // Create and set our custom delegate
    let delegate: Retained<StreamlibAppDelegate> =
        unsafe { msg_send![StreamlibAppDelegate::alloc(mtm), init] };
    let delegate_protocol: &ProtocolObject<dyn NSApplicationDelegate> =
        ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(delegate_protocol));

    tracing::info!("macOS: NSApplicationDelegate installed");
    tracing::info!("macOS: Main menu with Quit item created");
    tracing::info!("macOS: Starting NSApplication event loop");

    // Use polling event loop instead of app.run() to allow signal handlers (ctrlc)
    // to execute between iterations. The ctrlc crate handles Ctrl+C, but needs
    // periodic breaks in the event loop to deliver the signal.
    app.finishLaunching();

    loop {
        // Poll for events with a short timeout (0.1 seconds)
        let date = NSDate::dateWithTimeIntervalSinceNow(0.1);

        // Get next event (if any) - this is the polling approach
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&date),
                NSDefaultRunLoopMode,
                true,
            )
        };

        if let Some(event) = event {
            // Dispatch the event
            app.sendEvent(&event);
            // Update windows
            app.updateWindows();
        }
    }
}

/// Legacy compatibility shim - configures macOS event loop integration
///
/// DEPRECATED: This function is maintained for backward compatibility.
/// Use `install_macos_shutdown_handler()` followed by `run_macos_event_loop()` instead,
/// or let the executor handle the event loop via `runtime.run()`.
#[deprecated(
    since = "0.2.0",
    note = "Use install_macos_shutdown_handler() and run_macos_event_loop() separately"
)]
pub fn configure_macos_event_loop(_runtime: &mut crate::StreamRuntime) {
    install_macos_shutdown_handler();

    tracing::warn!(
        "configure_macos_event_loop is deprecated. \
         The executor now handles the event loop via runtime.run(). \
         For macOS GUI apps, call run_macos_event_loop() explicitly on the main thread."
    );
}
