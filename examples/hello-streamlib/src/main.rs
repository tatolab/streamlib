// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! hello-streamlib — camera → inline processor → display, in one small file.
//!
//! The whole pipeline is `App::new` + `add` / `add_local` + `connect` + `run`:
//! the camera and display come from installed packages (referenced by name,
//! version-free), and [`HelloForward`](hello_forward) is an inline
//! `#[processor]` authored right here in the app — no `build.rs`, no
//! `streamlib.yaml`, no `schemas:` list, no `_generated_`. `add_local`
//! registers it live and returns a connectable handle exactly like an
//! installed processor.

mod hello_forward;

use hello_forward::HelloForward;
use streamlib::sdk::App;
use streamlib::sdk::error::Result;
use streamlib::sdk::processor_type_ref;

fn main() -> Result<()> {
    println!("=== hello-streamlib: camera → inline forward → display ===\n");

    let app = App::new()?;

    println!("📷 Adding camera source...");
    let camera = app.add(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({}),
    )?;
    println!("✓ Camera: {camera}\n");

    println!("🔁 Registering the inline forward processor (add_local)...");
    let forward = app.add_local::<HelloForward::Processor>(serde_json::json!({}))?;
    println!("✓ HelloForward: {forward}\n");

    println!("🖥️  Adding display sink...");
    let display = app.add(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "title": "hello-streamlib",
        }),
    )?;
    println!("✓ Display: {display}\n");

    println!("🔗 Connecting camera → forward → display...");
    app.connect((&camera, "video"), (&forward, "video_in"))?;
    app.connect((&forward, "video_out"), (&display, "video"))?;
    println!("✓ Pipeline connected\n");

    println!("▶️  Running...");
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");

    app.run()
}
