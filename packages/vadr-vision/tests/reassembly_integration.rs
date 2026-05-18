// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
//
// Cross-module integration: drives the depayloader's public state
// machine API end-to-end with an AGP-representative chunked stream
// (~30 KB JPEG split across ~30 MTU-sized chunks at the spec's 640×360
// @ Q≈75 ballpark), validates byte-exact reassembly under both
// in-order and shuffled-order arrival, and locks the deferred-E2E
// boundary: this is the largest scenario we can test without a real
// `UdpSource` + `JpegDecoder` pairing.

use std::time::{Duration, Instant};

use streamlib_vadr_vision::header::{ChunkHeader, encode};
use streamlib_vadr_vision::reassembly::{
    CompletedFrame, DepayloaderState, IngestOutcome,
};

/// Build the bytes for one chunk of a frame given the frame_id +
/// chunk_id + total + sim_time_ns + slice of source bytes.
fn build_chunk_datagram(
    frame_id: u32,
    chunk_id: u16,
    total_chunks: u16,
    jpeg_size: u32,
    sim_time_ns: u64,
    payload: &[u8],
) -> Vec<u8> {
    let header = ChunkHeader {
        frame_id,
        chunk_id,
        total_chunks,
        jpeg_size,
        payload_size: payload.len() as u32,
        sim_time_ns,
    };
    encode(&header, payload)
}

/// AGP simulator @ 640×360 ≈ 30 KB JPEG payloads (per VADR-TS-002 §4.6
/// notes). With a 1400-byte effective payload per datagram (1500 MTU
/// minus IP/UDP/header overhead) that's ~22 chunks. Use a pseudo-
/// random byte stream as the "JPEG" — the depayloader treats payload
/// bytes opaquely so this catches every slicing/concatenation bug a
/// real JPEG would catch.
fn agp_scale_source() -> Vec<u8> {
    let total = 30_417; // not aligned to chunk boundary on purpose
    (0..total).map(|i| ((i * 73 + 11) % 251) as u8).collect()
}

fn chunked(source: &[u8], chunk_size: usize) -> Vec<&[u8]> {
    source.chunks(chunk_size).collect()
}

#[test]
fn agp_scale_in_order_reassembles_byte_exact() {
    let mut state = DepayloaderState::new(Duration::from_millis(200), 8);
    let now = Instant::now();
    let source = agp_scale_source();
    let pieces = chunked(&source, 1400);
    let total_chunks = pieces.len() as u16;
    assert!(
        total_chunks >= 20,
        "AGP-scale test needs ≥20 chunks, got {total_chunks}"
    );
    let jpeg_size = source.len() as u32;
    let sim_time_ns = 1_234_567_890u64;

    let mut completed: Option<CompletedFrame> = None;
    for (i, piece) in pieces.iter().enumerate() {
        let dgram =
            build_chunk_datagram(1, i as u16, total_chunks, jpeg_size, sim_time_ns, piece);
        match state.ingest(&dgram, now) {
            IngestOutcome::Progress => {}
            IngestOutcome::Completed(f) => completed = Some(f),
            other => panic!("unexpected outcome at chunk {i}: {other:?}"),
        }
    }

    let frame = completed.expect("frame must complete after final chunk");
    assert_eq!(frame.frame_id, 1);
    assert_eq!(frame.sim_time_ns, sim_time_ns);
    assert_eq!(frame.data, source, "reassembled bytes match source exactly");
    assert_eq!(
        state.pending_len(),
        0,
        "no pending state left after frame completes"
    );
    assert_eq!(state.metrics().frames_emitted, 1);
}

#[test]
fn agp_scale_shuffled_reassembles_byte_exact() {
    let mut state = DepayloaderState::new(Duration::from_millis(200), 8);
    let now = Instant::now();
    let source = agp_scale_source();
    let pieces = chunked(&source, 1400);
    let total_chunks = pieces.len() as u16;
    let jpeg_size = source.len() as u32;

    // Deterministic shuffle — reverse halves around the midpoint. Not
    // a uniform random permutation, but enough non-monotonic arrival
    // to exercise the slot indexing under out-of-order conditions.
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    let half = order.len() / 2;
    order[..half].reverse();
    order[half..].reverse();

    let mut completed: Option<CompletedFrame> = None;
    for (step, &i) in order.iter().enumerate() {
        let dgram = build_chunk_datagram(
            42,
            i as u16,
            total_chunks,
            jpeg_size,
            777,
            pieces[i],
        );
        match state.ingest(&dgram, now) {
            IngestOutcome::Progress => {}
            IngestOutcome::Completed(f) => completed = Some(f),
            other => panic!("unexpected outcome at step {step} (chunk {i}): {other:?}"),
        }
    }

    let frame = completed.expect("frame must complete after final chunk regardless of order");
    assert_eq!(frame.data, source, "byte-exact under shuffled arrival order");
}

/// Stress: 10 frames in flight at once, chunks arriving fully
/// interleaved across all 10. Every frame completes; reassembly
/// order is independent across frame_ids; the pending table never
/// exceeds 10 (well within default cap of 8 → bump cap explicitly
/// for this test).
#[test]
fn ten_frames_interleaved_all_complete_byte_exact() {
    let mut state = DepayloaderState::new(Duration::from_millis(500), 16);
    let now = Instant::now();

    let frame_count: u32 = 10;
    let chunks_per_frame: u16 = 5;
    // Each frame has its own deterministic byte signature so cross-
    // frame mixing would be caught.
    let mut sources: Vec<Vec<u8>> = Vec::new();
    for fid in 0..frame_count {
        let mut bytes = Vec::with_capacity(chunks_per_frame as usize * 16);
        for chunk in 0..chunks_per_frame as usize {
            for b in 0..16 {
                // Encode (frame_id, chunk_id, byte_idx) into the byte
                // value so any cross-contamination shows up as a
                // mismatch in the assertion below.
                let v = (fid * 1000 + (chunk as u32) * 17 + (b as u32) * 3) as u8;
                bytes.push(v);
            }
        }
        sources.push(bytes);
    }

    // Build the interleaved arrival schedule: for each chunk_id, walk
    // all frames in sequence. So order is (0,0), (0,1), ..., (0,9),
    // (1,0), (1,1), ..., (chunks_per_frame-1, 9).
    let mut completed: Vec<Option<CompletedFrame>> = vec![None; frame_count as usize];
    for chunk_id in 0..chunks_per_frame {
        for fid in 0..frame_count {
            let src = &sources[fid as usize];
            let total_bytes = src.len() as u32;
            let start = (chunk_id as usize) * 16;
            let end = start + 16;
            let dgram = build_chunk_datagram(
                fid,
                chunk_id,
                chunks_per_frame,
                total_bytes,
                fid as u64,
                &src[start..end],
            );
            match state.ingest(&dgram, now) {
                IngestOutcome::Progress => {}
                IngestOutcome::Completed(f) => {
                    completed[fid as usize] = Some(f);
                }
                other => panic!(
                    "unexpected outcome at frame {fid} chunk {chunk_id}: {other:?}"
                ),
            }
        }
    }

    for (i, c) in completed.iter().enumerate() {
        let f = c.as_ref().unwrap_or_else(|| panic!("frame {i} did not complete"));
        assert_eq!(f.frame_id, i as u32);
        assert_eq!(f.sim_time_ns, i as u64);
        assert_eq!(
            f.data,
            sources[i],
            "frame {i} byte-exact under 10-way interleaving"
        );
    }
    assert_eq!(state.pending_len(), 0);
    assert_eq!(state.metrics().frames_emitted, frame_count as u64);
}

/// Loss-recovery: send chunk 0 of frame 1 + chunk 0 of frame 2, time
/// passes, frame 1's remaining chunks never arrive. The sweep evicts
/// frame 1 at timeout; frame 2's remaining chunks arrive after the
/// sweep and complete normally. Locks: timeout doesn't cascade-kill
/// healthy in-flight frames.
#[test]
fn timeout_eviction_does_not_kill_healthy_in_flight_frames() {
    let timeout = Duration::from_millis(50);
    let mut state = DepayloaderState::new(timeout, 8);
    let t0 = Instant::now();

    // Frame 1: send chunk 0 only. Frame 2: send chunk 0 only.
    let h1_c0 = build_chunk_datagram(1, 0, 2, 8, 100, b"1AAA");
    let h2_c0 = build_chunk_datagram(2, 0, 2, 8, 200, b"2AAA");
    state.ingest(&h1_c0, t0);
    state.ingest(&h2_c0, t0);
    assert_eq!(state.pending_len(), 2);

    // Time passes past frame 1's timeout deadline, but frame 2 keeps
    // making progress just before. We push frame 2's first_seen
    // forward by re-arriving its chunk 0 — actually that'd be a
    // duplicate, not a refresh. So we just rely on first_seen for
    // both being t0, then evict at t0+60ms (past 50ms timeout): both
    // would evict together. To test the "only frame 1 dies" shape we
    // need different first_seens.
    let t1 = t0 + Duration::from_millis(40);
    // Refresh frame 2's first_seen by sending a NEW frame 2 entry —
    // not possible with the same frame_id without an eviction first.
    // Better: stagger the initial arrivals.
    let mut state = DepayloaderState::new(timeout, 8);
    state.ingest(&h1_c0, t0);
    state.ingest(&h2_c0, t1);
    assert_eq!(state.pending_len(), 2);

    let t_sweep = t0 + Duration::from_millis(60); // > 50ms past frame 1, < 50ms past frame 2
    let evicted = state.sweep_timeouts(t_sweep);
    assert_eq!(evicted.len(), 1, "only frame 1 should evict");
    assert_eq!(state.pending_len(), 1);

    // Now frame 2's remaining chunk arrives and completes normally.
    let h2_c1 = build_chunk_datagram(2, 1, 2, 8, 200, b"2BBB");
    let outcome = state.ingest(&h2_c1, t_sweep);
    let frame = match outcome {
        IngestOutcome::Completed(f) => f,
        other => panic!("expected Completed for frame 2, got {other:?}"),
    };
    assert_eq!(frame.frame_id, 2);
    assert_eq!(frame.data, b"2AAA2BBB");
    assert_eq!(state.pending_len(), 0);
}
