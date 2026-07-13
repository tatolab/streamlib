// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot manual source reference example (#542 / #604).
//!
//! Exercises the `execution: manual` worker-thread / async-loop idiom
//! in both the Python and Deno SDKs, AND the iceoryx2 publish path
//! from the worker — the gap #604 closes against the original #542 /
//! PR #602 framing, which used a host-visible file as a placeholder.
//!
//! Pipeline:
//!
//!   polyglot manual source → Rust counting sink (frame_out → video_in)
//!
//! The polyglot processor's `start()` spawns a worker (thread on
//! Python, async IIFE on Deno), the worker uses `MonotonicTimer` to
//! pace tick-rate frame emission, and each tick calls
//! `ctx.outputs.write("frame_out", videoframe, ts)`. The Rust counting
//! sink subscribes to that port and writes `{"frames_received": N, ...}`
//! to a stats file on `teardown()`. After the runtime stops, this
//! binary reads the stats file and asserts the count is at least the
//! expected minimum — proving (a) the worker ran, (b) `start()`
//! returned promptly, (c) `outputs.write` from a worker thread is
//! thread-safe under the new cdylib Mutex (#604), (d) the iceoryx2
//! frame actually reached a subscriber.
//!
//! Build the counting-sink plugin first:
//!   cargo build -p polyglot-manual-source-counting-sink-plugin
//!
//! Run:
//!   cargo run -p polyglot-manual-source-scenario -- --runtime=python
//!   cargo run -p polyglot-manual-source-scenario -- --runtime=deno

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::error::Error;
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::{ProcessorSpec, ProcessorTypeReference};
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::RunnerAutoBuild;

const RUN_DURATION: Duration = Duration::from_secs(2);
const INTERVAL_MS: u32 = 33;
/// Minimum frame count the counting sink must report inside
/// `RUN_DURATION` allowing for startup latency and scheduler noise.
/// ~30 Hz × 2 s = 60 nominal; require at least a third to keep the
/// gate robust against CI jitter.
const MIN_FRAMES_RECEIVED: u32 = 20;

fn counting_sink_processor_ref() -> ProcessorTypeReference {
    processor_type_ref!(
        "tatolab",
        "polyglot-manual-source-counting-sink",
        "PolyglotManualSourceCountingSink"
    )
}
/// Env var the counting sink reads to know where to write JSON stats.
/// Set unconditionally before `runtime.start()` so the sink picks it up
/// in `setup()` even if the host hasn't yet exec'd the test binary.
const SINK_OUTPUT_ENV_VAR: &str = "STREAMLIB_POLYGLOT_MANUAL_SOURCE_SINK_OUTPUT";

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

    fn processor_ref(self) -> ProcessorTypeReference {
        match self {
            Self::Python => {
                processor_type_ref!("tatolab", "polyglot-manual-source", "PolyglotManualSource")
            }
            Self::Deno => processor_type_ref!(
                "tatolab",
                "polyglot-manual-source-deno",
                "PolyglotManualSource"
            ),
        }
    }
}

#[derive(Debug)]
struct SinkReport {
    frames_received: u32,
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!(
                "✓ frames received by counting sink: {} (>= {MIN_FRAMES_RECEIVED})",
                report.frames_received,
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ scenario failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<SinkReport> {
    let mut runtime_kind = RuntimeKind::Python;
    let mut sink_stats_file: Option<PathBuf> = None;
    for a in std::env::args().skip(1) {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind =
                RuntimeKind::parse(value).map_err(Error::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--sink-stats-file=") {
            sink_stats_file = Some(PathBuf::from(value));
        }
    }
    let sink_stats_file = sink_stats_file.unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "polyglot-manual-source-{}-{}-sink-stats.json",
            runtime_kind.as_str(),
            std::process::id(),
        ))
    });
    // Clear any stale data from a prior run so the read-back is
    // unambiguous.
    let _ = std::fs::remove_file(&sink_stats_file);

    // Tell the counting-sink processor where to write its stats. Sidesteps
    // the JTD config-schema codegen path that an in-tree processor with a
    // typed config would normally use; for a self-contained example
    // sidecar plugin, an env var is plenty.
    // SAFETY: `set_var` is single-threaded by construction here (we're
    // pre-`runtime.start()`); it's only unsafe under POSIX rules when
    // racing against `getenv` on another thread.
    unsafe {
        std::env::set_var(SINK_OUTPUT_ENV_VAR, &sink_stats_file);
    }

    println!("=== Polyglot manual source scenario (#604) ===");
    println!("Runtime:           {}", runtime_kind.as_str());
    println!("Sink stats file:   {}", sink_stats_file.display());
    println!("Tick rate:         {INTERVAL_MS}ms");
    println!("Run length:        {:?}", RUN_DURATION);

    let runtime = Runner::with_auto_build()?;

    // No module-loading calls: the counting-sink plugin plus the
    // example-local `./python` + `./deno` polyglot source packages all live
    // in this app's `streamlib_modules/` folder (populated by `./setup.sh`).
    // The runtime lazily discovers + loads each on the first
    // `processor_type_ref!` reference; the runner picks the Python or Deno
    // provider by `--runtime`.

    // 3. Add processors.
    let manual = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_ref(),
        serde_json::json!({
            "interval_ms": INTERVAL_MS,
        }),
    ))?;
    println!("+ ManualSource: {manual}");

    let sink = runtime.add_processor(ProcessorSpec::new(
        counting_sink_processor_ref(),
        // Sink reads its output path from `SINK_OUTPUT_ENV_VAR` set above —
        // pass an empty config so the macro-derived `EmptyConfig` (the
        // default for processors without a YAML config schema) deserializes
        // cleanly.
        serde_json::Value::Null,
    ))?;
    println!("+ CountingSink: {sink}");

    // 4. Wire source → sink.
    runtime.connect(
        OutputLinkPortRef::new(&manual, "frame_out"),
        InputLinkPortRef::new(&sink, "video_in"),
    )?;

    // 5. Run.
    runtime.start()?;
    std::thread::sleep(RUN_DURATION);
    runtime.stop()?;

    let report = read_sink_report(&sink_stats_file)?;
    if report.frames_received < MIN_FRAMES_RECEIVED {
        return Err(Error::Runtime(format!(
            "counting sink received only {} frames; expected >= {MIN_FRAMES_RECEIVED}",
            report.frames_received,
        )));
    }
    Ok(report)
}

fn read_sink_report(path: &std::path::Path) -> Result<SinkReport> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        Error::Runtime(format!(
            "counting sink did not write {} — sink processor may not have received any frames: {e}",
            path.display()
        ))
    })?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::Runtime(format!("sink stats file is not valid JSON: {e}"))
    })?;
    let frames_received = v
        .get("frames_received")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| Error::Runtime("missing frames_received".into()))?
        as u32;
    Ok(SinkReport { frames_received })
}
