
use crate::core::StreamRuntime;
use objc2::{MainThreadMarker, rc::Retained, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask};
use objc2_foundation::NSDate;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

extern "C" {
    fn CFRunLoopRunInMode(
        mode: core_foundation::string::CFStringRef,
        seconds: core_foundation::date::CFTimeInterval,
        returnAfterSourceHandled: bool,
    ) -> i32;
}

pub fn configure_macos_event_loop(runtime: &mut StreamRuntime) {
    let running = Arc::new(AtomicBool::new(true));
    let running_event_loop = Arc::clone(&running);

    let event_loop = Box::new(move || {
        Box::pin(async move {
            unsafe {
                let mtm = MainThreadMarker::new().expect("Must be on main thread");
                let app = NSApplication::sharedApplication(mtm);

                while running_event_loop.load(Ordering::Relaxed) {
                    use core_foundation::string::CFString;
                    use core_foundation::base::TCFType;

                    let mode = CFString::new("kCFRunLoopDefaultMode");
                    CFRunLoopRunInMode(mode.as_concrete_TypeRef(), 0.016, true);

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
