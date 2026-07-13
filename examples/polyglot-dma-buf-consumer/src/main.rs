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
//! There is no module-loading call: every processor's package (`@tatolab/camera`,
//! `@tatolab/display`, and this example's own `./python` + `./deno` polyglot
//! packages) lives in this app's `streamlib_modules/` folder (populated by
//! `./setup.sh`), and the runtime lazily discovers + loads each on the first
//! `processor_type_ref!` reference.

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::{ProcessorSpec, ProcessorTypeReference};
use streamlib::sdk::runtime::Runner;

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
            Self::Python => processor_type_ref!(
                "tatolab",
                "polyglot-dma-buf-consumer",
                "DmaBufConsumer"
            ),
            Self::Deno => processor_type_ref!(
                "tatolab",
                "polyglot-dma-buf-consumer-deno",
                "DmaBufConsumer"
            ),
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
            runtime_kind = RuntimeKind::parse(value)
                .map_err(|e| streamlib::sdk::error::Error::Configuration(e))?;
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

    let runtime = Runner::with_auto_build()?;

    // No module-loading call: `@tatolab/camera`, `@tatolab/display`, and this
    // example's own `./python` + `./deno` polyglot packages all live in this
    // app's `streamlib_modules/` folder (populated by `./setup.sh`). The runtime
    // lazily discovers + loads each on the first `processor_type_ref!`
    // reference; the runner picks the Python or Deno consumer by `--runtime`.

    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({ "device_id": device }),
    ))?;
    println!("+ Camera: {camera}");

    let consumer_config = serde_json::json!({
        "force_bad_surface_id": negative,
    });
    let consumer = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_ref(),
        consumer_config,
    ))?;
    println!("+ Consumer: {consumer}");

    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": format!(
                "streamlib polyglot DMA-BUF consumer ({})",
                runtime_kind.as_str()
            ),
        }),
    ))?;
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
