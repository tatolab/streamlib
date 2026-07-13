// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Microphone → CLAP Reverb → Speaker Example
//!
//! Demonstrates streamlib's audio processing pipeline using CLAP as the
//! "shader language for audio": `AudioCapture → ClapEffect (reverb) →
//! AudioOutput`, with the reverb plugin discovered on the host via
//! `ClapScanner`.
//!
//! # Registry-only migration status — DEFERRED, not in scope
//!
//! This example is intentionally a no-op at HEAD. Its real implementation
//! (preserved in git history before the registry-only migration) cannot yet
//! be expressed as a standalone example for two reasons:
//!
//! 1. The CLAP plugin host (`@tatolab/clap`) is macOS/iOS-only and is **not**
//!    distributable as a package, so a standalone consumer cannot resolve it
//!    by version.
//! 2. The pipeline relies on `ClapScanner` — a compile-time library API for
//!    discovering installed CLAP plugins — and the deprecated typed-struct
//!    processor API. There is no runtime graph-API (`add_processor` /
//!    `processor_type_ref!`) equivalent for CLAP plugin discovery yet.
//!    Designing that runtime CLAP-discovery story is the out-of-scope work
//!    this example waits on.
//!
//! When CLAP ships on Linux and a runtime plugin-discovery path exists, restore
//! the `AudioCapture → ClapEffect → AudioOutput` pipeline from git history and
//! reference each processor with `processor_type_ref!` (no version, no load
//! call); each provider's package resolves from this app's `streamlib_modules/`
//! folder, populated by `./setup.sh`.

fn main() {
    eprintln!(
        "microphone-reverb-speaker is deferred and currently a no-op — its CLAP \
         reverb pipeline is macOS/iOS-only and has no registry-only / runtime \
         plugin-discovery path yet. See the module-level note in src/main.rs."
    );
}
