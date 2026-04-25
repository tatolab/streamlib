// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → polyglot DMA-BUF consumer → Display pipeline (Linux).
//!
//! Pipeline-level gate for the polyglot consumer DMA-BUF FD path shipped in
//! #394 / #420. The Python or Deno subprocess receives camera frames over
//! IPC, calls `ctx.gpu_limited_access.resolve_surface(frame.surface_id)` to
//! import the host-allocated DMA-BUF, locks it, reads a probe byte, then
//! forwards the frame unmodified to the display.
//!
//! Usage:
//!   cargo run -p polyglot-dma-buf-consumer-scenario -- \
//!       [--runtime=python|deno] [device] [seconds] [--negative]
//!
//! Defaults to `--runtime=python` and `/dev/video2` (the canonical vivid index
//! in `docs/testing.md`) for 15 seconds. On hosts where vivid landed at a
//! different index (e.g. `/dev/video0` after a UVC device unplug) pass the
//! device path explicitly. The `--negative` flag sets the consumer's
//! `force_bad_surface_id` config so resolve_surface fails deterministically on
//! every frame — the pipeline must still shut down cleanly.
//!
//! Build the Python .slpkg first when running --runtime=python (or it will
//! not be found):
//!   cargo run -p streamlib-cli -- pack examples/polyglot-dma-buf-consumer/python
//!
//! The Deno project loads its `streamlib.yaml` directly — no pack step.

use std::path::PathBuf;

use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    CameraProcessor, DisplayProcessor, ProcessorSpec, Result, StreamRuntime,
};

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
            Self::Python => "com.tatolab.dma_buf_consumer",
            Self::Deno => "com.tatolab.dma_buf_consumer_deno",
        }
    }
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);

    let mut runtime_kind = RuntimeKind::Python;
    let mut device = "/dev/video2".to_string();
    let mut duration_secs: u64 = 15;
    let mut negative = false;
    let mut positional: Vec<String> = Vec::new();

    for a in args {
        if a == "--negative" {
            negative = true;
        } else if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind = RuntimeKind::parse(value).map_err(|e| {
                streamlib::core::StreamError::Configuration(e)
            })?;
        } else {
            positional.push(a);
        }
    }
    if let Some(d) = positional.first() {
        device = d.clone();
    }
    if let Some(s) = positional.get(1) {
        duration_secs = s.parse().unwrap_or(duration_secs);
    }

    println!("=== Polyglot DMA-BUF Consumer Scenario ===");
    println!("Runtime:  {}", runtime_kind.as_str());
    println!("Camera:   {device}");
    println!("Duration: {duration_secs}s");
    println!(
        "Mode:     {}",
        if negative {
            "negative (force_bad_surface_id)"
        } else {
            "normal"
        }
    );
    println!();

    let runtime = StreamRuntime::new()?;

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path =
                manifest_dir.join("python/polyglot-dma-buf-consumer-0.1.0.slpkg");
            if !slpkg_path.exists() {
                return Err(streamlib::core::StreamError::Configuration(format!(
                    "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-dma-buf-consumer/python",
                    slpkg_path.display()
                )));
            }
            runtime.load_package(&slpkg_path)?;
        }
        RuntimeKind::Deno => {
            let project_path = manifest_dir.join("deno");
            if !project_path.join("streamlib.yaml").exists() {
                return Err(streamlib::core::StreamError::Configuration(format!(
                    "Deno project not found: {}",
                    project_path.display()
                )));
            }
            runtime.load_project(&project_path)?;
        }
    }

    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: Some(device.clone()),
        ..Default::default()
    }))?;
    println!("+ Camera: {camera}");

    let consumer_config = serde_json::json!({
        "force_bad_surface_id": negative,
    });
    let consumer = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        consumer_config,
    ))?;
    println!("+ Consumer: {consumer}");

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some(format!(
            "streamlib polyglot DMA-BUF consumer ({})",
            runtime_kind.as_str()
        )),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&consumer, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&consumer, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!(
        "\nPipeline: camera -> {} consumer -> display",
        runtime_kind.as_str()
    );

    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
