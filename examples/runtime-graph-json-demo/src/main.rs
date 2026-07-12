// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runtime graph → JSON demo.
//!
//! Builds a camera → display + camera → MP4-writer (fan-out) graph from
//! registry-resolved packages and prints the serialized graph as JSON via
//! [`Runner::to_json`]. No pipeline is started — this only exercises graph
//! construction and serialization.

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, SemVerRange, Strategy};
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let runtime = Runner::with_auto_build()?;

    // Resolve every package from the static generic store by version — the
    // cross-repo consumer path. The orchestrator pulls each `.slpkg` and builds
    // it from source on the host. Registry endpoint comes from
    // `STREAMLIB_REGISTRY_URL`.
    let registry = || Strategy::Registry {
        version_req: SemVerRange::Any,
        build: BuildPolicy::IfStale,
    };
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), registry())?;
    runtime
        .add_module_with_blocking(module_ident_any_version!("tatolab", "display"), registry())?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "mp4"), registry())?;

    // Add a camera processor
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({ "device_id": "device-abc-123" }),
    ))?;

    // Add a display processor
    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "My Display",
        }),
    ))?;

    // Add an MP4 writer processor
    let recorder = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "mp4", "LinuxMp4Writer", "1.0.0"),
        serde_json::json!({
            "output_path": "/tmp/recording.mp4",
            "fps": 30,
        }),
    ))?;

    // Connect camera to display
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    // Connect camera to recorder (fan-out)
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&recorder, "video_in"),
    )?;

    // Print the graph as JSON
    let json_str = runtime.to_json().expect("Failed to serialize graph");
    println!("{}", json_str);

    Ok(())
}
