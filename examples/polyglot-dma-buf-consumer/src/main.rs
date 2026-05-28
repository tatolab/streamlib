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
//! Both Python and Deno sub-packages are loaded declaratively via the
//! runner's `streamlib.yaml` (no separate pack step required).

use std::path::PathBuf;

use streamlib::sdk::descriptors::SchemaIdent;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
use streamlib::sdk::schema_ident;

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

    fn processor_ident(self) -> Result<SchemaIdent> {
        match self {
            Self::Python => streamlib::sdk::schema_ident_any_version!(
                "tatolab",
                "polyglot-dma-buf-consumer",
                "DmaBufConsumer"
            ),
            Self::Deno => streamlib::sdk::schema_ident_any_version!(
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
            runtime_kind = RuntimeKind::parse(value).map_err(|e| {
                streamlib::sdk::error::Error::Configuration(e)
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

    let runtime = Runner::new_with_orchestrator(streamlib::sdk::PolyglotBuildOrchestrator::default())?;

    // Load `@tatolab/camera` and `@tatolab/display` via the default
    // resolver chain (workspace stage → installed cache). Both must
    // have been staged via `cargo xtask build-plugins
    // --package @tatolab/camera --package @tatolab/display` first.
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/camera"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;

    // Load the polyglot processors via explicit add_module_with calls.
    // The Python and Deno sub-packages are example-local (siblings of
    // this example crate) and not workspace-staged, so each is
    // resolved by its manifest directory. The recursive dep walker
    // follows each sub-package's own dependencies. The runner picks
    // which one to instantiate via `schema_ident_any_version!` based
    // on `--runtime`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "polyglot-dma-buf-consumer"),
        Strategy::Path { path: manifest_dir.join("python"), build: BuildPolicy::IfStale },
    )?;
    runtime.add_module_with_blocking(
        module_ident_any_version!("tatolab", "polyglot-dma-buf-consumer-deno"),
        Strategy::Path { path: manifest_dir.join("deno"), build: BuildPolicy::IfStale },
    )?;

    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({ "device_id": device }),
    ))?;
    println!("+ Camera: {camera}");

    let consumer_config = serde_json::json!({
        "force_bad_surface_id": negative,
    });
    let consumer = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_ident()?,
        consumer_config,
    ))?;
    println!("+ Consumer: {consumer}");

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
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
