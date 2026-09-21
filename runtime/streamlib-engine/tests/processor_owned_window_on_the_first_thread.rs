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
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    use streamlib_engine::core::context::GpuContext;
    use streamlib_engine::core::processor_owned_window::{
        NamedSurfacePresentationOutcome, ProcessorOwnedWindow,
        ProcessorOwnedWindowAwaitingItsPresentTarget, ProcessorOwnedWindowRequest,
        SurfaceNamedForPresentationOnOwnedWindow,
    };
    use streamlib_engine::core::rhi::TextureFormat;
    use streamlib_engine::core::window_event_pump::{
        WindowEventPumpDriveOnTheFirstThread, WindowRegistrationRequestFromOwningProcessor,
        close_the_windows_released_while_the_event_pump_was_not_driven,
        drive_the_window_event_pump_on_the_first_thread_until, process_wide_window_event_pump,
    };
    use streamlib_engine::host_rhi::PresentScalingMode;
    use streamlib_engine::logging::{LoggingTunables, StreamlibLoggingConfig};

    const SOURCE_EXTENT_IN_PIXELS: u32 = 256;

    /// A surface id that names nothing this process can resolve.
    const UNRESOLVABLE_SURFACE_ID: &str = "a-surface-this-process-never-saw#7";

    /// Past `DisplayWindow`'s own two-second stop budget: a drop still blocked
    /// here is one a display's teardown would have detached and leaked.
    const TEARDOWN_TIME_WINDOW_RELEASE_BUDGET: Duration = Duration::from_secs(3);

    fn request_for(window_title: &str) -> ProcessorOwnedWindowRequest {
        ProcessorOwnedWindowRequest {
            window_registration_request: WindowRegistrationRequestFromOwningProcessor {
                window_title: window_title.to_string(),
                initial_width_in_physical_pixels: 320,
                initial_height_in_physical_pixels: 240,
            },
            scaling_mode_for_frame_in_window: PresentScalingMode::Fit,
        }
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

        let presentation_is_done = Arc::new(AtomicBool::new(false));
        let (release_the_kept_window_now, kept_window_release) = channel::<()>();
        let (kept_window_released_after, kept_window_release_elapsed) = channel::<Duration>();
        let worker = {
            let presentation_is_done = Arc::clone(&presentation_is_done);
            std::thread::Builder::new()
                .name("window-owner".to_string())
                .spawn(move || {
                    let kept_window = present_named_surfaces_from_a_worker_thread();
                    presentation_is_done.store(true, Ordering::Release);
                    wait_then_release_while_the_pump_is_not_driven(
                        kept_window,
                        kept_window_release,
                        kept_window_released_after,
                    );
                })
                .expect("spawn the window owner")
        };

        let drive_outcome = drive_the_window_event_pump_on_the_first_thread_until(
            Duration::from_millis(20),
            || {
                if presentation_is_done.load(Ordering::Acquire) || worker.is_finished() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            || {},
        );
        assert_eq!(
            drive_outcome,
            WindowEventPumpDriveOnTheFirstThread::DrivenUntilTheObservationBroke,
            "the first thread drives the loop until its observation breaks"
        );
        assert_eq!(
            drive_the_window_event_pump_on_the_first_thread_until(
                Duration::ZERO,
                || ControlFlow::Break(()),
                || {},
            ),
            WindowEventPumpDriveOnTheFirstThread::DrivenUntilTheObservationBroke,
            "the loop is re-entered by a later drive rather than spent by the first"
        );

        // The loop is no longer driven: this is where a runtime's teardown
        // releases its windows.
        release_the_kept_window_now
            .send(())
            .expect("the window owner is waiting");
        let released_after = kept_window_release_elapsed
            .recv_timeout(TEARDOWN_TIME_WINDOW_RELEASE_BUDGET)
            .unwrap_or_else(|_| {
                panic!(
                    "releasing a window while nothing drove the loop blocked past \
                     {TEARDOWN_TIME_WINDOW_RELEASE_BUDGET:?} — its close waited on the first \
                     thread instead of being handed to the pump"
                )
            });
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        assert_eq!(
            event_pump.registered_window_count(),
            1,
            "a window released while the loop is not driven stays registered until the pump \
             next runs (released after {released_after:?})"
        );

        close_the_windows_released_while_the_event_pump_was_not_driven();
        assert_eq!(
            event_pump.registered_window_count(),
            0,
            "one turn of the pump after teardown closes what teardown released"
        );
    }

    /// Mint two windows at once, present on one, and hand it back for the
    /// teardown-time release.
    fn present_named_surfaces_from_a_worker_thread() -> ProcessorOwnedWindow {
        let gpu_context = GpuContext::init_for_platform().expect("a GPU is required for this tier");
        let gpu_context_limited_access = gpu_context.limited_access();
        let event_pump = process_wide_window_event_pump()
            .expect("a worker reaches the pump the first thread built");

        let registered_window =
            ProcessorOwnedWindowAwaitingItsPresentTarget::register_on_the_process_wide_window_event_pump(
                request_for("streamlib first-thread window test"),
            )
            .expect("the pump mints a window for a worker thread");
        let second_registered_window =
            ProcessorOwnedWindowAwaitingItsPresentTarget::register_on_the_process_wide_window_event_pump(
                request_for("streamlib first-thread window test — second"),
            )
            .expect(
                "a second window registration — a refusal here is the one-event-loop-per-process \
                 regression the shared pump exists to remove",
            );
        assert_eq!(
            event_pump.registered_window_count(),
            2,
            "the pump routes to both windows at once"
        );
        drop(second_registered_window);
        let deadline = Instant::now() + Duration::from_secs(10);
        while event_pump.registered_window_count() > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            event_pump.registered_window_count(),
            1,
            "a registration dropped while the pump is driven deregisters promptly"
        );

        let (published_surface_id, source_texture, mut processor_owned_window) =
            gpu_context_limited_access
                .escalate(|gpu_context_full_access| {
                    let (published_surface_id, source_texture) = gpu_context_full_access
                        .acquire_output_texture(
                            SOURCE_EXTENT_IN_PIXELS,
                            SOURCE_EXTENT_IN_PIXELS,
                            TextureFormat::Bgra8Unorm,
                        )?;
                    let processor_owned_window =
                        ProcessorOwnedWindow::open_present_target_for_registered_window(
                            gpu_context_full_access,
                            registered_window,
                        )?;
                    Ok((published_surface_id, source_texture, processor_owned_window))
                })
                .expect(
                    "the present target mints from the Metal layer off the first thread, under \
                     one escalate with the compositor",
                );

        let (width, height) = processor_owned_window.current_extent_in_physical_pixels();
        assert!(
            width > 0 && height > 0,
            "the window reports a legal swapchain extent, got {width}x{height}"
        );

        let named_surface = SurfaceNamedForPresentationOnOwnedWindow {
            surface_id: &published_surface_id,
            source_width_in_pixels: SOURCE_EXTENT_IN_PIXELS,
            source_height_in_pixels: SOURCE_EXTENT_IN_PIXELS,
            producer_published_texture_layout: None,
            color_traits_of_frame: None,
            hdr_static_metadata_of_frame: None,
        };
        assert_eq!(
            processor_owned_window
                .show_named_surface(named_surface)
                .expect("a resolvable id composes and presents"),
            NamedSurfacePresentationOutcome::ComposedAndPresented,
            "naming a published surface must reach the window"
        );
        assert_eq!(
            processor_owned_window
                .show_named_surface(SurfaceNamedForPresentationOnOwnedWindow {
                    surface_id: UNRESOLVABLE_SURFACE_ID,
                    ..named_surface
                })
                .expect("an id that resolves to nothing is an outcome, not an error"),
            NamedSurfacePresentationOutcome::SurfaceIdDidNotResolve,
            "an unresolvable id must be reported rather than drawn or raised"
        );
        assert_eq!(
            processor_owned_window
                .show_named_surface(named_surface)
                .expect("the window still presents after an unresolvable id"),
            NamedSurfacePresentationOutcome::ComposedAndPresented,
            "one unresolvable id must not wedge the window for every later frame"
        );

        drop(source_texture);
        processor_owned_window
    }

    fn wait_then_release_while_the_pump_is_not_driven(
        kept_window: ProcessorOwnedWindow,
        release_the_kept_window_now: Receiver<()>,
        kept_window_released_after: std::sync::mpsc::Sender<Duration>,
    ) {
        release_the_kept_window_now
            .recv()
            .expect("the first thread says when the loop has stopped");
        let release_started_at = Instant::now();
        drop(kept_window);
        let _ = kept_window_released_after.send(release_started_at.elapsed());
    }
}
