// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Screen Recorder Example — Screen Capture → MP4 Writer
//!
//! Captures the screen and records it to an MP4 file.
//!
//! # Registry-only migration status — DEFERRED, not in scope
//!
//! This example is intentionally a no-op at HEAD. Its real implementation
//! (preserved in git history before the registry-only migration) cannot yet
//! be a standalone example: `@tatolab/screen-capture` is
//! **Apple-only** (macOS/iOS via ScreenCaptureKit) — there is no Linux
//! screen-capture backend — and the pipeline still uses the deprecated
//! compile-time typed-struct API rather than the runtime graph API
//! (`add_processor` + `processor_type_ref!`).
//!
//! When a Linux screen-capture backend lands, restore the
//! `ScreenCapture → Mp4Writer` pipeline from git history and reference each
//! processor with `processor_type_ref!` (no version, no load call); each
//! provider's package resolves from this app's `streamlib_modules/` folder,
//! populated by `./setup.sh`.

fn main() {
    eprintln!(
        "screen-recorder is deferred and currently a no-op — screen capture is \
         macOS/iOS-only (ScreenCaptureKit) with no Linux backend yet, and the \
         example has no registry-only path. See the module-level note in \
         src/main.rs."
    );
}
