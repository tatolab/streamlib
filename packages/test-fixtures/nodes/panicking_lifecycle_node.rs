// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::{ContinuousNode, ManualNode};

#[streamlib::sdk::node(
    execution = manual,
    config = crate::test_fixture_node_configs::PanickingManualLifecycleNodeConfig,
)]
#[meta(
    description = "Panic-injection Manual fixture. Panics in the configured lifecycle hook (setup / start / stop / teardown / on_pause / on_resume); the host's panic-safety net is expected to absorb the panic and keep the runtime alive."
)]
pub struct PanickingManualLifecycle {}

impl ManualNode for PanickingManualLifecycle::Node {
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

#[streamlib::sdk::node(
    execution = continuous,
    config = crate::test_fixture_node_configs::PanickingContinuousLifecycleNodeConfig,
)]
#[meta(
    description = "Panic-injection Continuous fixture. Panics in the configured lifecycle hook (process); the host's panic-safety net is expected to absorb the panic and keep the runtime alive."
)]
pub struct PanickingContinuousLifecycle {}

impl ContinuousNode for PanickingContinuousLifecycle::Node {
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
