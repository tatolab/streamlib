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
//! Build the Python `.slpkg` first (Deno doesn't need a pack step):
//!   cargo run -p streamlib-cli -- pack examples/polyglot-manual-source/python
//!
//! Build the counting-sink plugin:
//!   cargo build -p polyglot-manual-source-counting-sink-plugin
//!
//! Run:
//!   cargo run -p polyglot-manual-source-scenario -- --runtime=python
//!   cargo run -p polyglot-manual-source-scenario -- --runtime=deno

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::{ProcessorSpec, Result, StreamRuntime};

const RUN_DURATION: Duration = Duration::from_secs(2);
const INTERVAL_MS: u32 = 33;
/// Minimum frame count the counting sink must report inside
/// `RUN_DURATION` allowing for startup latency and scheduler noise.
/// ~30 Hz × 2 s = 60 nominal; require at least a third to keep the
/// gate robust against CI jitter.
const MIN_FRAMES_RECEIVED: u32 = 20;

const COUNTING_SINK_PROCESSOR: &str = "com.tatolab.polyglot_manual_source_counting_sink";
const COUNTING_SINK_PLUGIN_DYLIB: &str = "libpolyglot_manual_source_counting_sink_plugin.so";
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

    fn processor_name(self) -> &'static str {
        match self {
            Self::Python => "com.tatolab.polyglot_manual_source",
            Self::Deno => "com.tatolab.polyglot_manual_source_deno",
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
                RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
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

    let runtime = StreamRuntime::new()?;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // 1. Stage the counting-sink plugin. The plugin sub-crate builds a
    //    cdylib that `runtime.load_project(plugin/)` picks up by
    //    iterating `plugin/lib/` for `*.so`. Mirrors the camera-rust-plugin
    //    pattern: the example's `cargo build` produces the dylib in
    //    `target/<profile>/`, and the scenario binary copies it into
    //    `plugin/lib/` before loading.
    let plugin_dir = manifest_dir.join("plugin");
    stage_plugin_dylib(&plugin_dir)?;
    runtime.load_project(&plugin_dir)?;

    // 2. Load the polyglot project (Python .slpkg or Deno project).
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

    // 3. Add processors.
    let manual = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        serde_json::json!({
            "interval_ms": INTERVAL_MS,
        }),
    ))?;
    println!("+ ManualSource: {manual}");

    let sink = runtime.add_processor(ProcessorSpec::new(
        COUNTING_SINK_PROCESSOR,
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
        return Err(StreamError::Runtime(format!(
            "counting sink received only {} frames; expected >= {MIN_FRAMES_RECEIVED}",
            report.frames_received,
        )));
    }
    Ok(report)
}

/// Locate the built plugin cdylib in the workspace target dir and copy
/// it under `plugin/lib/` so `load_project` finds it. Mirrors the
/// `camera-rust-plugin` example.
fn stage_plugin_dylib(plugin_dir: &std::path::Path) -> Result<()> {
    let lib_dir = plugin_dir.join("lib");
    std::fs::create_dir_all(&lib_dir).map_err(|e| {
        StreamError::Configuration(format!(
            "failed to create plugin lib dir {}: {e}",
            lib_dir.display(),
        ))
    })?;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| StreamError::Configuration("workspace root not found".into()))?;

    let target_dir = workspace_root.join("target");
    let candidates = [
        target_dir.join("debug").join(COUNTING_SINK_PLUGIN_DYLIB),
        target_dir.join("release").join(COUNTING_SINK_PLUGIN_DYLIB),
    ];
    let source = candidates.iter().find(|p| p.exists()).ok_or_else(|| {
        StreamError::Configuration(format!(
            "counting-sink plugin dylib not found. Build it first:\n  \
             cargo build -p polyglot-manual-source-counting-sink-plugin\n\
             Looked in:\n  {}\n  {}",
            candidates[0].display(),
            candidates[1].display(),
        ))
    })?;
    let dest = lib_dir.join(COUNTING_SINK_PLUGIN_DYLIB);
    std::fs::copy(source, &dest).map_err(|e| {
        StreamError::Configuration(format!(
            "failed to copy {} → {}: {e}",
            source.display(),
            dest.display(),
        ))
    })?;
    Ok(())
}

fn read_sink_report(path: &std::path::Path) -> Result<SinkReport> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        StreamError::Runtime(format!(
            "counting sink did not write {} — sink processor may not have received any frames: {e}",
            path.display()
        ))
    })?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        StreamError::Runtime(format!("sink stats file is not valid JSON: {e}"))
    })?;
    let frames_received = v
        .get("frames_received")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| StreamError::Runtime("missing frames_received".into()))?
        as u32;
    Ok(SinkReport { frames_received })
}
