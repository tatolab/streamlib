// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::ContinuousNode;

/// The settings [`LifecycleProbe`] reads at setup.
#[streamlib::sdk::node_settings]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleProbeSettings {
    #[description = "How many runs of `process` append a line before it stops."]
    pub max_iterations: u32,
    #[description = "The file the node writes its result to."]
    pub output_path: String,
}

#[streamlib::sdk::node(
    execution = continuous,
    settings = crate::lifecycle_probe_node::LifecycleProbeSettings,
)]
#[meta(
    description = "Lifecycle-probe node — appends marker lines for each lifecycle hook (setup / process / on_pause / on_resume / teardown) to a file so the integration test can confirm every hook dispatched correctly."
)]
pub struct LifecycleProbe {
    iter_count: AtomicU32,
}

impl LifecycleProbe::Node {
    fn append_line(&self, line: &str) -> Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.settings.output_path)
            .map_err(|e| {
                Error::Runtime(format!(
                    "LifecycleProbe: open {}: {e}",
                    self.settings.output_path
                ))
            })?;
        writeln!(f, "{line}").map_err(|e| {
            Error::Runtime(format!(
                "LifecycleProbe: write {}: {e}",
                self.settings.output_path
            ))
        })?;
        Ok(())
    }
}

impl ContinuousNode for LifecycleProbe::Node {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.append_line("SETUP")
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let n = self.iter_count.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.settings.max_iterations {
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
