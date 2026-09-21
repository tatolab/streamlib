// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One source fanned out to two `DisplayWindow` instances in one process on
//! Apple, asserted against the window server — the macOS counterpart of
//! `two_display_windows_live`.
//!
//! `harness = false`: the graph runs under `App::run`, which drives the
//! window event pump on the process's first thread, and libtest never runs a
//! test there. Display tier — it needs the window server and a GPU, so Cargo
//! builds it only under `hardware-tests`. `STREAMLIB_TWO_WINDOW_HARNESS_SECONDS`
//! holds both windows up long enough to photograph.
//!
//! Beyond two live windows it pins what a close means: closing one window
//! leaves the other showing and the graph running, and the rest close when the
//! run returns.

#[cfg(target_os = "macos")]
mod two_display_windows_harness;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    apple_window_server::run();
}

#[cfg(target_os = "macos")]
mod apple_window_server {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_core_foundation::{
        CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
    };
    use objc2_core_graphics::{
        CGSessionCopyCurrentDictionary, CGWindowListCopyWindowInfo, CGWindowListOption,
        kCGNullWindowID, kCGWindowName, kCGWindowOwnerPID,
    };
    use streamlib::sdk::App;
    use streamlib::sdk::runtime::{Runner, RuntimeStatus};
    use streamlib::sdk::runtime_control::request_runtime_shutdown;
    use streamlib::sdk::window_event_pump::process_wide_window_event_pump;

    use crate::two_display_windows_harness::{
        FIRST_WINDOW_TITLE, SECOND_WINDOW_TITLE, add_one_source_fanned_out_to_two_display_windows,
        harness_duration,
    };

    /// Cold swapchain creation is the slow step, and it varies.
    const WINDOWS_MAPPED_DEADLINE: Duration = Duration::from_secs(20);

    /// How long the window server's on-screen list may trail an order-out.
    const WINDOW_SERVER_ORDER_OUT_LAG: Duration = Duration::from_millis(100);

    /// How long the surviving window is watched after its neighbour closes.
    const SURVIVOR_OBSERVATION: Duration = Duration::from_secs(1);

    /// How many windows of this process the window server shows on screen
    /// under `window_title`.
    fn windows_on_screen_titled(window_title: &str) -> usize {
        let Some(on_screen_windows) = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
            kCGNullWindowID,
        ) else {
            return 0;
        };
        // SAFETY: the window server documents the list as an array of
        // CFDictionaries keyed by CFString.
        let on_screen_windows: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
            unsafe { CFRetained::cast_unchecked(on_screen_windows) };
        let this_process = i64::from(std::process::id());
        // SAFETY: reading two constant CFString keys the framework exports.
        let (owner_pid_key, window_name_key) = unsafe { (kCGWindowOwnerPID, kCGWindowName) };
        on_screen_windows
            .iter()
            .filter(|window| {
                let owner_pid = window
                    .get(owner_pid_key)
                    .and_then(|value| value.downcast::<CFNumber>().ok())
                    .and_then(|number| number.as_i64());
                let window_name = window
                    .get(window_name_key)
                    .and_then(|value| value.downcast::<CFString>().ok())
                    .map(|name| name.to_string());
                owner_pid == Some(this_process) && window_name.as_deref() == Some(window_title)
            })
            .count()
    }

    /// Set on `run`'s last line. AppKit can end the process from under the
    /// loop with status 0, which would read as a pass, so an exit before it
    /// fails instead.
    static RUN_REACHED_ITS_END: AtomicBool = AtomicBool::new(false);

    extern "C" fn fail_an_exit_that_did_not_come_from_the_end_of_run() {
        if !RUN_REACHED_ITS_END.load(Ordering::SeqCst) {
            const EXITED_BEFORE_THE_END_OF_RUN: &[u8] =
                b"the process exited before run() finished - AppKit ended it from under the loop\n";
            // SAFETY: two async-signal-safe calls on a static buffer.
            unsafe {
                libc::write(
                    libc::STDERR_FILENO,
                    EXITED_BEFORE_THE_END_OF_RUN.as_ptr().cast(),
                    EXITED_BEFORE_THE_END_OF_RUN.len(),
                );
                libc::_exit(1);
            }
        }
    }

    /// Requests the shutdown that ends `App::run` when dropped — on a panic
    /// too, so a watcher that fails cannot leave the run blocked forever.
    struct RequestTheShutdownThatEndsTheRunOnDrop;

    impl Drop for RequestTheShutdownThatEndsTheRunOnDrop {
        fn drop(&mut self) {
            let _ = request_runtime_shutdown("the window-server watcher is done");
        }
    }

    /// Whether the login session's screen is locked. The lock screen covers
    /// every window, so the window server's on-screen list stops describing
    /// what a user sees and this test cannot run.
    fn the_login_sessions_screen_is_locked() -> bool {
        let Some(login_session) = CGSessionCopyCurrentDictionary() else {
            return false;
        };
        // SAFETY: the session dictionary is keyed by CFString.
        let login_session: CFRetained<CFDictionary<CFString, CFType>> =
            unsafe { CFRetained::cast_unchecked(login_session) };
        login_session
            .get(&CFString::from_static_str("CGSSessionScreenIsLocked"))
            .and_then(|value| value.downcast::<CFBoolean>().ok())
            .is_some_and(|screen_is_locked| screen_is_locked.as_bool())
    }

    fn wait_until(deadline: Duration, condition: impl Fn() -> bool) -> bool {
        let give_up_at = Instant::now() + deadline;
        while Instant::now() < give_up_at {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    /// Close the window titled `window_title` as its close button would, on
    /// the first thread the pump is running on.
    fn close_the_window_as_the_user_would(window_title: &'static str) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            let Some(first_thread) = MainThreadMarker::new() else {
                return;
            };
            for window in NSApplication::sharedApplication(first_thread).windows() {
                if window.title().to_string() == window_title {
                    window.performClose(None);
                }
            }
        });
    }

    /// What the watcher saw, checked on the first thread once the run returns.
    fn watch_the_window_server_while_the_graph_runs(runner: &Runner) -> Result<(), String> {
        if !wait_until(WINDOWS_MAPPED_DEADLINE, || {
            windows_on_screen_titled(FIRST_WINDOW_TITLE) > 0
                && windows_on_screen_titled(SECOND_WINDOW_TITLE) > 0
        }) {
            return Err(format!(
                "both displays must own a live window at the same time; the window server \
                 showed {} titled '{FIRST_WINDOW_TITLE}' and {} titled '{SECOND_WINDOW_TITLE}'",
                windows_on_screen_titled(FIRST_WINDOW_TITLE),
                windows_on_screen_titled(SECOND_WINDOW_TITLE),
            ));
        }
        let (first_seen, second_seen) = (
            windows_on_screen_titled(FIRST_WINDOW_TITLE),
            windows_on_screen_titled(SECOND_WINDOW_TITLE),
        );
        if (first_seen, second_seen) != (1, 1) {
            return Err(format!(
                "one window per display, got {first_seen} and {second_seen}"
            ));
        }

        // Hold the graph up so a capture can be taken against live windows.
        std::thread::sleep(harness_duration());

        close_the_window_as_the_user_would(FIRST_WINDOW_TITLE);
        if !wait_until(Duration::from_secs(10), || {
            windows_on_screen_titled(FIRST_WINDOW_TITLE) == 0
        }) {
            return Err("closing the first window never took it off the screen".into());
        }
        std::thread::sleep(SURVIVOR_OBSERVATION);
        if windows_on_screen_titled(SECOND_WINDOW_TITLE) != 1 {
            return Err("closing one window took its neighbour down with it".into());
        }
        if runner.status() != RuntimeStatus::Started {
            return Err(format!(
                "closing one window must leave the graph running, but the runtime is {:?}",
                runner.status()
            ));
        }
        Ok(())
    }

    pub fn run() {
        // SAFETY: registers a plain `extern "C"` fn that captures nothing.
        unsafe { libc::atexit(fail_an_exit_that_did_not_come_from_the_end_of_run) };
        assert!(
            !the_login_sessions_screen_is_locked(),
            "cannot run: the screen is locked, so the window server cannot say what is on \
             screen — unlock the session and rerun"
        );
        let app = App::new().expect("runtime");
        add_one_source_fanned_out_to_two_display_windows(&app);

        let runner = Arc::clone(app.runner());
        let watcher = std::thread::Builder::new()
            .name("window-server-watcher".to_string())
            .spawn(move || {
                let _ends_the_run_however_the_watch_ends = RequestTheShutdownThatEndsTheRunOnDrop;
                watch_the_window_server_while_the_graph_runs(&runner)
            })
            .expect("spawn the watcher");

        let run_outcome = app.run();

        let watched = watcher
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        if let Err(what_the_window_server_showed) = watched {
            panic!("{what_the_window_server_showed}");
        }
        run_outcome.expect("the graph stops cleanly after one of its windows was closed");
        // Bounded well under AppKit's ~270 ms close animation, so a window
        // still animating out when `run` returned fails here; the window
        // server's own list trails an order-out by a few milliseconds.
        assert!(
            wait_until(WINDOW_SERVER_ORDER_OUT_LAG, || {
                windows_on_screen_titled(SECOND_WINDOW_TITLE) == 0
            }),
            "the run tore the graph down, so its remaining window must have left the screen \
             when it returned"
        );
        assert_eq!(
            process_wide_window_event_pump()
                .expect("the run built the pump on this thread")
                .registered_window_count(),
            0,
            "teardown hands every window back to the pump, and the run releases them before \
             it returns"
        );
        RUN_REACHED_ITS_END.store(true, Ordering::SeqCst);
    }
}
