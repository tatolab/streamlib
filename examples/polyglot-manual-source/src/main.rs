// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot manual source reference example (issue #542).
//!
//! Exercises the `execution: manual` worker-thread / async-loop idiom
//! in both the Python and Deno SDKs.
//!
//! The polyglot processor's `start()` spawns a worker (thread on
//! Python, async IIFE on Deno), the worker uses `MonotonicTimer` to
//! pace tick-rate frame emission against `clock_gettime(CLOCK_MONOTONIC)`,
//! and each tick atomically writes an incrementing frame counter into
//! a host-visible output file. After the runtime stops, this binary
//! reads the file and asserts the counter is at least the expected
//! minimum — proving (a) the worker ran, (b) `start()` returned
//! promptly so lifecycle messages could land, (c) `MonotonicTimer`
//! paced correctly.
//!
//! See the per-runtime processor module docs for why a file and not
//! an iceoryx2 output port: concurrent escalate-IPC from worker
//! threads is out of scope under the current bridge protocol.
//!
//! Build the Python `.slpkg` first (Deno doesn't need a pack step):
//!   cargo run -p streamlib-cli -- pack examples/polyglot-manual-source/python
//!
//! Run:
//!   cargo run -p polyglot-manual-source-scenario -- --runtime=python
//!   cargo run -p polyglot-manual-source-scenario -- --runtime=deno

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use streamlib::core::StreamError;
use streamlib::{ProcessorSpec, Result, StreamRuntime};

const RUN_DURATION: Duration = Duration::from_secs(2);
const INTERVAL_MS: u32 = 33;
/// Minimum tick count we expect within `RUN_DURATION` allowing for
/// startup latency and scheduler noise. ~30Hz × 2s = 60 nominal;
/// require at least a third to keep the gate robust against CI
/// jitter.
const MIN_FRAMES_EMITTED: u32 = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    Python,
    Deno,
}

impl RuntimeKind {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "python" => Ok(Self::Python),
            "deno" => Ok(Self::Deno),
            other => Err(format!(
                "unknown --runtime value '{other}' (expected 'python' or 'deno')"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Deno => "deno",
        }
    }

    fn processor_name(self) -> &'static str {
        match self {
            Self::Python => "com.tatolab.polyglot_manual_source",
            Self::Deno => "com.tatolab.polyglot_manual_source_deno",
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(frames) => {
            println!("✓ frames emitted: {frames} (>= {MIN_FRAMES_EMITTED})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ scenario failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u32> {
    let mut runtime_kind = RuntimeKind::Python;
    let mut output_file: Option<PathBuf> = None;
    for a in std::env::args().skip(1) {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output-file=") {
            output_file = Some(PathBuf::from(value));
        }
    }
    let output_file = output_file.unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "polyglot-manual-source-{}-frames.txt",
            std::process::id()
        ))
    });
    // Clear any stale data from a prior run so the read-back is
    // unambiguous.
    let _ = std::fs::remove_file(&output_file);

    println!("=== Polyglot manual source scenario (#542) ===");
    println!("Runtime:      {}", runtime_kind.as_str());
    println!("Output file:  {}", output_file.display());
    println!("Tick rate:    {INTERVAL_MS}ms");
    println!("Run length:   {:?}", RUN_DURATION);

    let runtime = StreamRuntime::new()?;
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path =
                manifest_dir.join("python/polyglot-manual-source-0.1.0.slpkg");
            let project_path = manifest_dir.join("python");
            if slpkg_path.exists() {
                runtime.load_package(&slpkg_path)?;
            } else {
                runtime.load_project(&project_path)?;
            }
        }
        RuntimeKind::Deno => {
            runtime.load_project(&manifest_dir.join("deno"))?;
        }
    }

    let manual = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        serde_json::json!({
            "output_file": output_file.to_string_lossy(),
            "interval_ms": INTERVAL_MS,
        }),
    ))?;
    println!("+ ManualSource: {manual}");

    runtime.start()?;
    std::thread::sleep(RUN_DURATION);
    runtime.stop()?;

    let frames = read_frame_count(&output_file)?;
    if frames < MIN_FRAMES_EMITTED {
        return Err(StreamError::Runtime(format!(
            "manual source emitted only {frames} frames; expected >= {MIN_FRAMES_EMITTED}",
        )));
    }
    Ok(frames)
}

fn read_frame_count(path: &std::path::Path) -> Result<u32> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        StreamError::Runtime(format!(
            "polyglot manual source did not write {} — worker thread may not have run: {e}",
            path.display()
        ))
    })?;
    raw.trim()
        .parse::<u32>()
        .map_err(|e| StreamError::Runtime(format!("output file did not contain a u32: {e}")))
}
