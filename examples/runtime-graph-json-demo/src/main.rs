// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runtime graph → JSON demo.
//!
//! Builds a camera → display + camera → MP4-writer (fan-out) graph and prints
//! the serialized graph as JSON via [`Runner::to_json`]. No pipeline is started
//! — this only exercises graph construction and serialization.
//!
//! There is no module-loading call: every processor's package
//! (`@tatolab/camera`, `@tatolab/display`, `@tatolab/mp4`) lives in this app's
//! `streamlib_modules/` folder (populated by `./setup.sh`), and the runtime
//! lazily discovers + loads each on the first `processor_type_ref!` reference.

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

fn main() -> Result<()> {
    let runtime = Runner::with_auto_build()?;

    // Add a camera processor
    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({ "device_id": "device-abc-123" }),
    ))?;

    // Add a display processor
    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "My Display",
        }),
    ))?;

    // Add an MP4 writer processor
    let recorder = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "mp4", "LinuxMp4Writer"),
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
