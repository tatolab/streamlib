// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! VADR-TS-002 §4.6 chunk-reassembly state machine. Pure; no streamlib
//! types, no async, no I/O — the processor wraps it.
//!
//! Owns the `HashMap<frame_id, FrameAssembly>` and the rules for
//! ingesting a chunk, sweeping timed-out frames, and emitting a
//! completed JPEG byte blob.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::header::{self, ChunkHeader};

/// Default reassembly timeout — see `streamlib.yaml` schema description.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(200);

/// Default max pending frames — see `streamlib.yaml` schema description.
pub const DEFAULT_MAX_PENDING: usize = 8;

/// A reassembled JPEG byte blob ready to emit downstream. Carries the
/// timing + identity fields the depayloader propagates to its output
/// `EncodedJpegFrame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedFrame {
    pub frame_id: u32,
    pub sim_time_ns: u64,
    pub data: Vec<u8>,
}

/// Reasons a chunk or frame was dropped. Surfaced for tests + logging
/// (the processor turns them into typed `tracing::warn` events).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    /// Header parse failed — datagram is malformed at the wire level.
    HeaderError(header::HeaderError),
    /// Chunk arrived for a frame_id with conflicting per-frame metadata
    /// (`total_chunks` or `jpeg_size` disagreed with the first chunk).
    /// The entire pending frame is dropped so we don't mix bytes from
    /// what is clearly two different stream resets sharing a frame_id.
    MetadataConflict {
        frame_id: u32,
        field: &'static str,
        existing: u64,
        incoming: u64,
    },
    /// `chunk_id` already received — duplicate datagram, drop.
    DuplicateChunk { frame_id: u32, chunk_id: u16 },
    /// Reassembly took longer than `timeout`. Per §4.6, vision streams
    /// are loss-tolerant — drop and advance.
    Timeout {
        frame_id: u32,
        chunks_received: u16,
        total_chunks: u16,
    },
    /// `max_pending_frames` cap reached and a new frame arrived. Oldest
    /// pending frame is evicted to make room.
    CapacityEvicted {
        frame_id: u32,
        chunks_received: u16,
        total_chunks: u16,
    },
}

/// Result of feeding one datagram into the assembler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Chunk accepted; frame still incomplete.
    Progress,
    /// All chunks present → reassembled JPEG byte blob ready.
    Completed(CompletedFrame),
    /// Chunk or frame dropped — see `DropReason`. The assembler stays
    /// in a coherent state regardless; the caller may log + advance.
    Dropped(DropReason),
}

/// Internal per-frame accumulator. `chunks` is sized to `total_chunks`
/// at first-chunk time; `Some(bytes)` means received, `None` means
/// pending.
struct FrameAssembly {
    total_chunks: u16,
    jpeg_size: u32,
    sim_time_ns: u64,
    chunks_received: u16,
    received_bytes: u32,
    chunks: Vec<Option<Vec<u8>>>,
    first_seen: Instant,
}

impl FrameAssembly {
    fn new(header: &ChunkHeader, now: Instant) -> Self {
        let mut chunks = Vec::with_capacity(header.total_chunks as usize);
        chunks.resize_with(header.total_chunks as usize, || None);
        Self {
            total_chunks: header.total_chunks,
            jpeg_size: header.jpeg_size,
            sim_time_ns: header.sim_time_ns,
            chunks_received: 0,
            received_bytes: 0,
            chunks,
            first_seen: now,
        }
    }

    fn is_complete(&self) -> bool {
        self.chunks_received == self.total_chunks && self.received_bytes == self.jpeg_size
    }

    /// Drain the accumulated chunks into a single contiguous byte blob.
    /// Caller must have verified `is_complete()` first; this asserts.
    fn assemble(self, frame_id: u32) -> CompletedFrame {
        debug_assert!(self.is_complete(), "assemble called on incomplete frame");
        let mut data = Vec::with_capacity(self.jpeg_size as usize);
        for slot in self.chunks {
            // Safe to unwrap: is_complete() guarantees every slot is Some.
            data.extend_from_slice(&slot.expect("complete frame has all chunks"));
        }
        CompletedFrame {
            frame_id,
            sim_time_ns: self.sim_time_ns,
            data,
        }
    }
}

/// Reassembly state machine. Independent of any framework — built and
/// driven by the processor on the hot path, but unit-testable in
/// isolation.
pub struct DepayloaderState {
    pending: HashMap<u32, FrameAssembly>,
    timeout: Duration,
    max_pending: usize,
    // Counters surfaced via `metrics()` for periodic logging.
    frames_emitted: u64,
    frames_dropped_timeout: u64,
    frames_dropped_capacity: u64,
    chunks_dropped_malformed: u64,
    chunks_dropped_duplicate: u64,
    frames_dropped_metadata_conflict: u64,
}

/// Lightweight snapshot of accumulated counters — copied out for logging
/// so the caller doesn't need to peek into the state struct's internals.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub frames_emitted: u64,
    pub frames_dropped_timeout: u64,
    pub frames_dropped_capacity: u64,
    pub chunks_dropped_malformed: u64,
    pub chunks_dropped_duplicate: u64,
    pub frames_dropped_metadata_conflict: u64,
}

impl DepayloaderState {
    pub fn new(timeout: Duration, max_pending: usize) -> Self {
        Self {
            pending: HashMap::new(),
            timeout,
            // 0 would deadlock the cap-evict path. Clamp to ≥ 1 so a
            // misconfigured cap can't brick the depayloader.
            max_pending: max_pending.max(1),
            frames_emitted: 0,
            frames_dropped_timeout: 0,
            frames_dropped_capacity: 0,
            chunks_dropped_malformed: 0,
            chunks_dropped_duplicate: 0,
            frames_dropped_metadata_conflict: 0,
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn metrics(&self) -> Metrics {
        Metrics {
            frames_emitted: self.frames_emitted,
            frames_dropped_timeout: self.frames_dropped_timeout,
            frames_dropped_capacity: self.frames_dropped_capacity,
            chunks_dropped_malformed: self.chunks_dropped_malformed,
            chunks_dropped_duplicate: self.chunks_dropped_duplicate,
            frames_dropped_metadata_conflict: self.frames_dropped_metadata_conflict,
        }
    }

    /// Drop pending frames whose first chunk arrived more than
    /// `self.timeout` ago. Returns the list of evicted frame_ids (for
    /// logging) and the count of chunks they had accumulated.
    pub fn sweep_timeouts(&mut self, now: Instant) -> Vec<DropReason> {
        let timeout = self.timeout;
        let mut dropped = Vec::new();
        self.pending.retain(|&frame_id, assembly| {
            let alive = now.duration_since(assembly.first_seen) < timeout;
            if !alive {
                dropped.push(DropReason::Timeout {
                    frame_id,
                    chunks_received: assembly.chunks_received,
                    total_chunks: assembly.total_chunks,
                });
            }
            alive
        });
        self.frames_dropped_timeout += dropped.len() as u64;
        dropped
    }

    /// Ingest one datagram. Returns:
    ///
    /// - `Progress` — chunk accepted, frame still pending
    /// - `Completed(...)` — all chunks present, JPEG ready
    /// - `Dropped(...)` — chunk or frame dropped (malformed,
    ///   duplicate, conflict, capacity-evicted)
    ///
    /// This call does not internally sweep timeouts; the caller is
    /// expected to drive `sweep_timeouts` on whatever cadence they
    /// want stale state cleared. The processor sweeps before each
    /// ingest so a quiet-stream sender that suddenly resumes doesn't
    /// see frames evicted only at the *next* arrival; a periodic-
    /// timer caller would sweep on the timer tick instead.
    ///
    /// Outcome priority when both eviction and completion fire in
    /// the same call (cap full + single-chunk new frame): `Completed`
    /// wins — the caller wants the JPEG downstream — and the
    /// eviction is only visible via `metrics().frames_dropped_capacity`.
    pub fn ingest(&mut self, datagram: &[u8], now: Instant) -> IngestOutcome {
        let (header, payload) = match header::parse(datagram) {
            Ok(parsed) => parsed,
            Err(err) => {
                self.chunks_dropped_malformed += 1;
                return IngestOutcome::Dropped(DropReason::HeaderError(err));
            }
        };

        // Cap check before insertion. If we'd be adding a brand-new
        // frame_id and we're already at the cap, evict the oldest.
        // Existing frame_ids don't hit the cap (we're updating in
        // place). We don't short-circuit here — the new chunk still
        // gets ingested below, and if it's a single-chunk frame the
        // Completed outcome takes priority over the eviction.
        let mut cap_evicted: Option<DropReason> = None;
        if !self.pending.contains_key(&header.frame_id)
            && self.pending.len() >= self.max_pending
            && let Some((_, oldest)) = self.evict_oldest()
        {
            self.frames_dropped_capacity += 1;
            cap_evicted = Some(DropReason::CapacityEvicted {
                frame_id: oldest.0,
                chunks_received: oldest.1,
                total_chunks: oldest.2,
            });
        }

        // From here, either the frame_id exists or we're inserting
        // fresh under the cap.
        if let Some(assembly) = self.pending.get_mut(&header.frame_id) {
            // Sanity-check per-frame metadata. Disagreements indicate
            // either a producer reset reusing the frame_id or wire
            // corruption — drop the whole pending frame so we don't
            // mix bytes from two different streams.
            if assembly.total_chunks != header.total_chunks {
                let existing = assembly.total_chunks;
                let frame_id = header.frame_id;
                self.pending.remove(&frame_id);
                self.frames_dropped_metadata_conflict += 1;
                return IngestOutcome::Dropped(DropReason::MetadataConflict {
                    frame_id,
                    field: "total_chunks",
                    existing: existing as u64,
                    incoming: header.total_chunks as u64,
                });
            }
            if assembly.jpeg_size != header.jpeg_size {
                let existing = assembly.jpeg_size;
                let frame_id = header.frame_id;
                self.pending.remove(&frame_id);
                self.frames_dropped_metadata_conflict += 1;
                return IngestOutcome::Dropped(DropReason::MetadataConflict {
                    frame_id,
                    field: "jpeg_size",
                    existing: existing as u64,
                    incoming: header.jpeg_size as u64,
                });
            }

            let slot = &mut assembly.chunks[header.chunk_id as usize];
            if slot.is_some() {
                self.chunks_dropped_duplicate += 1;
                return IngestOutcome::Dropped(DropReason::DuplicateChunk {
                    frame_id: header.frame_id,
                    chunk_id: header.chunk_id,
                });
            }
            *slot = Some(payload.to_vec());
            assembly.chunks_received += 1;
            assembly.received_bytes = assembly
                .received_bytes
                .saturating_add(header.payload_size);

            if assembly.is_complete() {
                // Pull the entry out and assemble.
                let completed = self
                    .pending
                    .remove(&header.frame_id)
                    .expect("checked above")
                    .assemble(header.frame_id);
                self.frames_emitted += 1;
                return IngestOutcome::Completed(completed);
            }
            return IngestOutcome::Progress;
        }

        // Fresh frame_id under cap (or under cap after the eviction above).
        self.start_new_assembly(&header, payload, now);
        // A single-chunk frame is complete immediately. When this fires
        // in the same call as a cap eviction, Completed takes priority
        // over the CapacityEvicted DropReason — the eviction stays
        // visible via the metrics counter.
        if let Some(assembly) = self.pending.get(&header.frame_id)
            && assembly.is_complete()
        {
            let completed = self
                .pending
                .remove(&header.frame_id)
                .expect("just inserted")
                .assemble(header.frame_id);
            self.frames_emitted += 1;
            return IngestOutcome::Completed(completed);
        }
        match cap_evicted {
            Some(reason) => IngestOutcome::Dropped(reason),
            None => IngestOutcome::Progress,
        }
    }

    fn start_new_assembly(&mut self, header: &ChunkHeader, payload: &[u8], now: Instant) {
        let mut assembly = FrameAssembly::new(header, now);
        assembly.chunks[header.chunk_id as usize] = Some(payload.to_vec());
        assembly.chunks_received = 1;
        assembly.received_bytes = header.payload_size;
        self.pending.insert(header.frame_id, assembly);
    }

    /// Returns the (frame_id, chunks_received, total_chunks) of the
    /// evicted entry. None if the table was empty.
    fn evict_oldest(&mut self) -> Option<(u32, (u32, u16, u16))> {
        let oldest_id = self
            .pending
            .iter()
            .min_by_key(|(_, a)| a.first_seen)
            .map(|(id, _)| *id)?;
        let evicted = self.pending.remove(&oldest_id)?;
        Some((
            oldest_id,
            (oldest_id, evicted.chunks_received, evicted.total_chunks),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::encode;

    fn header(
        frame_id: u32,
        chunk_id: u16,
        total_chunks: u16,
        jpeg_size: u32,
        payload_size: u32,
        sim_time_ns: u64,
    ) -> ChunkHeader {
        ChunkHeader {
            frame_id,
            chunk_id,
            total_chunks,
            jpeg_size,
            payload_size,
            sim_time_ns,
        }
    }

    /// Build the on-wire datagram bytes for a chunk with the given
    /// header + chunk payload. Wrapper around `header::encode` so test
    /// cases stay tight.
    fn datagram(h: ChunkHeader, payload: &[u8]) -> Vec<u8> {
        encode(&h, payload)
    }

    fn make_state() -> DepayloaderState {
        DepayloaderState::new(DEFAULT_TIMEOUT, DEFAULT_MAX_PENDING)
    }

    /// A single-chunk frame (total_chunks = 1) completes on first
    /// arrival. This locks the fast path for tiny JPEGs that fit in one
    /// MTU — the spec rate at 640×360 is well within 1 datagram for
    /// low-quality frames.
    #[test]
    fn single_chunk_frame_completes_immediately() {
        let mut state = make_state();
        let now = Instant::now();
        let payload = b"\xFF\xD8\xFF\xD9"; // SOI + EOI = minimal JPEG byte pattern
        let h = header(1, 0, 1, payload.len() as u32, payload.len() as u32, 12345);

        let outcome = state.ingest(&datagram(h, payload), now);
        match outcome {
            IngestOutcome::Completed(frame) => {
                assert_eq!(frame.frame_id, 1);
                assert_eq!(frame.sim_time_ns, 12345);
                assert_eq!(frame.data, payload);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(state.pending_len(), 0);
        assert_eq!(state.metrics().frames_emitted, 1);
    }

    /// In-order arrival of chunks [0, 1, 2] for a 3-chunk frame.
    /// Asserts the reassembled bytes are exactly the concatenation in
    /// chunk_id order.
    #[test]
    fn three_chunk_frame_in_order_reassembles_byte_exact() {
        let mut state = make_state();
        let now = Instant::now();
        let chunks: Vec<&[u8]> = vec![b"AAAA", b"BBBB", b"CCCC"];
        let jpeg_size: u32 = chunks.iter().map(|c| c.len() as u32).sum();

        let mut completed = None;
        for (i, c) in chunks.iter().enumerate() {
            let h = header(7, i as u16, 3, jpeg_size, c.len() as u32, 42);
            match state.ingest(&datagram(h, c), now) {
                IngestOutcome::Progress => {}
                IngestOutcome::Completed(frame) => completed = Some(frame),
                other => panic!("unexpected outcome for chunk {i}: {other:?}"),
            }
        }

        let frame = completed.expect("frame should complete after chunk 2");
        assert_eq!(frame.frame_id, 7);
        assert_eq!(frame.sim_time_ns, 42);
        assert_eq!(frame.data, b"AAAABBBBCCCC");
        assert_eq!(state.pending_len(), 0);
    }

    /// Out-of-order arrival of chunks [2, 0, 1] for a 3-chunk frame.
    /// Locks that the assembler positions by `chunk_id`, not by
    /// arrival order — the reassembled bytes must still be in
    /// chunk_id order.
    #[test]
    fn three_chunk_frame_out_of_order_reassembles_byte_exact() {
        let mut state = make_state();
        let now = Instant::now();
        let pieces: [(u16, &[u8]); 3] = [(2, b"CCCC"), (0, b"AAAA"), (1, b"BBBB")];
        let jpeg_size: u32 = pieces.iter().map(|(_, c)| c.len() as u32).sum();

        let mut completed = None;
        for (chunk_id, c) in pieces {
            let h = header(9, chunk_id, 3, jpeg_size, c.len() as u32, 99);
            match state.ingest(&datagram(h, c), now) {
                IngestOutcome::Progress => {}
                IngestOutcome::Completed(frame) => completed = Some(frame),
                other => panic!("unexpected outcome at chunk {chunk_id}: {other:?}"),
            }
        }
        assert_eq!(completed.unwrap().data, b"AAAABBBBCCCC");
    }

    /// A duplicate chunk_id arrival is rejected without disturbing the
    /// pending state — the second `Dropped(DuplicateChunk)` outcome
    /// must NOT remove the entry or corrupt the slot.
    #[test]
    fn duplicate_chunk_drops_without_corrupting_state() {
        let mut state = make_state();
        let now = Instant::now();
        let h0 = header(3, 0, 2, 8, 4, 0);
        let h1 = header(3, 1, 2, 8, 4, 0);

        assert!(matches!(
            state.ingest(&datagram(h0, b"AAAA"), now),
            IngestOutcome::Progress
        ));
        // Resend chunk 0 with different bytes — must be rejected.
        match state.ingest(&datagram(h0, b"XXXX"), now) {
            IngestOutcome::Dropped(DropReason::DuplicateChunk { frame_id, chunk_id }) => {
                assert_eq!(frame_id, 3);
                assert_eq!(chunk_id, 0);
            }
            other => panic!("expected DuplicateChunk, got {other:?}"),
        }
        // Finalize with chunk 1 — must still yield AAAA in slot 0, not XXXX.
        match state.ingest(&datagram(h1, b"BBBB"), now) {
            IngestOutcome::Completed(frame) => {
                assert_eq!(frame.data, b"AAAABBBB");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(state.metrics().chunks_dropped_duplicate, 1);
    }

    /// Two frames in flight at once. Chunks arrive interleaved:
    /// 1,A → 2,A → 1,B → 2,B. Frame 1 completes when its B arrives;
    /// frame 2 completes when its B arrives. Both reassemble correctly.
    #[test]
    fn two_frames_interleaved_complete_independently() {
        let mut state = make_state();
        let now = Instant::now();

        let h1a = header(1, 0, 2, 8, 4, 100);
        let h2a = header(2, 0, 2, 8, 4, 200);
        let h1b = header(1, 1, 2, 8, 4, 100);
        let h2b = header(2, 1, 2, 8, 4, 200);

        assert!(matches!(
            state.ingest(&datagram(h1a, b"1AAA"), now),
            IngestOutcome::Progress
        ));
        assert!(matches!(
            state.ingest(&datagram(h2a, b"2AAA"), now),
            IngestOutcome::Progress
        ));

        let frame1 = match state.ingest(&datagram(h1b, b"1BBB"), now) {
            IngestOutcome::Completed(f) => f,
            other => panic!("expected Completed for frame 1, got {other:?}"),
        };
        assert_eq!(frame1.frame_id, 1);
        assert_eq!(frame1.sim_time_ns, 100);
        assert_eq!(frame1.data, b"1AAA1BBB");
        assert_eq!(state.pending_len(), 1, "frame 2 still in flight");

        let frame2 = match state.ingest(&datagram(h2b, b"2BBB"), now) {
            IngestOutcome::Completed(f) => f,
            other => panic!("expected Completed for frame 2, got {other:?}"),
        };
        assert_eq!(frame2.frame_id, 2);
        assert_eq!(frame2.sim_time_ns, 200);
        assert_eq!(frame2.data, b"2AAA2BBB");
        assert_eq!(state.pending_len(), 0);
    }

    /// A frame whose first chunk arrives at T=0 and whose remaining
    /// chunks never arrive must be evicted by `sweep_timeouts` after
    /// the configured timeout. The HashMap must not grow under
    /// sustained loss — that's the AI-Agent-Note's gnarly bit.
    #[test]
    fn timeout_eviction_removes_incomplete_frame_no_leak() {
        let mut state = DepayloaderState::new(Duration::from_millis(50), 8);
        let start = Instant::now();
        let h = header(99, 0, 4, 16, 4, 7);
        assert!(matches!(
            state.ingest(&datagram(h, b"ZZZZ"), start),
            IngestOutcome::Progress
        ));
        assert_eq!(state.pending_len(), 1);

        // Just before timeout — entry survives.
        let dropped = state.sweep_timeouts(start + Duration::from_millis(20));
        assert!(dropped.is_empty());
        assert_eq!(state.pending_len(), 1);

        // Just after timeout — entry is evicted.
        let dropped = state.sweep_timeouts(start + Duration::from_millis(60));
        assert_eq!(dropped.len(), 1);
        match &dropped[0] {
            DropReason::Timeout {
                frame_id,
                chunks_received,
                total_chunks,
            } => {
                assert_eq!(*frame_id, 99);
                assert_eq!(*chunks_received, 1);
                assert_eq!(*total_chunks, 4);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
        assert_eq!(state.pending_len(), 0);
        assert_eq!(state.metrics().frames_dropped_timeout, 1);
    }

    /// Sustained-loss leak guard: ingest 1000 distinct frame_ids,
    /// each with one chunk of a 4-chunk frame, advance time past
    /// the timeout, and assert the table is empty. Without the
    /// sweep, this would grow unbounded.
    #[test]
    fn sustained_loss_does_not_leak_pending_table() {
        let timeout = Duration::from_millis(10);
        let mut state = DepayloaderState::new(timeout, 1000);
        let mut now = Instant::now();
        for fid in 0u32..1000 {
            let h = header(fid, 0, 4, 16, 4, 0);
            state.ingest(&datagram(h, b"AAAA"), now);
            now += Duration::from_micros(10); // Stagger so first_seen differs.
        }
        assert_eq!(state.pending_len(), 1000);
        // Jump past the timeout.
        let after = now + timeout + Duration::from_millis(1);
        let dropped = state.sweep_timeouts(after);
        assert_eq!(dropped.len(), 1000);
        assert_eq!(state.pending_len(), 0);
        assert_eq!(state.metrics().frames_dropped_timeout, 1000);
    }

    /// Corner case: when the pending table is at capacity AND the
    /// incoming chunk is itself a single-chunk frame (`total_chunks = 1`),
    /// the new frame is BOTH eviction-triggering AND immediately
    /// complete. Outcome priority: `Completed` wins (the caller wants
    /// the JPEG downstream); the eviction is only visible via the
    /// `frames_dropped_capacity` metrics counter. Without the priority
    /// rule, the complete frame would sit in pending until timeout —
    /// silently delaying a frame that's already done.
    #[test]
    fn capacity_eviction_plus_single_chunk_frame_completes_priority() {
        let mut state = DepayloaderState::new(Duration::from_secs(60), 2);
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(1);
        let t2 = t1 + Duration::from_millis(1);

        // Fill cap with two multi-chunk frames.
        let h1_c0 = header(1, 0, 4, 16, 4, 0);
        let h2_c0 = header(2, 0, 4, 16, 4, 0);
        state.ingest(&datagram(h1_c0, b"1111"), t0);
        state.ingest(&datagram(h2_c0, b"2222"), t1);
        assert_eq!(state.pending_len(), 2);

        // Single-chunk frame 3 arrives at cap. Expected: Completed
        // (priority over eviction), eviction visible via counter.
        let h3 = header(3, 0, 1, 4, 4, 999);
        let bytes = b"3333";
        let outcome = state.ingest(&datagram(h3, bytes), t2);
        match outcome {
            IngestOutcome::Completed(frame) => {
                assert_eq!(frame.frame_id, 3);
                assert_eq!(frame.sim_time_ns, 999);
                assert_eq!(frame.data, bytes);
            }
            other => panic!("expected Completed (priority over eviction), got {other:?}"),
        }
        let metrics = state.metrics();
        assert_eq!(metrics.frames_emitted, 1);
        assert_eq!(metrics.frames_dropped_capacity, 1, "eviction still counted");
        // After: one entry left (frame 2 — frame 1 was evicted, frame 3 emitted).
        assert_eq!(state.pending_len(), 1);
    }

    /// Capacity eviction: with `max_pending = 2`, ingesting chunks
    /// for three distinct frame_ids must evict the oldest. The
    /// counter increments and the new chunk is accepted.
    #[test]
    fn capacity_eviction_drops_oldest_keeps_newest() {
        let mut state = DepayloaderState::new(Duration::from_secs(60), 2);
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(1);
        let t2 = t1 + Duration::from_millis(1);

        let h1 = header(1, 0, 4, 16, 4, 0);
        let h2 = header(2, 0, 4, 16, 4, 0);
        let h3 = header(3, 0, 4, 16, 4, 0);

        state.ingest(&datagram(h1, b"1111"), t0);
        state.ingest(&datagram(h2, b"2222"), t1);

        match state.ingest(&datagram(h3, b"3333"), t2) {
            IngestOutcome::Dropped(DropReason::CapacityEvicted {
                frame_id,
                chunks_received,
                total_chunks,
            }) => {
                assert_eq!(frame_id, 1, "oldest (frame 1) must be evicted");
                assert_eq!(chunks_received, 1);
                assert_eq!(total_chunks, 4);
            }
            other => panic!("expected CapacityEvicted for frame 3, got {other:?}"),
        }
        // After eviction, table holds frames 2 and 3.
        assert_eq!(state.pending_len(), 2);
        assert_eq!(state.metrics().frames_dropped_capacity, 1);
    }

    /// A chunk for an existing frame_id whose `total_chunks` field
    /// disagrees with the first-chunk's value must drop the whole
    /// pending frame. Documents the "treat producer-reset as fatal
    /// for the in-flight frame" rule.
    #[test]
    fn metadata_conflict_drops_pending_frame() {
        let mut state = make_state();
        let now = Instant::now();
        let h_first = header(5, 0, 3, 12, 4, 0);
        let h_conflict = header(5, 1, 5, 20, 4, 0); // different total_chunks AND jpeg_size

        state.ingest(&datagram(h_first, b"AAAA"), now);
        assert_eq!(state.pending_len(), 1);

        match state.ingest(&datagram(h_conflict, b"BBBB"), now) {
            IngestOutcome::Dropped(DropReason::MetadataConflict { frame_id, field, .. }) => {
                assert_eq!(frame_id, 5);
                // total_chunks is checked first, so that's the one
                // reported. Either is acceptable from a correctness
                // standpoint; locking the field name makes the test
                // diagnose-able if the check order is ever reordered.
                assert_eq!(field, "total_chunks");
            }
            other => panic!("expected MetadataConflict, got {other:?}"),
        }
        assert_eq!(state.pending_len(), 0, "pending frame must be evicted");
        assert_eq!(state.metrics().frames_dropped_metadata_conflict, 1);
    }

    /// A malformed datagram (too short) is counted under
    /// `chunks_dropped_malformed` and does not affect any pending
    /// frame's state.
    #[test]
    fn malformed_datagram_counted_no_state_change() {
        let mut state = make_state();
        let now = Instant::now();
        let h = header(1, 0, 2, 8, 4, 0);
        state.ingest(&datagram(h, b"AAAA"), now);
        assert_eq!(state.pending_len(), 1);

        // Datagram with no header bytes at all.
        match state.ingest(&[], now) {
            IngestOutcome::Dropped(DropReason::HeaderError(_)) => {}
            other => panic!("expected HeaderError, got {other:?}"),
        }
        assert_eq!(state.metrics().chunks_dropped_malformed, 1);
        assert_eq!(state.pending_len(), 1, "pending frame untouched");
    }

    /// Reassembled byte-exactness against a 1 KiB pseudo-random
    /// payload split across 7 chunks of unequal sizes (last chunk
    /// shorter). Locks: chunk slicing, payload_size != fixed-MTU,
    /// concatenation order, jpeg_size accounting.
    #[test]
    fn byte_exact_reassembly_with_unequal_chunks() {
        let mut state = make_state();
        let now = Instant::now();

        // Deterministic 1037-byte "JPEG" — not real JPEG bytes, but the
        // depayloader treats them as opaque.
        let total: usize = 1037;
        let source: Vec<u8> = (0..total).map(|i| (i as u32).wrapping_mul(31) as u8).collect();
        let chunk_size = 150;
        let pieces: Vec<&[u8]> = source.chunks(chunk_size).collect();
        let total_chunks: u16 = pieces.len() as u16;
        let jpeg_size: u32 = total as u32;
        assert!(total_chunks >= 7, "test wants ≥ 7 chunks");

        let mut completed = None;
        for (i, piece) in pieces.iter().enumerate() {
            let h = header(
                42,
                i as u16,
                total_chunks,
                jpeg_size,
                piece.len() as u32,
                99,
            );
            match state.ingest(&datagram(h, piece), now) {
                IngestOutcome::Progress => {}
                IngestOutcome::Completed(frame) => completed = Some(frame),
                other => panic!("unexpected outcome at chunk {i}: {other:?}"),
            }
        }

        let frame = completed.expect("frame should complete on last chunk");
        assert_eq!(frame.data.len(), total);
        assert_eq!(frame.data, source, "byte-exact reassembly");
        assert_eq!(state.pending_len(), 0);
    }
}
