// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase H (#1006 scenario 1) dlopen-cdylib concurrent-escalate test
//! fixture.
//!
//! Spawns `config.thread_count` threads from inside the cdylib's
//! `start()` lifecycle, each cloning `gpu_limited_access()` and
//! calling `escalate(|_full| ...)` concurrently. The escalate gate
//! is documented to serialize concurrent callers — overlapping
//! closures would be a regression. Each thread bumps a shared
//! atomic on closure entry; if the atomic was already set, that's
//! an overlap.
//!
//! Output format:
//!   - `OK\n<thread_count>\noverlaps=<N>` — every escalate closure
//!     ran without overlapping any other. Expected N=0.
//!   - `ERR:<message>` — a thread's escalate returned an error or
//!     panicked.
//!
//! Mirrors the in-process `test_escalate_serializes_concurrent_callers`
//! test (`runtime/streamlib-engine/src/core/context/gpu_context.rs`) but
//! drives the cdylib path through `escalate_via_vtable` — the
//! plugin ABI contract the audit flagged as previously uncovered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/ConcurrentEscalateTestProcessor@1.0.0",
    execution = manual,
    config = crate::_generated_::ConcurrentEscalateTestProcessorConfig,
)]
pub struct ConcurrentEscalateTest {}

impl ManualProcessor for ConcurrentEscalateTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // **Currently un-driveable end-to-end.** Per PR #1075,
        // `ProcessorInstance::start` wraps cdylib-resident Manual-
        // mode dispatch in `with_cdylib_scope`, which acquires the
        // escalate gate for the entire start body. The worker
        // threads spawned below try to acquire that same gate via
        // their `limited.escalate(...)` call and deadlock — start
        // blocks waiting for join, workers block waiting for the
        // gate, gate never releases. The integration test at
        // `runtime/streamlib-engine/tests/load_project_dylib_concurrent_escalate.rs`
        // is `#[ignore]`d as a result. The serialization invariant
        // this fixture was guarding is covered by the unit test
        // `escalate_gate::tests::enter_serializes_concurrent_callers`.
        // Restructuring to drive the concurrent escalates from a
        // Reactive `process()` body (LimitedAccess, no wrap) is the
        // natural follow-up.
        let output_path = self.config.output_path.clone();
        let thread_count = self.config.thread_count as usize;
        let hold_ms = self.config.hold_ms as u64;

        // Clone the LimitedAccess handle once; each spawned thread
        // gets its own clone (the Clone impl bumps the inner Arc
        // refcount via the FullAccess vtable's `clone_handle`).
        let limited_template = ctx.gpu_limited_access().clone();

        let in_closure = Arc::new(AtomicBool::new(false));
        let overlap_count = Arc::new(AtomicUsize::new(0));
        let completed_count = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                let limited = limited_template.clone();
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

        drop(limited_template);

        let line = match first_error {
            Some(msg) => format!("ERR:{msg}"),
            None => {
                let completed = completed_count.load(Ordering::SeqCst);
                let overlaps = overlap_count.load(Ordering::SeqCst);
                if completed != thread_count {
                    format!("ERR:completed {completed}/{thread_count} threads",)
                } else {
                    format!("OK\n{thread_count}\noverlaps={overlaps}")
                }
            }
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!("ConcurrentEscalateTest: write {output_path}: {e}"))
        })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}
