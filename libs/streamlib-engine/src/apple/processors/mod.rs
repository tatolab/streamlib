// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod screen_capture;

// Display processor lives in `@tatolab/display` (#674).
// MP4 writer processor lives in `@tatolab/mp4` (#678) — the Apple
// implementation is preserved there but gated off pending an Apple
// SDK surface.

pub use screen_capture::AppleScreenCaptureProcessor;
