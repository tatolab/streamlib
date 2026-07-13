// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera + Audio Recorder Example — Camera + Microphone → MP4
//!
//! Records synchronized camera video and microphone audio into an MP4 file.
//!
//! # Registry-only migration status — DEFERRED, not in scope
//!
//! This example is intentionally a no-op at HEAD. Its real implementation
//! (preserved in git history before the registry-only migration) is
//! macOS/iOS-only: the Linux MP4 writer (`@tatolab/mp4`'s `LinuxMp4Writer`)
//! does not yet accept an audio input, so there is no Linux path for an
//! audio+video recorder, and the pipeline still uses the deprecated
//! compile-time typed-struct API rather than the runtime graph API
//! (`add_processor` + `processor_type_ref!`).
//!
//! When the Linux MP4 writer gains an audio input, restore the
//! `Camera + AudioCapture → Mp4Writer` pipeline from git history and reference
//! each processor with `processor_type_ref!` (no version, no load call); each
//! provider's package resolves from this app's `streamlib_modules/` folder,
//! populated by `./setup.sh`.

fn main() {
    eprintln!(
        "camera-audio-recorder is deferred and currently a no-op — synchronized \
         audio+video MP4 recording is macOS/iOS-only (the Linux MP4 writer has \
         no audio input yet) and the example has no registry-only path. See the \
         module-level note in src/main.rs."
    );
}
