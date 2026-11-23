// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::{ContinuousNode, ManualNode};

/// The settings [`PanickingManualLifecycle`] reads at setup.
#[streamlib::sdk::node_settings]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanickingManualLifecycleSettings {
    #[description = "The lifecycle hook that panics: `setup`, `start`, `stop`, `teardown`, `on_pause` or `on_resume`; any other name panics in none."]
    pub panic_at_hook: String,
}

#[streamlib::sdk::node(
    execution = manual,
    settings = crate::panicking_lifecycle_node::PanickingManualLifecycleSettings,
)]
#[meta(
    description = "Panic-injection Manual fixture. Panics in the lifecycle hook its `panic_at_hook` setting names (setup / start / stop / teardown / on_pause / on_resume); the host's panic-safety net is expected to absorb the panic and keep the runtime alive."
)]
pub struct PanickingManualLifecycle {}

impl ManualNode for PanickingManualLifecycle::Node {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "setup" {
            panic!("PanickingManualLifecycle: injected panic at setup");
        }
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "start" {
            panic!("PanickingManualLifecycle: injected panic at start");
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "stop" {
            panic!("PanickingManualLifecycle: injected panic at stop");
        }
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "teardown" {
            panic!("PanickingManualLifecycle: injected panic at teardown");
        }
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "on_pause" {
            panic!("PanickingManualLifecycle: injected panic at on_pause");
        }
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "on_resume" {
            panic!("PanickingManualLifecycle: injected panic at on_resume");
        }
        Ok(())
    }
}

/// The settings [`PanickingContinuousLifecycle`] reads at setup.
#[streamlib::sdk::node_settings]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanickingContinuousLifecycleSettings {
    #[description = "The lifecycle hook that panics: `process`; any other name panics in none."]
    pub panic_at_hook: String,
}

#[streamlib::sdk::node(
    execution = continuous,
    settings = crate::panicking_lifecycle_node::PanickingContinuousLifecycleSettings,
)]
#[meta(
    description = "Panic-injection Continuous fixture. Panics in the lifecycle hook its `panic_at_hook` setting names, only `process`; the host's panic-safety net is expected to absorb the panic and keep the runtime alive."
)]
pub struct PanickingContinuousLifecycle {}

impl ContinuousNode for PanickingContinuousLifecycle::Node {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.settings.panic_at_hook == "process" {
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
