// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase H (#1005) dlopen-cdylib panic-injection lifecycle fixtures.
//!
//! Two processor types, one Manual (covers `setup` / `start` / `stop` /
//! `teardown` / `on_pause` / `on_resume`) and one Continuous (covers
//! `process`, the slot Manual doesn't expose). Each carries a
//! `panic_at_hook` config field that names the hook to panic in; all
//! other hooks no-op. The companion integration test
//! (`runtime/streamlib-engine/tests/load_project_dylib_processor_panic_safety.rs`)
//! drives each variant in turn and asserts the host's
//! `run_host_extern_c` panic-safety net caught the panic at the cdylib
//! DSO boundary (i.e. the runtime stayed alive instead of unwinding
//! into the host's runtime thread).
//!
//! Why two fixtures: the cdylib-reachable `ProcessorVTable` carries
//! all seven hooks (`setup`, `teardown`, `on_pause`, `on_resume`,
//! `process`, `start`, `stop`) but a single `Manual` trait impl
//! delivers only six of them (no `process`) and a single `Continuous`
//! trait impl delivers only five (no `start` / `stop`). Splitting the
//! fixture in two covers all seven canonical slots without needing a
//! synthetic trait merge.
//!
//! What this fixture locks: any regression that lets a panic inside a
//! cdylib processor hook escape the FFI boundary into the host (rather
//! than being absorbed by `run_host_extern_c`) surfaces here as a
//! crashed test binary instead of a clean "host stayed alive"
//! assertion. The wrapper's `catch_unwind` is the engine-tier
//! invariant; mentally reverting it would abort the test process.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{ContinuousProcessor, ManualProcessor};

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/PanickingManualLifecycleProcessor",
    description = "Phase H (#1005) dlopen-cdylib panic-injection Manual fixture. Panics in the configured lifecycle hook (setup / start / stop / teardown / on_pause / on_resume); the host's run_host_extern_c panic-safety net is expected to absorb the panic and keep the runtime alive.",
    execution = manual,
    config = crate::_generated_::PanickingManualLifecycleProcessorConfig,
)]
pub struct PanickingManualLifecycle {}

impl ManualProcessor for PanickingManualLifecycle::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "setup" {
            panic!("PanickingManualLifecycle: injected panic at setup");
        }
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "start" {
            panic!("PanickingManualLifecycle: injected panic at start");
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "stop" {
            panic!("PanickingManualLifecycle: injected panic at stop");
        }
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "teardown" {
            panic!("PanickingManualLifecycle: injected panic at teardown");
        }
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "on_pause" {
            panic!("PanickingManualLifecycle: injected panic at on_pause");
        }
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "on_resume" {
            panic!("PanickingManualLifecycle: injected panic at on_resume");
        }
        Ok(())
    }
}

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/PanickingContinuousLifecycleProcessor",
    description = "Phase H (#1005) dlopen-cdylib panic-injection Continuous fixture. Panics in the configured lifecycle hook (process); the host's run_host_extern_c panic-safety net is expected to absorb the panic and keep the runtime alive.",
    execution = continuous,
    config = crate::_generated_::PanickingContinuousLifecycleProcessorConfig,
)]
pub struct PanickingContinuousLifecycle {}

impl ContinuousProcessor for PanickingContinuousLifecycle::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.config.panic_at_hook == "process" {
            panic!("PanickingContinuousLifecycle: injected panic at process");
        }
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
