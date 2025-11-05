
use crate::core::{StreamRuntime, Result};
use objc2::{MainThreadMarker, rc::Retained, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask};
use objc2_foundation::NSDate;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

#[allow(dead_code)] // Public API for future runtime management
pub async fn run_runtime_macos(mut runtime: StreamRuntime) -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    let runtime_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            runtime.start().await.unwrap();

            tokio::select! {
                _ = runtime.run() => {},
                _ = async {
                    while running_clone.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => {
                    runtime.stop().await.unwrap();
                }
            }
        });
    });

    unsafe {
        let mtm = MainThreadMarker::new().expect("Must be on main thread");
        let app = NSApplication::sharedApplication(mtm);

        loop {
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

            if runtime_thread.is_finished() {
                break;
            }

            std::thread::sleep(Duration::from_millis(16));
        }
    }

    runtime_thread.join().unwrap();
    Ok(())
}
