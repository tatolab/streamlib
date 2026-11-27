// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Concurrent-escalate test fixture.
//!
//! Its computed value spawns `thread_count` threads, each cloning
//! `gpu_limited_access()` and calling `escalate(|_full| ...)`
//! concurrently. The escalate gate is documented to serialize
//! concurrent callers — overlapping closures would be a regression.
//! Each thread bumps a shared atomic on closure entry; if the atomic
//! was already set, that's an overlap.
//!
//! Output format:
//!   - `OK\n<thread_count>\noverlaps=<N>` — every escalate closure
//!     ran without overlapping any other. Expected N=0.
//!   - `ERR:<message>` — a thread's escalate returned an error or
//!     panicked.
//!
//! Mirrors the in-process `test_escalate_serializes_concurrent_callers`
//! test (`runtime/streamlib-engine/src/core/context/gpu_context/tests/access_tiers_and_escalation.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::{Input, computed};

use crate::result_written_to_its_file::the_result_written_to_its_file;

#[streamlib::sdk::node(
    execution = manual,
    input("hold_ms", description = "How long each thread holds the escalate gate, in milliseconds.", value = u32, default = 0),
    input("output_path", description = "The file the node writes its result to.", value = String, default = String::new()),
    input("thread_count", description = "How many threads escalate at once.", value = u32, default = 0),
)]
#[meta(
    description = "Concurrent-escalate fixture. Its computed value spawns thread_count threads; each clones gpu_limited_access() and calls escalate concurrently. Output captures overlap count; expected overlaps=0 — proves the escalate gate serializes concurrent callers."
)]
pub fn concurrent_escalate_test(
    hold_ms: Input<u32>,
    output_path: Input<String>,
    thread_count: Input<u32>,
    ctx: &RuntimeContextFullAccess<'_>,
) {
    // Orphaned end-to-end: the integration test that drove this
    // fixture was removed, so nothing exercises it. The
    // serialization invariant it guarded is covered by the unit test
    // `escalate_gate::tests::enter_serializes_concurrent_callers`.
    let gpu = ctx.gpu_limited_access().clone();
    computed(move |_build_ctx| {
        let outcome = escalate_from_threads_at_once(
            &gpu,
            *thread_count.get() as usize,
            u64::from(*hold_ms.get()),
        );
        the_result_written_to_its_file("ConcurrentEscalateTest", &output_path.get(), &outcome)
    });
}

/// Escalate through `gpu` from `thread_count` threads at once, each holding the gate `hold_ms`:
/// the thread count and how many escalate closures overlapped another.
fn escalate_from_threads_at_once(
    gpu: &GpuContextLimitedAccess,
    thread_count: usize,
    hold_ms: u64,
) -> Result<Vec<String>> {
    let in_closure = Arc::new(AtomicBool::new(false));
    let overlap_count = Arc::new(AtomicUsize::new(0));
    let completed_count = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..thread_count)
        .map(|_| {
            // Each spawned thread holds its own clone; the clone bumps the inner Arc's count.
            let limited = gpu.clone();
            let in_closure = Arc::clone(&in_closure);
            let overlap_count = Arc::clone(&overlap_count);
            let completed_count = Arc::clone(&completed_count);
            std::thread::spawn(move || -> Result<()> {
                limited.escalate(|_full| -> Result<()> {
                    if in_closure.swap(true, Ordering::SeqCst) {
                        overlap_count.fetch_add(1, Ordering::SeqCst);
                    }
                    // Hold the gate for a small window so
                    // overlapping callers have a real chance to
                    // race in.
                    std::thread::sleep(Duration::from_millis(hold_ms));
                    in_closure.store(false, Ordering::SeqCst);
                    completed_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
        })
        .collect();

    let mut first_error: Option<String> = None;
    for h in handles {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_error.is_none() {
                    first_error = Some(format!("escalate failed: {e}"));
                }
            }
            Err(_) => {
                if first_error.is_none() {
                    first_error = Some("thread panicked".into());
                }
            }
        }
    }

    match first_error {
        Some(msg) => Err(Error::Runtime(msg)),
        None => {
            let completed = completed_count.load(Ordering::SeqCst);
            let overlaps = overlap_count.load(Ordering::SeqCst);
            if completed != thread_count {
                Err(Error::Runtime(format!(
                    "completed {completed}/{thread_count} threads"
                )))
            } else {
                Ok(vec![
                    thread_count.to_string(),
                    format!("overlaps={overlaps}"),
                ])
            }
        }
    }
}
