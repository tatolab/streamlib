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

/// Run the NSApplication event loop (blocking).
///
/// Call `setup_macos_app()` first. This blocks until the app terminates.
pub fn run_macos_event_loop() {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    let mtm = MainThreadMarker::new().expect("Must be on main thread");
    let app = NSApplication::sharedApplication(mtm);

    app.finishLaunching();

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
