// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A processor-owned window on Apple, minted and presented from a worker
//! thread while the process's first thread drives the window event pump.
//!
//! `harness = false`: AppKit lets only the first thread drive the loop, and
//! libtest never runs a test there, so `main` is this file's own. Display tier —
//! it needs the window server and a GPU, so Cargo builds it only under
//! `hardware-tests`. On Linux the pump drives itself and the same contract is
//! pinned by `processor_owned_window_shows_named_surfaces`.

#[cfg(target_os = "macos")]
mod processor_owned_window_named_surface_contract;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    apple_first_thread::run();
}

#[cfg(target_os = "macos")]
mod apple_first_thread {
    use std::ops::ControlFlow;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use streamlib_engine::core::context::{GpuContext, GpuContextLimitedAccess};
    use streamlib_engine::core::processor_owned_window::{
        ProcessorOwnedWindow, ProcessorOwnedWindowAwaitingItsPresentTarget,
    };
    use streamlib_engine::core::runtime::{
        is_runtime_shutdown_requested, take_runtime_shutdown_escalation,
    };
    use streamlib_engine::core::window_event_pump::{
        WindowEventPumpDriveOnTheFirstThreadOutcome,
        drive_the_window_event_pump_on_the_first_thread_until, process_wide_window_event_pump,
        release_the_windows_handed_back_while_the_event_pump_was_not_driven,
    };
    use streamlib_engine::logging::{LoggingTunables, StreamlibLoggingConfig};

    use crate::processor_owned_window_named_surface_contract::{
        mint_and_hold_a_window_to_the_named_surface_contract, request_for,
    };

    /// Past `DisplayWindow`'s own two-second stop budget: a step still blocked
    /// here is one a display's teardown would have detached and leaked.
    const TEARDOWN_TIME_STEP_BUDGET: Duration = Duration::from_secs(3);

    /// What the window owner does once the first thread has stopped driving
    /// the loop, and how long each step took.
    struct StepsTakenWhileTheLoopWasNotDriven {
        present_target_opened_after: Duration,
        windows_released_after: Duration,
    }

    pub fn run() {
        let _logging = streamlib_engine::logging::init(StreamlibLoggingConfig {
            service_name: "processor-owned-window-on-the-first-thread".to_string(),
            runtime_id: None,
            stdout: true,
            jsonl: false,
            intercept_stdio: false,
            tunables: LoggingTunables::default(),
        })
        .expect("logging");

        let event_pump = process_wide_window_event_pump()
            .expect("the pump builds on the process's first thread");

        let work_while_driven_is_done = Arc::new(AtomicBool::new(false));
        let (the_loop_is_no_longer_driven, wait_until_the_loop_is_no_longer_driven) =
            channel::<()>();
        let (report_the_undriven_steps, undriven_steps_reported) =
            channel::<StepsTakenWhileTheLoopWasNotDriven>();
        let window_owner = {
            let work_while_driven_is_done = Arc::clone(&work_while_driven_is_done);
            std::thread::Builder::new()
                .name("window-owner".to_string())
                .spawn(move || {
                    let gpu_context =
                        GpuContext::init_for_platform().expect("a GPU is required for this tier");
                    let gpu_context_limited_access = gpu_context.limited_access();
                    let (presented_window, source_texture, window_to_open_later) =
                        mint_and_present_while_the_loop_is_driven(&gpu_context_limited_access);
                    quit_from_the_application_menu_while_the_loop_is_driven();
                    work_while_driven_is_done.store(true, Ordering::Release);

                    wait_until_the_loop_is_no_longer_driven
                        .recv()
                        .expect("the first thread says when the loop has stopped");
                    let undriven_steps = open_and_release_while_the_loop_is_not_driven(
                        &gpu_context_limited_access,
                        presented_window,
                        window_to_open_later,
                    );
                    drop(source_texture);
                    let _ = report_the_undriven_steps.send(undriven_steps);
                })
                .expect("spawn the window owner")
        };

        let drive_outcome = drive_the_window_event_pump_on_the_first_thread_until(
            Duration::from_millis(20),
            || {
                if work_while_driven_is_done.load(Ordering::Acquire) || window_owner.is_finished() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            || {},
        );
        assert_eq!(
            drive_outcome,
            WindowEventPumpDriveOnTheFirstThreadOutcome::DrivenUntilTheObservationBroke,
            "the first thread drives the loop until its observation breaks"
        );
        assert_eq!(
            drive_the_window_event_pump_on_the_first_thread_until(
                Duration::ZERO,
                || ControlFlow::Break(()),
                || {},
            ),
            WindowEventPumpDriveOnTheFirstThreadOutcome::DrivenUntilTheObservationBroke,
            "the loop is re-entered by a later drive rather than spent by the first"
        );

        // The loop is no longer driven: this is where a runtime's teardown
        // releases its windows.
        the_loop_is_no_longer_driven
            .send(())
            .expect("the window owner is waiting");
        let undriven_steps = undriven_steps_reported
            .recv_timeout(2 * TEARDOWN_TIME_STEP_BUDGET)
            .unwrap_or_else(|_| {
                panic!(
                    "a window step blocked while nothing drove the loop — it waited on the \
                     first thread instead of on the pump's own record"
                )
            });
        window_owner
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        assert!(
            undriven_steps.present_target_opened_after < TEARDOWN_TIME_STEP_BUDGET,
            "opening a present target while nothing drove the loop took {:?}",
            undriven_steps.present_target_opened_after
        );
        assert!(
            undriven_steps.windows_released_after < TEARDOWN_TIME_STEP_BUDGET,
            "releasing windows while nothing drove the loop took {:?}",
            undriven_steps.windows_released_after
        );
        assert_eq!(
            event_pump.registered_window_count(),
            2,
            "windows released while the loop is not driven stay registered until the pump \
             next runs"
        );

        release_the_windows_handed_back_while_the_event_pump_was_not_driven();
        assert_eq!(
            event_pump.registered_window_count(),
            0,
            "one turn of the pump after teardown releases what teardown handed back"
        );
    }

    /// Three windows at once: one deregistered while driven, one held to the
    /// named-surface contract, and one left for its present target to open
    /// later.
    fn mint_and_present_while_the_loop_is_driven(
        gpu_context_limited_access: &GpuContextLimitedAccess,
    ) -> (
        ProcessorOwnedWindow,
        streamlib_engine::core::rhi::Texture,
        ProcessorOwnedWindowAwaitingItsPresentTarget,
    ) {
        let event_pump = process_wide_window_event_pump()
            .expect("a worker reaches the pump the first thread built");
        let register = |window_title: &str| {
            ProcessorOwnedWindowAwaitingItsPresentTarget::register_on_the_process_wide_window_event_pump(
                request_for(window_title),
            )
        };

        let window_to_present = register("streamlib first-thread window test")
            .expect("the pump mints a window for a worker thread");
        let window_to_drop = register("streamlib first-thread window test — dropped").expect(
            "a second window registration — a refusal here is the one-event-loop-per-process \
             regression the shared pump exists to remove",
        );
        let window_to_open_later = register("streamlib first-thread window test — opened later")
            .expect("a third window registration");
        assert_eq!(
            event_pump.registered_window_count(),
            3,
            "the pump routes to every window at once"
        );
        drop(window_to_drop);
        let deadline = Instant::now() + Duration::from_secs(10);
        while event_pump.registered_window_count() > 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            event_pump.registered_window_count(),
            2,
            "a registration dropped while the pump is driven deregisters promptly"
        );

        let (presented_window, source_texture) =
            mint_and_hold_a_window_to_the_named_surface_contract(
                gpu_context_limited_access,
                window_to_present,
            );
        (presented_window, source_texture, window_to_open_later)
    }

    /// Choose the application menu's Quit as a user would, and check it asks
    /// the runtime to shut down rather than terminating the process.
    fn quit_from_the_application_menu_while_the_loop_is_driven() {
        dispatch2::DispatchQueue::main().exec_sync(|| {
            let first_thread =
                MainThreadMarker::new().expect("the main queue runs on the first thread");
            let application_submenu = NSApplication::sharedApplication(first_thread)
                .mainMenu()
                .and_then(|menu_bar| menu_bar.itemAtIndex(0))
                .and_then(|application_menu_item| application_menu_item.submenu())
                .expect("the pump installs an application menu on its first drive");
            application_submenu.performActionForItemAtIndex(0);
        });
        assert!(
            is_runtime_shutdown_requested(),
            "the menu's Quit must reach the runtime's shutdown request"
        );
        take_runtime_shutdown_escalation();
    }

    fn open_and_release_while_the_loop_is_not_driven(
        gpu_context_limited_access: &GpuContextLimitedAccess,
        presented_window: ProcessorOwnedWindow,
        window_to_open_later: ProcessorOwnedWindowAwaitingItsPresentTarget,
    ) -> StepsTakenWhileTheLoopWasNotDriven {
        let open_started_at = Instant::now();
        let opened_later = gpu_context_limited_access
            .escalate(|gpu_context_full_access| {
                ProcessorOwnedWindow::open_present_target_for_registered_window(
                    gpu_context_full_access,
                    window_to_open_later,
                )
            })
            .expect("a present target opens while nothing drives the loop");
        let present_target_opened_after = open_started_at.elapsed();

        let release_started_at = Instant::now();
        drop(presented_window);
        drop(opened_later);
        StepsTakenWhileTheLoopWasNotDriven {
            present_target_opened_after,
            windows_released_after: release_started_at.elapsed(),
        }
    }
}
