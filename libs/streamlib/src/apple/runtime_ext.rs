use crate::core::StreamRuntime;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, NSProcessInfo};

define_class!(
    /// Custom NSApplicationDelegate that handles graceful shutdown on Cmd+Q or system shutdown
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StreamlibAppDelegate"]
    struct StreamlibAppDelegate;

    impl StreamlibAppDelegate {
        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: *const NSObject) {
            tracing::info!("macOS: applicationWillTerminate called - beginning graceful shutdown");

            // Publish shutdown event to event bus to notify processors
            use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
            let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
            let topic = shutdown_event.topic();
            tracing::info!("macOS: Publishing RuntimeEvent::RuntimeShutdown to topic: {}", topic);
            EVENT_BUS.publish(&topic, &shutdown_event);
            tracing::info!("macOS: Published RuntimeEvent::RuntimeShutdown to event bus");

            // Give processors time to gracefully shut down
            // This is a best-effort attempt - if macOS forces termination, we'll exit anyway
            const GRACEFUL_SHUTDOWN_TIMEOUT_MS: u64 = 500;
            tracing::info!("macOS: Waiting up to {}ms for processors to stop gracefully", GRACEFUL_SHUTDOWN_TIMEOUT_MS);
            std::thread::sleep(std::time::Duration::from_millis(GRACEFUL_SHUTDOWN_TIMEOUT_MS));

            tracing::info!("macOS: applicationWillTerminate completed, app will terminate");
        }
    }

    unsafe impl NSObjectProtocol for StreamlibAppDelegate {}

    unsafe impl NSApplicationDelegate for StreamlibAppDelegate {}
);

pub fn configure_macos_event_loop(runtime: &mut StreamRuntime) {
    let event_loop = Box::new(move || {

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
        let delegate: Retained<StreamlibAppDelegate> = unsafe {
            msg_send![StreamlibAppDelegate::alloc(mtm), init]
        };
        let delegate_protocol: &ProtocolObject<dyn NSApplicationDelegate> = ProtocolObject::from_ref(&*delegate);
        app.setDelegate(Some(delegate_protocol));

        tracing::info!("macOS: NSApplicationDelegate installed");
        tracing::info!("macOS: Main menu with Quit item created");
        tracing::info!("macOS: Starting NSApplication event loop");

        // Use polling event loop instead of app.run() to allow signal handlers (ctrlc)
        // to execute between iterations. The ctrlc crate handles Ctrl+C, but needs
        // periodic breaks in the event loop to deliver the signal.
        use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
        use objc2_app_kit::NSEventMask;

        app.finishLaunching();

        loop {
            // Poll for events with a short timeout (0.1 seconds)
            let date = NSDate::dateWithTimeIntervalSinceNow(0.1);

            // Get next event (if any) - this is the polling approach
            let event = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&date),
                    &NSDefaultRunLoopMode,
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

        // Note: This code is unreachable - the app exits via NSApplication.terminate()
        // which calls applicationWillTerminate: and then exits the process
        #[allow(unreachable_code)]
        {
            tracing::info!("macOS: NSApplication event loop exited");
        }

        Ok(())
    }) as crate::core::runtime::EventLoopFn;

    runtime.set_event_loop(event_loop);
}
