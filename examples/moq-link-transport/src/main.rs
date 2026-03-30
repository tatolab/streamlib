// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera -> Display with MoQ link-level transport.
//!
//! Demonstrates a pipeline where the link between camera and display
//! uses both iceoryx2 (local IPC) and MoQ (remote relay) transport.
//! Frames are published to the MoQ relay alongside local iceoryx2 delivery,
//! and the display subscribes from both sources.
//!
//! ## Usage
//!
//! ```bash
//! RELAY_URL=https://draft-14.cloudflare.mediaoverquic.com \
//! BROADCAST_PATH=streamlib-link-test \
//! cargo run -p moq-link-transport
//! ```

use streamlib::core::InputLinkPortRef;
use streamlib::core::OutputLinkPortRef;
use streamlib::{CameraProcessor, DisplayProcessor, MoqLinkTransportConfig, Result, StreamRuntime};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "streamlib-link-transport";

fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| DEFAULT_BROADCAST_PATH.to_string());

    println!("=== MoQ Link-Level Transport Example ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!();

    let runtime = StreamRuntime::new()?;

    // Add camera processor
    println!("Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("Camera added: {camera}");

    // Add display processor
    println!("Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("MoQ Link Transport Demo".to_string()),
        scaling_mode: Default::default(),
        ..Default::default()
    }))?;
    println!("Display added: {display}");

    // Connect camera -> display with MoQ link-level transport.
    // This wires both iceoryx2 (local) and MoQ (remote) on the same link.
    println!("Connecting camera -> display with MoQ transport...");
    let moq_config = MoqLinkTransportConfig {
        moq_relay_url: relay_url,
        moq_broadcast_namespace: broadcast_path,
        moq_track_name_override: Some("video".to_string()),
    };
    runtime.connect_with_moq_transport(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&display, "video"),
        moq_config,
    )?;
    println!("Pipeline connected with MoQ transport");

    // Start the pipeline
    println!("\nStarting pipeline... Press Ctrl+C to stop\n");
    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\nPipeline stopped");
    Ok(())
}
