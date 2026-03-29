// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use streamlib::core::streaming::{MoqRelayConfig, MoqSubscribeSession};
use std::io::Write;

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "streamlib-test";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| DEFAULT_BROADCAST_PATH.to_string());
    let track_name =
        std::env::var("TRACK_NAME").unwrap_or_else(|_| "video".to_string());
    let output_file =
        std::env::var("OUTPUT_FILE").unwrap_or_else(|_| "moq_output.h264".to_string());
    let max_frames: u64 =
        std::env::var("MAX_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(300);

    let config = MoqRelayConfig {
        relay_endpoint_url: relay_url.clone(),
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify: std::env::var("TLS_DISABLE_VERIFY")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        timeout_ms: 10000,
    };

    println!("=== MoQ Subscribe → File ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Track:     {track_name}");
    println!("Output:    {output_file}");
    println!("Max frames: {max_frames}");
    println!();

    println!("Connecting to relay...");
    let session = MoqSubscribeSession::connect(config).await?;
    println!("Subscribing to track '{track_name}'...");

    let mut track_reader = session.subscribe_track(&track_name)?;
    println!("Subscribed. Saving frames...\n");

    let mut file = std::fs::File::create(&output_file)?;
    let mut frame_count: u64 = 0;
    let mut total_bytes: u64 = 0;

    loop {
        let mut subgroup = match track_reader.next_subgroup().await {
            Ok(Some(sg)) => sg,
            Ok(None) => {
                println!("Track ended.");
                break;
            }
            Err(e) => {
                eprintln!("Subgroup error: {e}");
                break;
            }
        };

        loop {
            match subgroup.read_frame().await {
                Ok(Some(frame_bytes)) => {
                    file.write_all(&frame_bytes)?;
                    total_bytes += frame_bytes.len() as u64;
                    frame_count += 1;

                    if frame_count % 30 == 0 {
                        println!("[{frame_count}] {total_bytes} bytes saved");
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Frame error: {e}");
                    break;
                }
            }
        }

        if frame_count >= max_frames {
            break;
        }
    }

    file.flush()?;
    println!("\nDone. Saved {frame_count} frames ({total_bytes} bytes) to {output_file}");
    println!("Verify with: ffmpeg -i {output_file} -f null -");
    Ok(())
}
