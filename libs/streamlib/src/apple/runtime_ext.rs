use crate::core::StreamRuntime;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, NSProcessInfo};
use parking_lot::Mutex;
use std::sync::Arc;

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

pub fn configure_macos_event_loop(runtime: &mut StreamRuntime) {
    // Register shutdown callback that applicationWillTerminate can invoke
    let processors = Arc::clone(&runtime.processors);
    let shutdown_callback = Arc::new(move || {
        // Step 1: Publish shutdown event to EVENT_BUS (for Pull mode processors using shutdown_aware_loop)
        use crate::core::pubsub::{Event, RuntimeEvent, EVENT_BUS};
        let shutdown_event = Event::RuntimeGlobal(RuntimeEvent::RuntimeShutdown);
        EVENT_BUS.publish(&shutdown_event.topic(), &shutdown_event);
        tracing::info!("Shutdown callback: Published RuntimeShutdown event to EVENT_BUS");

        // Step 2: Send shutdown signals to all processor threads (for Push mode processors)
        {
            let procs = processors.lock();
            for (processor_id, proc_handle) in procs.iter() {
                // Send via shutdown channel
                if let Err(e) = proc_handle.shutdown_tx.send(()) {
                    tracing::warn!("[{}] Failed to send shutdown signal: {}", processor_id, e);
                }
                // Also send via wakeup channel for Push mode processors
                if let Err(e) = proc_handle.wakeup_tx.send(crate::core::runtime::WakeupEvent::Shutdown) {
                    tracing::warn!("[{}] Failed to send shutdown wakeup: {}", processor_id, e);
                }
            }
            tracing::info!("Shutdown callback: Sent shutdown signals to {} processors", procs.len());
        }

        // Step 3: Join all processor threads to wait for teardown (with timeout)
        const SHUTDOWN_TIMEOUT_SECS: u64 = 120; // 2 minutes as user suggested

        // Collect all processor IDs
        let processor_ids: Vec<String> = {
            let procs = processors.lock();
            procs.keys().cloned().collect()
        };

        // Join each thread with timeout
        for processor_id in processor_ids.iter() {
            let thread_handle = {
                let mut procs = processors.lock();
                procs.get_mut(processor_id).and_then(|proc| proc.thread.take())
            };

            if let Some(handle) = thread_handle {
                // Create a channel to signal when join completes
                let (join_tx, join_rx) = crossbeam_channel::bounded(1);

                std::thread::spawn(move || {
                    let result = handle.join();
                    let _ = join_tx.send(result);
                });

                // Wait for join with timeout
                match join_rx.recv_timeout(std::time::Duration::from_secs(SHUTDOWN_TIMEOUT_SECS)) {
                    Ok(Ok(())) => {
                        tracing::info!("[{}] Thread stopped successfully", processor_id);
                        let mut procs = processors.lock();
                        if let Some(proc) = procs.get_mut(processor_id) {
                            *proc.status.lock() = crate::core::runtime::ProcessorStatus::Stopped;
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!("[{}] Thread panicked during shutdown: {:?}", processor_id, e);
                    }
                    Err(_) => {
                        tracing::error!("[{}] Timeout waiting for thread to stop ({}s)", processor_id, SHUTDOWN_TIMEOUT_SECS);
                        // Thread will be forcefully terminated when process exits
                    }
                }
            }
        }

        tracing::info!("Shutdown callback: Completed processor shutdown");
    });

    *SHUTDOWN_CALLBACK.lock() = Some(shutdown_callback);

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
