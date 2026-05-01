// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot continuous processor reference example (issue #542).
//!
//! Exercises `execution: continuous` end-to-end through both the
//! Python and Deno polyglot SDKs after the subprocess runner's
//! continuous-mode dispatch was reworked from `time.sleep` /
//! `setTimeout` to a real `MonotonicTimer` (timerfd, drift-free).
//!
//! The polyglot processor's `process()` is called by the runner
//! once per tick at the manifest's `interval_ms`. Each call records
//! `monotonic_now_ns()` and updates an in-memory tick counter +
//! first/last-tick timestamps — no per-tick IO, no escalate IPC.
//! On `teardown()` the polyglot processor writes (count, first_ns,
//! last_ns) as JSON to a host-visible output file. After the
//! runtime stops, this binary reads the file and asserts both that
//! the count is in the expected range AND that the implied average
//! inter-tick interval falls within ±5ms of the manifest's nominal
//! 16ms — the primary regression detector for the runner's
//! monotonic-clock dispatch contract.
//!
//! Why no cpu-readback adapter: cpu-readback is a last-resort tool
//! that does GPU→CPU + CPU→GPU copies per acquire. Driving it 60Hz
//! from a continuous processor's hot path is exactly the misuse
//! pattern. Worse, the IPC roundtrip overhead inflates measured
//! cadence by 1–2ms per tick, masking how good the timerfd
//! actually is. File-based stats (in-memory counters, write once
//! on teardown) give clean measurements that surface the timer's
//! true accuracy.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-continuous-processor/python
//!
//! Run:
//!   cargo run -p polyglot-continuous-processor-scenario -- --runtime=python
//!   cargo run -p polyglot-continuous-processor-scenario -- --runtime=deno

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use streamlib::core::StreamError;
use streamlib::{ProcessorSpec, Result, StreamRuntime};

const RUN_DURATION: Duration = Duration::from_secs(2);
/// Manifest-declared interval. Must match the YAML in
/// {python,deno}/streamlib.yaml — change both if changing.
const NOMINAL_INTERVAL_MS: u32 = 16;
/// ±5ms slack on the average inter-tick interval. Picked to stay
/// within a small multiple of the timerfd's nominal granularity per
/// the issue's tests/validation criterion: tight enough that a
/// regression to `time.sleep` / `setTimeout` semantics would blow
/// the gate, loose enough to absorb scheduler noise on a quiet box.
/// Now that the example has no per-tick IO overhead (cpu-readback
/// removed), real measurements should land within 1–2ms of nominal —
/// 5ms is comfortable headroom.
const INTERVAL_SLACK_MS: f64 = 5.0;
const MIN_TICK_COUNT: u32 = 30;
const MAX_TICK_COUNT: u32 = 400;

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
            Self::Python => "com.tatolab.polyglot_continuous_processor",
            Self::Deno => "com.tatolab.polyglot_continuous_processor_deno",
        }
    }
}

#[derive(Debug)]
struct TickReport {
    count: u32,
    first_ns: u64,
    last_ns: u64,
}

impl TickReport {
    fn average_interval_ns(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }
        let span = self.last_ns.saturating_sub(self.first_ns);
        Some(span as f64 / (self.count as f64 - 1.0))
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            let avg_ns = report.average_interval_ns();
            println!(
                "✓ ticks={} avg_interval_ms={}",
                report.count,
                avg_ns
                    .map(|ns| format!("{:.3}", ns / 1_000_000.0))
                    .unwrap_or_else(|| "n/a".into()),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ scenario failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<TickReport> {
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
            "polyglot-continuous-processor-{}-stats.json",
            std::process::id()
        ))
    });
    let _ = std::fs::remove_file(&output_file);

    println!("=== Polyglot continuous processor scenario (#542) ===");
    println!("Runtime:           {}", runtime_kind.as_str());
    println!("Output file:       {}", output_file.display());
    println!("Nominal interval:  {NOMINAL_INTERVAL_MS}ms");
    println!("Run length:        {:?}", RUN_DURATION);

    let runtime = StreamRuntime::new()?;
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path = manifest_dir
                .join("python/polyglot-continuous-processor-0.1.0.slpkg");
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

    let processor = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        serde_json::json!({
            "output_file": output_file.to_string_lossy(),
        }),
    ))?;
    println!("+ ContinuousProcessor: {processor}");

    runtime.start()?;
    std::thread::sleep(RUN_DURATION);
    runtime.stop()?;

    let report = read_tick_report(&output_file)?;

    if report.count < MIN_TICK_COUNT || report.count > MAX_TICK_COUNT {
        return Err(StreamError::Runtime(format!(
            "tick count {} outside expected [{MIN_TICK_COUNT}, {MAX_TICK_COUNT}]",
            report.count,
        )));
    }
    if let Some(avg_ns) = report.average_interval_ns() {
        let nominal_ns = (NOMINAL_INTERVAL_MS as f64) * 1_000_000.0;
        let slack_ns = INTERVAL_SLACK_MS * 1_000_000.0;
        if (avg_ns - nominal_ns).abs() > slack_ns {
            return Err(StreamError::Runtime(format!(
                "average inter-tick interval {:.3}ms outside nominal {NOMINAL_INTERVAL_MS}ms ± {INTERVAL_SLACK_MS}ms",
                avg_ns / 1_000_000.0,
            )));
        }
    }
    Ok(report)
}

fn read_tick_report(path: &std::path::Path) -> Result<TickReport> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        StreamError::Runtime(format!(
            "polyglot continuous processor did not write {} — teardown may have failed: {e}",
            path.display()
        ))
    })?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        StreamError::Runtime(format!("output file is not valid JSON: {e}"))
    })?;
    let count = v
        .get("tick_count")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| StreamError::Runtime("missing tick_count".into()))? as u32;
    // The Python side writes integers, the Deno side writes BigInts as
    // strings (JSON has no native u64). Accept both shapes.
    let first_ns = parse_u64_or_string(&v, "first_tick_ns")?;
    let last_ns = parse_u64_or_string(&v, "last_tick_ns")?;
    Ok(TickReport { count, first_ns, last_ns })
}

fn parse_u64_or_string(v: &serde_json::Value, key: &str) -> Result<u64> {
    let field = v.get(key).ok_or_else(|| {
        StreamError::Runtime(format!("missing {key}"))
    })?;
    if let Some(n) = field.as_u64() {
        return Ok(n);
    }
    if let Some(s) = field.as_str() {
        return s.parse::<u64>().map_err(|e| {
            StreamError::Runtime(format!("{key} not a u64 string: {e}"))
        });
    }
    Err(StreamError::Runtime(format!(
        "{key} has unexpected JSON shape: {field:?}"
    )))
}
