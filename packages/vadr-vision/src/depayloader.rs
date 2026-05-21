// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VadrVisionDepayloader` processor — accepts `NetworkPacket` payloads
//! on `chunks_in`, feeds each datagram into a [`DepayloaderState`]
//! reassembler, and emits one `EncodedJpegFrame` per completed frame on
//! `jpeg_out`. Incomplete frames are dropped on timeout; malformed and
//! duplicate chunks are counted and logged on first occurrence + powers
//! of two thereafter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::_generated_::{EncodedJpegFrame, NetworkPacket};
use crate::reassembly::{
    CompletedFrame, DEFAULT_MAX_PENDING, DEFAULT_TIMEOUT, DepayloaderState, DropReason,
    IngestOutcome,
};

#[streamlib::sdk::processor("VadrVisionDepayloader")]
pub struct VadrVisionDepayloaderProcessor {
    /// Reassembly state machine. `Option` so we can build it in
    /// `setup()` from the config (default values plumbed in).
    state: Option<DepayloaderState>,

    /// Monotonic frame counter — increments per emitted
    /// `EncodedJpegFrame` (NOT per source `frame_id`, which can repeat
    /// or skip). Threaded into `EncodedJpegFrame::frame_number` so
    /// downstream consumers see a monotonic series.
    frames_emitted: AtomicU64,

    /// Counters for periodic logging. Mirror `DepayloaderState::metrics`
    /// last-seen values so we can compute "since last log" deltas.
    drops_logged_so_far: AtomicU64,
}

impl ReactiveProcessor for VadrVisionDepayloaderProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let timeout = self
            .config
            .reassembly_timeout_ms
            .map(|ms| Duration::from_millis(ms as u64))
            .unwrap_or(DEFAULT_TIMEOUT);
        let max_pending = self
            .config
            .max_pending_frames
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_PENDING);

        self.state = Some(DepayloaderState::new(timeout, max_pending));

        tracing::info!(
            reassembly_timeout_ms = timeout.as_millis() as u64,
            max_pending_frames = max_pending,
            warn_on_drop = self.config.warn_on_drop.unwrap_or(true),
            "VadrVisionDepayloader: setup",
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("chunks_in") {
            return Ok(());
        }
        let packet: NetworkPacket = self.inputs.read("chunks_in")?;
        let warn = self.config.warn_on_drop.unwrap_or(true);

        // Re-stash to satisfy the borrow checker — we need `&mut state`
        // for ingest + sweep but also need to read `self.frames_emitted`
        // afterward. Pull the state out, drive it, put it back.
        let state = self
            .state
            .as_mut()
            .expect("setup() built the state — process() runs after setup");

        let now = Instant::now();

        // Sweep first so a quiet-stream sender that suddenly resumes
        // doesn't see stale partial frames sitting in the table forever
        // — and so the cap-evict path inside ingest() sees the
        // already-expired entries gone.
        let timed_out = state.sweep_timeouts(now);
        for reason in &timed_out {
            log_drop(warn, &self.drops_logged_so_far, reason);
        }

        match state.ingest(&packet.payload, now) {
            IngestOutcome::Progress => {}
            IngestOutcome::Completed(frame) => {
                emit_completed(&self.outputs, &self.frames_emitted, frame)?;
            }
            IngestOutcome::Dropped(reason) => {
                log_drop(warn, &self.drops_logged_so_far, &reason);
            }
        }

        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let metrics = self
            .state
            .as_ref()
            .map(|s| s.metrics())
            .unwrap_or_default();
        tracing::info!(
            frames_emitted = metrics.frames_emitted,
            frames_dropped_timeout = metrics.frames_dropped_timeout,
            frames_dropped_capacity = metrics.frames_dropped_capacity,
            chunks_dropped_malformed = metrics.chunks_dropped_malformed,
            chunks_dropped_duplicate = metrics.chunks_dropped_duplicate,
            frames_dropped_metadata_conflict = metrics.frames_dropped_metadata_conflict,
            "VadrVisionDepayloader: teardown",
        );
        Ok(())
    }
}

/// Emit a completed JPEG byte blob as an `EncodedJpegFrame`. The output
/// `timestamp_ns` is the simulator-domain `sim_time_ns` from the §4.6
/// header — that's the canonical timing for AGP vision and is what
/// downstream consumers (including `JpegDecoder`'s VideoFrame output)
/// should propagate. `frame_number` is the processor's own monotonic
/// counter, NOT the source `frame_id` (which can repeat or skip after
/// loss).
fn emit_completed(
    outputs: &streamlib::sdk::iceoryx2::OutputWriter,
    counter: &AtomicU64,
    completed: CompletedFrame,
) -> Result<()> {
    let frame_number = counter.fetch_add(1, Ordering::Relaxed);
    let log_first = frame_number == 0;

    let encoded = EncodedJpegFrame {
        data: completed.data,
        // sim_time_ns is the spec's u64 simulator timestamp. The wire
        // schema declares timestamp_ns as int64-as-string, so we
        // saturate-cast to i64 (the max possible u64 sim_time_ns is
        // ~584 years at nanosecond resolution; saturate is the right
        // behavior for the never-observed overflow case).
        timestamp_ns: i64::try_from(completed.sim_time_ns)
            .unwrap_or(i64::MAX)
            .to_string(),
        frame_number: frame_number.to_string(),
        // VADR-TS-002 §4.6 declares 30 Hz as the spec rate but the
        // depayloader has no visibility into actual on-the-wire rate
        // (which can drop under loss). Leave fps unset and let
        // downstream consumers configure rate if they need it.
        fps: None,
    };

    outputs.write("jpeg_out", &encoded)?;

    if log_first {
        tracing::info!(
            source_frame_id = completed.frame_id,
            sim_time_ns = completed.sim_time_ns,
            jpeg_bytes = encoded.data.len(),
            "VadrVisionDepayloader: first frame emitted",
        );
    } else if (frame_number + 1).is_multiple_of(300) {
        tracing::info!(
            frames_emitted = frame_number + 1,
            "VadrVisionDepayloader: emit progress",
        );
    }
    Ok(())
}

/// Log a `DropReason` via `tracing::warn` when `warn_on_drop` is true,
/// rate-limited to first occurrence + powers of two. Counter advance is
/// best-effort relaxed; missing the exact power-of-two boundary is fine
/// (this is logging, not metrics).
fn log_drop(warn: bool, counter: &AtomicU64, reason: &DropReason) {
    if !warn {
        return;
    }
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if n != 1 && !n.is_power_of_two() {
        return;
    }
    match reason {
        DropReason::HeaderError(err) => {
            tracing::warn!(
                drops_total = n,
                error = %err,
                "VadrVisionDepayloader: dropped malformed datagram",
            );
        }
        DropReason::MetadataConflict {
            frame_id,
            field,
            existing,
            incoming,
        } => {
            tracing::warn!(
                drops_total = n,
                frame_id = frame_id,
                field = field,
                existing = existing,
                incoming = incoming,
                "VadrVisionDepayloader: per-frame metadata conflict, dropping pending frame",
            );
        }
        DropReason::DuplicateChunk { frame_id, chunk_id } => {
            tracing::warn!(
                drops_total = n,
                frame_id = frame_id,
                chunk_id = chunk_id,
                "VadrVisionDepayloader: duplicate chunk dropped",
            );
        }
        DropReason::Timeout {
            frame_id,
            chunks_received,
            total_chunks,
        } => {
            tracing::warn!(
                drops_total = n,
                frame_id = frame_id,
                chunks_received = chunks_received,
                total_chunks = total_chunks,
                "VadrVisionDepayloader: reassembly timeout, dropping incomplete frame",
            );
        }
        DropReason::CapacityEvicted {
            frame_id,
            chunks_received,
            total_chunks,
        } => {
            tracing::warn!(
                drops_total = n,
                frame_id = frame_id,
                chunks_received = chunks_received,
                total_chunks = total_chunks,
                "VadrVisionDepayloader: pending-frame cap reached, evicting oldest",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `log_drop` with `warn=false` must not advance the counter (the
    /// caller relies on this so opt-out is total). The point of the
    /// `warn` knob is "I'm OK with silent drops, don't even rate-limit
    /// — count internally via state metrics".
    #[test]
    fn log_drop_silent_mode_does_not_increment_counter() {
        let counter = AtomicU64::new(0);
        let reason = DropReason::DuplicateChunk {
            frame_id: 1,
            chunk_id: 0,
        };
        for _ in 0..10 {
            log_drop(false, &counter, &reason);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    /// `log_drop` with `warn=true` increments per call regardless of
    /// whether the call actually emitted a log line. Caller depends on
    /// the counter being a faithful drop-count for periodic reporting.
    #[test]
    fn log_drop_warn_mode_increments_each_call() {
        let counter = AtomicU64::new(0);
        let reason = DropReason::DuplicateChunk {
            frame_id: 1,
            chunk_id: 0,
        };
        for _ in 0..10 {
            log_drop(true, &counter, &reason);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 10);
    }
}
