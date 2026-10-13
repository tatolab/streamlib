// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! On Apple, a run entered on the process's first thread builds the window
//! event pump there, so the first window asked for from any other thread — a
//! stream loaded through the local API — is minted rather than refused.
//!
//! `harness = false`: libtest never runs a test on the first thread, so `main`
//! is this file's own. Display tier — it needs the window server and a GPU, so
//! Cargo builds it only under `hardware-tests`. Empty off Apple, where the pump
//! runs on a thread of its own.
//!
//! Mental revert: drop the pump's build from
//! `run_owning_the_machine_shutdown_signals` and the worker is the first to
//! ask, so the pump settles as refused and the window is never minted.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    apple_first_thread::run();
}

#[cfg(target_os = "macos")]
mod apple_first_thread {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use streamlib::sdk::runtime::Runner;
    use streamlib_engine::core::window_event_pump::{
        WindowRegistrationRequestFromOwningProcessor, process_wide_window_event_pump,
    };

    /// Set on `run`'s last line. AppKit can end the process from under the
    /// loop with status 0, which would read as a pass, so an exit before it
    /// fails instead.
    static RUN_REACHED_ITS_END: AtomicBool = AtomicBool::new(false);

    extern "C" fn fail_an_exit_that_did_not_come_from_the_end_of_run() {
        if !RUN_REACHED_ITS_END.load(Ordering::SeqCst) {
            const EXITED_BEFORE_THE_END_OF_RUN: &[u8] =
                b"the process exited before run() reached its end\n";
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

    pub fn run() {
        // SAFETY: registers a plain `extern "C"` fn that captures nothing.
        unsafe { libc::atexit(fail_an_exit_that_did_not_come_from_the_end_of_run) };

        let runner = Runner::new().expect("Runner::new");
        let runner_for_the_local_api_thread = Arc::clone(&runner);
        let mut window_asked_for_off_the_first_thread = None;

        runner
            .run_owning_the_machine_shutdown_signals(|| {
                let local_api_thread = std::thread::Builder::new()
                    .name("local-api-load".to_string())
                    .spawn(move || {
                        let window_minted = process_wide_window_event_pump()
                            .and_then(|event_pump| {
                                event_pump.request_window_for_owning_processor(
                                    WindowRegistrationRequestFromOwningProcessor {
                                        window_title:
                                            "streamlib window event pump built when the run starts"
                                                .to_string(),
                                        initial_width_in_logical_pixels: 320,
                                        initial_height_in_logical_pixels: 240,
                                    },
                                )
                            })
                            .map(drop)
                            .map_err(|refusal| refusal.to_string());
                        runner_for_the_local_api_thread
                            .request_the_shutdown_of_every_loaded_stream(
                                "the window was asked for off the first thread",
                            )
                            .expect("the machine's shutdown is requested");
                        window_minted
                    })
                    .expect("spawn the local-API thread");
                runner.wait_until_a_machine_shutdown_is_requested();
                window_asked_for_off_the_first_thread = Some(
                    local_api_thread
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                );
                Ok(())
            })
            .expect("the run owns the machine's shutdown signals");
        runner.shut_down().expect("the engine shuts down");

        if let Err(refusal) =
            window_asked_for_off_the_first_thread.expect("the local-API thread reported its window")
        {
            panic!(
                "a window first asked for off the first thread, inside a run entered on it, \
                 was refused: {refusal}"
            );
        }
        RUN_REACHED_ITS_END.store(true, Ordering::SeqCst);
    }
}
