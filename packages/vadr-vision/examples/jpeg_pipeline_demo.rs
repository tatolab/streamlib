// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Visual end-to-end demo of the AGP vision pipeline. Sends a real
//! 320×180 baseline JPEG over loopback UDP as VADR-TS-002 §4.6 chunks,
//! runs them through `VadrVisionDepayloader → JpegDecoder → Display`,
//! and (when `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR` is set) writes decoded
//! PNGs straight from the display processor's sampler.
//!
//! Usage:
//!     STREAMLIB_DISPLAY_PNG_SAMPLE_DIR=/tmp/out \
//!     STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=2 \
//!     STREAMLIB_DISPLAY_FRAME_LIMIT=60 \
//!     cargo run -p streamlib-vadr-vision --example jpeg_pipeline_demo
//!
//! `STREAMLIB_DISPLAY_FRAME_LIMIT` is important for automated runs —
//! winit + X11 don't always respect SIGTERM cleanly, so the limit lets
//! the display processor self-exit after a known number of frames.

use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;
use streamlib_vadr_vision::header::{ChunkHeader, encode};

#[allow(unused_imports)]
use streamlib_display::DisplayProcessor as _;
#[allow(unused_imports)]
use streamlib_jpeg::JpegDecoderProcessor as _;
#[allow(unused_imports)]
use streamlib_network::UdpSourceProcessor as _;
#[allow(unused_imports)]
use streamlib_vadr_vision::VadrVisionDepayloaderProcessor as _;

const FIXTURE_WIDTH: u32 = 320;
const FIXTURE_HEIGHT: u32 = 180;
const CHUNK_PAYLOAD_BYTES: usize = 1200;
const SEND_FPS: u32 = 15;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test_320x180.jpg")
}

fn pick_free_udp_port() -> SocketAddr {
    let probe = UdpSocket::bind("127.0.0.1:0").expect("probe bind");
    let addr = probe.local_addr().expect("probe local_addr");
    drop(probe);
    addr
}

fn chunked_datagrams(frame_id: u32, sim_time_ns: u64, jpeg: &[u8]) -> Vec<Vec<u8>> {
    let pieces: Vec<&[u8]> = jpeg.chunks(CHUNK_PAYLOAD_BYTES).collect();
    let total_chunks = pieces.len() as u16;
    let jpeg_size = jpeg.len() as u32;
    pieces
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let header = ChunkHeader {
                frame_id,
                chunk_id: i as u16,
                total_chunks,
                jpeg_size,
                payload_size: c.len() as u32,
                sim_time_ns,
            };
            encode(&header, c)
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let jpeg = std::fs::read(fixture_path())?;
    println!(
        "Loaded JPEG fixture: {} bytes ({} chunks of {} bytes max)",
        jpeg.len(),
        jpeg.len().div_ceil(CHUNK_PAYLOAD_BYTES),
        CHUNK_PAYLOAD_BYTES
    );

    let bind_addr = pick_free_udp_port();
    println!("UdpSource will bind: {bind_addr}");

    let runtime = Runner::new()?;

    let source_id = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "network", "UdpSource", "1.0.0"),
        serde_json::json!({
            "bind_addr": bind_addr.to_string(),
            "recv_buffer_bytes": 1 << 20,
        }),
    ))?;
    println!("+ UdpSource: {source_id}");

    let depayloader_id = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "vadr-vision", "VadrVisionDepayloader", "1.0.0"),
        serde_json::json!({
            "reassembly_timeout_ms": 1000,
            "max_pending_frames": 8,
            "warn_on_drop": true,
        }),
    ))?;
    println!("+ VadrVisionDepayloader: {depayloader_id}");

    let decoder_id = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "jpeg", "JpegDecoder", "1.0.0"),
        serde_json::json!({"max_width": 640, "max_height": 480}),
    ))?;
    println!("+ JpegDecoder: {decoder_id}");

    let display_id = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": FIXTURE_WIDTH,
            "height": FIXTURE_HEIGHT,
            "title": "VADR-vision pipeline demo",
            "vsync": false,
        }),
    ))?;
    println!("+ Display: {display_id}");

    runtime.connect(
        OutputLinkPortRef::new(source_id.as_str(), "packets"),
        InputLinkPortRef::new(depayloader_id.as_str(), "chunks_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(depayloader_id.as_str(), "jpeg_out"),
        InputLinkPortRef::new(decoder_id.as_str(), "encoded_jpeg_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(decoder_id.as_str(), "video_out"),
        InputLinkPortRef::new(display_id.as_str(), "video"),
    )?;

    println!("\nStarting pipeline...\n");
    runtime.start()?;
    std::thread::sleep(Duration::from_millis(750)); // PUBSUB / iceoryx2 warm-up

    // Spawn the chunked-JPEG sender. Runs until `stop_flag` is set.
    let stop_flag = Arc::new(AtomicBool::new(false));
    let sender_jpeg = jpeg.clone();
    let sender_stop = Arc::clone(&stop_flag);
    let sender = std::thread::spawn(move || {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("test sender bind");
        let frame_period = Duration::from_millis(1000 / SEND_FPS as u64);
        let mut frame_id: u32 = 0;
        let start = Instant::now();
        while !sender_stop.load(Ordering::Relaxed) {
            let sim_time_ns = start.elapsed().as_nanos() as u64;
            let dgrams = chunked_datagrams(frame_id, sim_time_ns, &sender_jpeg);
            for d in &dgrams {
                if socket.send_to(d, bind_addr).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2)); // pace
            }
            frame_id = frame_id.wrapping_add(1);
            std::thread::sleep(frame_period);
        }
    });

    // Let the pipeline run long enough for PNG samples to land. With
    // STREAMLIB_DISPLAY_FRAME_LIMIT set, the display self-exits — wait
    // a generous fixed window regardless.
    let run_seconds = std::env::var("VADR_DEMO_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6u64);
    println!("Running pipeline for ~{run_seconds}s...");
    std::thread::sleep(Duration::from_secs(run_seconds));

    stop_flag.store(true, Ordering::Relaxed);
    let _ = sender.join();

    println!("\nStopping pipeline...");
    runtime.stop()?;

    if let Ok(dir) = std::env::var("STREAMLIB_DISPLAY_PNG_SAMPLE_DIR") {
        println!("\nPNG samples written under: {dir}");
    } else {
        println!(
            "\n(STREAMLIB_DISPLAY_PNG_SAMPLE_DIR unset — set it to capture decoded PNGs.)"
        );
    }
    Ok(())
}
