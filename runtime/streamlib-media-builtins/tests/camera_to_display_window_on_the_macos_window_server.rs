// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CameraSource` → `DisplayWindow` on Apple, asserted against the window
//! server — the macOS counterpart of the camera→display E2E fixture, which
//! drives the wheel and so waits on the macOS wheel.
//!
//! A collector fanned out beside the window reads every bag the camera
//! publishes, as any downstream consumer does. It holds the camera to one
//! capture instant on both stamps, and writes one frame's pixels to a PNG
//! scored for real content: `STREAMLIB_CAMERA_DISPLAY_PNG` names where, a
//! temporary file otherwise.
//!
//! `harness = false`: the graph runs under `App::run`, which drives the window
//! event pump on the process's first thread, and libtest never runs a test
//! there. Rig tier — a camera, the window server, a GPU, and camera access
//! allowed for the terminal running it — so Cargo builds it only under
//! `hardware-tests`.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    apple_camera_to_display::run();
}

#[cfg(target_os = "macos")]
mod apple_camera_to_display {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_graphics::{
        CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID, kCGWindowName,
        kCGWindowOwnerPID,
    };
    use serde_json::json;
    use streamlib::sdk::App;
    use streamlib::sdk::context::RuntimeContextLimitedAccess;
    use streamlib::sdk::error::Result;
    use streamlib::sdk::media_clock::MediaClock;
    use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ReactiveProcessor};
    use streamlib::sdk::runtime::{Runner, RuntimeStatus};
    use streamlib::sdk::runtime_control::request_runtime_shutdown;
    use streamlib_media_builtins::{
        CameraSource, DisplayWindow, VideoFrame, register_media_builtin_processor_types,
    };

    const WINDOW_TITLE: &str = "streamlib camera→display harness";

    /// Enough frames for a steady stream, and the one whose pixels are scored —
    /// past the first few, which an auto-exposing camera delivers dark.
    const FRAMES_TO_COLLECT: usize = 60;
    const FRAME_WHOSE_PIXELS_ARE_SCORED: usize = 30;

    /// A cold swapchain and a cold camera are the slow steps.
    const WINDOW_MAPPED_DEADLINE: Duration = Duration::from_secs(20);
    const FRAMES_COLLECTED_DEADLINE: Duration = Duration::from_secs(20);

    /// Below this, a frame's luma spread is a flat field rather than anything
    /// a camera sees.
    const LEAST_LUMA_STANDARD_DEVIATION_OF_REAL_CONTENT: f64 = 4.0;

    #[derive(Default)]
    struct WhatTheCollectorSaw {
        payload_and_envelope_timestamps_ns: Vec<(i64, i64)>,
        read_at_ns: Vec<i64>,
        scored_frame: Option<(u32, u32, Vec<u8>)>,
        refusals: Vec<String>,
    }

    fn what_the_collector_saw() -> &'static Mutex<WhatTheCollectorSaw> {
        static SEEN: OnceLock<Mutex<WhatTheCollectorSaw>> = OnceLock::new();
        SEEN.get_or_init(Mutex::default)
    }

    /// Reads the camera's bags beside the window, the way any downstream
    /// consumer does.
    #[streamlib::sdk::processor(
        description = "Collects the camera's published frames for the harness's assertions",
        execution = reactive,
        input(
            "video",
            delivery_profile = "ordered",
            description = "The camera's video frames"
        )
    )]
    pub struct CameraFrameCollector;

    impl ReactiveProcessor for CameraFrameCollector::Processor {
        fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
            while let Some((bag_bytes, envelope_timestamp_ns)) = self.inputs.read_raw("video")? {
                let read_at_ns = MediaClock::now().as_nanos() as i64;
                let mut seen = what_the_collector_saw().lock().unwrap();
                let frame = match rmp_serde::from_slice::<VideoFrame>(&bag_bytes) {
                    Ok(frame) => frame,
                    Err(refusal) => {
                        seen.refusals.push(refusal.to_string());
                        continue;
                    }
                };
                if seen.payload_and_envelope_timestamps_ns.len() == FRAME_WHOSE_PIXELS_ARE_SCORED {
                    match ctx
                        .gpu_limited_access()
                        .resolve_pixel_buffer_by_surface_id(&frame.surface_id)
                    {
                        Ok(pixel_buffer) => {
                            let byte_len = frame.width as usize * frame.height as usize * 4;
                            // SAFETY: the bag names a live pooled `Rgba32` slot of
                            // this extent — `width × height × 4` tightly packed.
                            let rgba = unsafe {
                                std::slice::from_raw_parts(
                                    pixel_buffer.plane_base_address(0),
                                    byte_len,
                                )
                            };
                            seen.scored_frame = Some((frame.width, frame.height, rgba.to_vec()));
                        }
                        Err(e) => seen.refusals.push(e.to_string()),
                    }
                }
                seen.payload_and_envelope_timestamps_ns
                    .push((frame.timestamp_ns, envelope_timestamp_ns));
                seen.read_at_ns.push(read_at_ns);
            }
            Ok(())
        }
    }

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

    fn wait_until(deadline: Duration, condition: impl Fn() -> bool) -> bool {
        let give_up_at = Instant::now() + deadline;
        while Instant::now() < give_up_at {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        condition()
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
            let _ = request_runtime_shutdown("the camera→display watcher is done");
        }
    }

    fn watch_the_camera_reach_the_window(runner: &Runner) -> std::result::Result<(), String> {
        if !wait_until(WINDOW_MAPPED_DEADLINE, || {
            windows_on_screen_titled(WINDOW_TITLE) == 1
        }) {
            return Err(format!(
                "the display must own one live window; the window server showed {}",
                windows_on_screen_titled(WINDOW_TITLE)
            ));
        }
        if !wait_until(FRAMES_COLLECTED_DEADLINE, || {
            what_the_collector_saw()
                .lock()
                .unwrap()
                .payload_and_envelope_timestamps_ns
                .len()
                >= FRAMES_TO_COLLECT
        }) {
            return Err(format!(
                "only {} camera frames reached the collector in {FRAMES_COLLECTED_DEADLINE:?} — \
                 is camera access allowed for the terminal running this (System Settings › \
                 Privacy & Security › Camera)?",
                what_the_collector_saw()
                    .lock()
                    .unwrap()
                    .payload_and_envelope_timestamps_ns
                    .len()
            ));
        }
        if runner.status() != RuntimeStatus::Started {
            return Err(format!(
                "the graph must still be running, but the runtime is {:?}",
                runner.status()
            ));
        }
        Ok(())
    }

    pub fn run() {
        // SAFETY: registers a plain `extern "C"` fn that captures nothing.
        unsafe { libc::atexit(fail_an_exit_that_did_not_come_from_the_end_of_run) };
        register_media_builtin_processor_types();
        PROCESSOR_REGISTRY.register::<CameraFrameCollector::Processor>();

        let app = App::new().expect("runtime");
        let camera = app
            .add(
                CameraSource::Processor::processor_class_import_path(),
                json!({ "max_width": 1280, "max_height": 720 }),
                Some("camera"),
            )
            .expect("the camera");
        let display = app
            .add(
                DisplayWindow::Processor::processor_class_import_path(),
                json!({ "title": WINDOW_TITLE, "width": 640, "height": 360, "scaling": "fit" }),
                Some("display"),
            )
            .expect("the display");
        let collector = app
            .add(
                CameraFrameCollector::Processor::processor_class_import_path(),
                json!({}),
                Some("collector"),
            )
            .expect("the collector");
        app.connect((&camera, "video"), (&display, "video"))
            .expect("camera to the display");
        app.connect((&camera, "video"), (&collector, "video"))
            .expect("camera to the collector");

        let runner = Arc::clone(app.runner());
        let watcher = std::thread::Builder::new()
            .name("camera-display-watcher".to_string())
            .spawn(move || {
                let _ends_the_run_however_the_watch_ends = RequestTheShutdownThatEndsTheRunOnDrop;
                watch_the_camera_reach_the_window(&runner)
            })
            .expect("spawn the watcher");

        let run_outcome = app.run();
        let watched = watcher
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        if let Err(what_the_watcher_saw) = watched {
            panic!("{what_the_watcher_saw}");
        }
        run_outcome.expect("the graph stops cleanly");

        let seen = what_the_collector_saw().lock().unwrap();
        assert!(seen.refusals.is_empty(), "{:?}", seen.refusals);
        for (&(payload_timestamp_ns, envelope_timestamp_ns), &read_at_ns) in seen
            .payload_and_envelope_timestamps_ns
            .iter()
            .zip(&seen.read_at_ns)
        {
            assert_eq!(
                payload_timestamp_ns, envelope_timestamp_ns,
                "one frame, one capture instant, on both of its stamps"
            );
            assert!(payload_timestamp_ns > 0);
            assert!(
                payload_timestamp_ns <= read_at_ns,
                "a frame is captured before a consumer reads it"
            );
        }
        for (earlier, later) in seen
            .payload_and_envelope_timestamps_ns
            .iter()
            .zip(seen.payload_and_envelope_timestamps_ns.iter().skip(1))
        {
            assert!(
                later.0 > earlier.0,
                "capture instants increase frame to frame"
            );
        }

        let (width, height, rgba) = seen
            .scored_frame
            .as_ref()
            .expect("the scored frame was read");
        let png_path = std::env::var_os("STREAMLIB_CAMERA_DISPLAY_PNG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("streamlib-camera-display.png"));
        write_rgba_png(&png_path, *width, *height, rgba);
        let luma_standard_deviation = luma_standard_deviation_of(rgba);
        tracing::info!(
            png = %png_path.display(),
            width,
            height,
            luma_standard_deviation,
            "scored frame written"
        );
        assert!(
            luma_standard_deviation > LEAST_LUMA_STANDARD_DEVIATION_OF_REAL_CONTENT,
            "frame {FRAME_WHOSE_PIXELS_ARE_SCORED} is a flat field; see {}",
            png_path.display()
        );
        RUN_REACHED_ITS_END.store(true, Ordering::SeqCst);
    }

    fn luma_standard_deviation_of(rgba: &[u8]) -> f64 {
        let lumas: Vec<f64> = rgba
            .chunks_exact(4)
            .map(|pixel| {
                0.2126 * pixel[0] as f64 + 0.7152 * pixel[1] as f64 + 0.0722 * pixel[2] as f64
            })
            .collect();
        let mean = lumas.iter().sum::<f64>() / lumas.len() as f64;
        (lumas.iter().map(|luma| (luma - mean).powi(2)).sum::<f64>() / lumas.len() as f64).sqrt()
    }

    fn write_rgba_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
        let file = std::fs::File::create(path).expect("the PNG path is writable");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(rgba))
            .expect("the PNG encodes");
    }
}
