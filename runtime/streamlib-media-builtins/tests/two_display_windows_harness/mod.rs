// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One test-pattern source fanned out to two `DisplayWindow` instances — the
//! graph every platform's two-window harness asserts against its own window
//! server.

use std::time::Duration;

use serde_json::json;
use streamlib::sdk::App;
use streamlib_media_builtins::{
    DisplayWindow, TestPatternSource, register_media_builtin_processor_types,
};

pub const FIRST_WINDOW_TITLE: &str = "streamlib two-window harness — first";
pub const SECOND_WINDOW_TITLE: &str = "streamlib two-window harness — second";

/// How long the graph is held up once both windows are live, long enough to
/// photograph: `STREAMLIB_TWO_WINDOW_HARNESS_SECONDS`, six by default.
pub fn harness_duration() -> Duration {
    let seconds = std::env::var("STREAMLIB_TWO_WINDOW_HARNESS_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(6);
    Duration::from_secs(seconds)
}

/// Add the source and both displays to `app` and link them.
pub fn add_one_source_fanned_out_to_two_display_windows(app: &App) {
    register_media_builtin_processor_types();

    let pattern_source = app
        .add(
            TestPatternSource::Processor::processor_class_import_path(),
            json!({ "width": 1280, "height": 720 }),
            Some("pattern-source"),
        )
        .expect("the test-pattern source");
    let first_display = app
        .add(
            DisplayWindow::Processor::processor_class_import_path(),
            json!({ "title": FIRST_WINDOW_TITLE, "width": 640, "height": 360 }),
            Some("first-display"),
        )
        .expect("the first display");
    let second_display = app
        .add(
            DisplayWindow::Processor::processor_class_import_path(),
            json!({ "title": SECOND_WINDOW_TITLE, "width": 640, "height": 360 }),
            Some("second-display"),
        )
        .expect(
            "the second display — one process may hold only one winit event loop, and before \
             the shared pump this is where a second window-owning processor died",
        );

    app.connect((&pattern_source, "video"), (&first_display, "video"))
        .expect("source to the first display");
    app.connect((&pattern_source, "video"), (&second_display, "video"))
        .expect("source to the second display");
}
