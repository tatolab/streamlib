// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lifecycle-probe processor fixture.
//!
//! ContinuousProcessor whose lifecycle hooks (`setup`, `process`,
//! `on_pause`, `on_resume`, `teardown`) each append a single marker
//! line to `config.output_path`. An integration test parses the file
//! to assert each hook dispatched correctly.
//!
//! `config.max_iterations` caps the number of `PROCESS:n` lines
//! the probe will append — once the counter hits the cap the
//! `process()` body becomes a no-op (still returns Ok). Keeps the
//! file size bounded so the test reads a stable known content
//! after a sleep.
//!
//! What this fixture locks: regressions in `process`, `on_pause`, or
//! `on_resume` dispatch either surface as missing marker lines or the
//! hooks fire on the wrong thread / wrong order. Smoke-only — the lines
//! are observed for presence, not pixel correctness or timing.

use std::sync::atomic::{AtomicU32, Ordering};

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ContinuousProcessor;

#[streamlib::sdk::processor(
    description = "Lifecycle-probe processor — appends marker lines for each lifecycle hook (setup / process / on_pause / on_resume / teardown) to a file so the integration test can confirm every hook dispatched correctly.",
    execution = continuous,
    config = crate::test_fixture_processor_configs::LifecycleProbeProcessorConfig,
)]
pub struct LifecycleProbe {
    iter_count: AtomicU32,
}

impl LifecycleProbe::Processor {
    fn append_line(&self, line: &str) -> Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.output_path)
            .map_err(|e| {
                Error::Runtime(format!(
                    "LifecycleProbe: open {}: {e}",
                    self.config.output_path
                ))
            })?;
        writeln!(f, "{line}").map_err(|e| {
            Error::Runtime(format!(
                "LifecycleProbe: write {}: {e}",
                self.config.output_path
            ))
        })?;
        Ok(())
    }
}

impl ContinuousProcessor for LifecycleProbe::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.append_line("SETUP")
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let n = self.iter_count.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.config.max_iterations {
            // Stop appending once the cap is hit so the test reads
            // a stable file. The runtime keeps calling process()
            // until shutdown — that's fine; we just no-op.
            return Ok(());
        }
        self.append_line(&format!("PROCESS:{n}"))
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.append_line("PAUSE")
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.append_line("RESUME")
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.append_line("TEARDOWN")
    }
}
