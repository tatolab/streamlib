// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node written as one function that appends a line to the file its `output_path` names for
//! each value its computed value builds (`BUILT:<max_iterations>`), each one dropped
//! (`DROPPED:<max_iterations>`) and each frame up to `max_iterations` (`FRAME:<n>`), so an
//! integration test can confirm what replaces the lifecycle hooks: a value set live builds once
//! more and drops the value it replaces, and removing the node or stopping its stream drops
//! every value it built.

use std::io::Write;

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::nodes::{Input, computed};

/// The computed value: the file a probe appends to and the frames it appends, recording its
/// build and its drop in the file.
#[derive(Debug, PartialEq, Eq)]
pub struct LifecycleProbeFile {
    output_path: String,
    max_iterations: u32,
}

impl LifecycleProbeFile {
    /// Open the probe file at `output_path`, appending `BUILT:<max_iterations>`.
    fn built(output_path: &str, max_iterations: u32) -> Result<Self> {
        let probe_file = Self {
            output_path: output_path.to_owned(),
            max_iterations,
        };
        probe_file.append_line(&format!("BUILT:{max_iterations}"))?;
        Ok(probe_file)
    }

    fn append_line(&self, line: &str) -> Result<()> {
        let output_path = &self.output_path;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_path)
            .map_err(|why| Error::Runtime(format!("LifecycleProbe: open {output_path}: {why}")))?;
        writeln!(file, "{line}")
            .map_err(|why| Error::Runtime(format!("LifecycleProbe: write {output_path}: {why}")))
    }
}

impl Drop for LifecycleProbeFile {
    fn drop(&mut self) {
        // A drop cannot fail; a line it could not write shows as one missing from the file.
        let _ = self.append_line(&format!("DROPPED:{}", self.max_iterations));
    }
}

#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    input("max_iterations", description = "How many frames append a line.", value = u32, default = 0),
    input("output_path", description = "The file the node appends its lines to.", value = String, default = String::new()),
)]
#[meta(
    description = "Lifecycle-probe node — appends a line to a file for each value its computed value builds and drops, and for each frame up to max_iterations, so an integration test can confirm a live set rebuilds only that value and a removal or a stop drops every value it built."
)]
pub fn lifecycle_probe(
    max_iterations: Input<u32>,
    output_path: Input<String>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let probe_file = computed(move |_build_ctx| {
        LifecycleProbeFile::built(&output_path.get(), *max_iterations.get())
    });
    let mut frames_appended = 0u32;
    move |_ctx| {
        let probe_file = probe_file.get();
        if frames_appended >= probe_file.max_iterations {
            // Stop appending once the cap is hit so the test reads a stable file; frames run
            // on until the node goes.
            return Ok(());
        }
        frames_appended += 1;
        probe_file.append_line(&format!("FRAME:{frames_appended}"))
    }
}
