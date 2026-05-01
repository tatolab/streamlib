// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Counting sink for the polyglot-manual-source example (#604).
//!
//! Subscribes to a `Videoframe` input port via iceoryx2, counts every
//! frame received in `process()`, and writes a JSON stats file on
//! `teardown()`. The scenario binary reads the stats file post-stop to
//! verify the polyglot manual source actually published frames over
//! iceoryx2 — the goal #604 unlocks (replacing PR #602's file-based
//! placeholder verification).
//!
//! The output stats path is taken from the
//! `STREAMLIB_POLYGLOT_MANUAL_SOURCE_SINK_OUTPUT` env var (set by the
//! scenario binary). Using an env var sidesteps the JTD schema codegen
//! dance that an in-tree config field would need, while still letting
//! the scenario binary route per-runtime test runs to distinct files.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use streamlib::_generated_::Videoframe;
use streamlib::core::{
    Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamError,
};
use streamlib_plugin_abi::export_plugin;

const OUTPUT_ENV_VAR: &str = "STREAMLIB_POLYGLOT_MANUAL_SOURCE_SINK_OUTPUT";

#[streamlib::processor("com.tatolab.polyglot_manual_source_counting_sink")]
pub struct PolyglotManualSourceCountingSink {
    output_file: Option<PathBuf>,
    frame_counter: u64,
    first_ns: u64,
    last_ns: u64,
}

impl streamlib::core::ReactiveProcessor for PolyglotManualSourceCountingSink::Processor {
    fn setup(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let output = std::env::var(OUTPUT_ENV_VAR)
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        if output.is_none() {
            tracing::warn!(
                "[CountingSink] {OUTPUT_ENV_VAR} not set — teardown will skip writing stats"
            );
        }
        self.output_file = output;
        std::future::ready(Ok(()))
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;
        let ts: u64 = frame.timestamp_ns.parse().unwrap_or(0);
        if self.frame_counter == 0 {
            self.first_ns = ts;
        }
        self.last_ns = ts;
        self.frame_counter += 1;
        if self.frame_counter <= 3 || self.frame_counter % 30 == 0 {
            tracing::debug!(
                "[CountingSink] received frame {} ts_ns={}",
                self.frame_counter,
                ts
            );
        }
        Ok(())
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = match &self.output_file {
            Some(path) => {
                // JSON has no native u64 — emit timestamps as decimal
                // strings so the scenario binary's `parse_u64_or_string`
                // shape continues to work and Python/Deno-comparison
                // parity stays clean.
                let stats = serde_json::json!({
                    "frames_received": self.frame_counter,
                    "first_timestamp_ns": self.first_ns.to_string(),
                    "last_timestamp_ns": self.last_ns.to_string(),
                });
                std::fs::write(path, stats.to_string()).map_err(|e| {
                    StreamError::Runtime(format!(
                        "[CountingSink] failed to write stats to {}: {}",
                        path.display(),
                        e
                    ))
                })
            }
            None => Ok(()),
        };
        tracing::info!(
            "[CountingSink] teardown — frames_received={}",
            self.frame_counter,
        );
        std::future::ready(result)
    }
}

export_plugin!(PolyglotManualSourceCountingSink::Processor);
