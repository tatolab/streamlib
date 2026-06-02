// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Monotonic process clock used by [`crate::iceoryx2::OutputWriter::write`]
//! to stamp frame timestamps. Engine-free twin of the engine's
//! `core::media_clock::MediaClock` (non-macOS arm) so the SDK's output-writer
//! view can timestamp without linking the engine.

/// Monotonic process clock.
pub struct MediaClock;

impl MediaClock {
    /// Elapsed time since the first call, monotonic.
    #[inline]
    pub fn now() -> std::time::Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(std::time::Instant::now);
        start.elapsed()
    }
}
