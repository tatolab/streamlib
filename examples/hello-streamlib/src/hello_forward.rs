// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The inline processor at the heart of `hello-streamlib`.
//!
//! `HelloForward` sits between the camera source and the display sink and
//! forwards every frame from `video_in` to `video_out` verbatim. It is the
//! zero-ceremony proof of the milestone: an `#[processor]` authored inline in
//! an application crate with
//!
//! - no `build.rs`,
//! - no `streamlib.yaml`,
//! - no `schemas:` list,
//! - no `_generated_` module.
//!
//! The identity is omitted, so the macro synthesizes `@app/local/HelloForward`
//! — an app-local processor the runtime registers live via `App::add_local`
//! with no package on disk. The frame payload is treated opaquely (`read_raw`
//! → `write_raw`), so the processor needs no frame type, no codegen, and no
//! schema package: it forwards whatever bytes the camera produced straight to
//! the display, incrementing an observable counter per frame.

use std::sync::atomic::{AtomicU64, Ordering};

/// Inline forward processor: `video_in` → `video_out`, verbatim.
///
/// `frames_forwarded` is the observability surface — the headless E2E reads it
/// to assert frames traversed the processor, and it is the count a host would
/// surface in its own logs.
#[streamlib::sdk::processor(
    execution = reactive,
    input(
        "video_in",
        "@tatolab/core/VideoFrame",
        description = "Frames from the upstream camera source"
    ),
    output(
        "video_out",
        "@tatolab/core/VideoFrame",
        description = "The same frames, forwarded verbatim to the downstream sink"
    ),
)]
pub struct HelloForward {
    frames_forwarded: AtomicU64,
}

impl streamlib::sdk::processors::ReactiveProcessor for HelloForward::Processor {
    fn process(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> streamlib::sdk::error::Result<()> {
        self.forward_pending()?;
        Ok(())
    }
}

impl HelloForward::Processor {
    /// Forward one pending frame from `video_in` to `video_out` byte-for-byte.
    ///
    /// Returns `Ok(true)` when a frame was forwarded and `Ok(false)` when no
    /// frame was pending. Split out from [`process`](Self::process) so it can be
    /// driven directly — with a fixture-populated input mailbox — without a
    /// live runtime context, which is exactly what the headless E2E does.
    pub fn forward_pending(&mut self) -> streamlib::sdk::error::Result<bool> {
        if !self.inputs.has_data("video_in") {
            return Ok(false);
        }
        let Some((frame_bytes, timestamp_ns)) = self.inputs.read_raw("video_in")? else {
            return Ok(false);
        };
        self.outputs
            .write_raw("video_out", &frame_bytes, timestamp_ns)?;
        self.frames_forwarded.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Number of frames forwarded so far.
    pub fn frames_forwarded(&self) -> u64 {
        self.frames_forwarded.load(Ordering::Relaxed)
    }
}
