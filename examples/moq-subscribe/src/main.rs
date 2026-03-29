// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::{SystemTime, UNIX_EPOCH};

use streamlib::core::streaming::{MoqRelayConfig, MoqSubscribeSession};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "streamlib-test";
const TRACK_NAME: &str = "counter";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| DEFAULT_BROADCAST_PATH.to_string());

    let config = MoqRelayConfig {
        relay_endpoint_url: relay_url.clone(),
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify: std::env::var("TLS_DISABLE_VERIFY")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        timeout_ms: 10000,
    };

    println!("=== MoQ Subscribe Example ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Track:     {TRACK_NAME}");
    println!();

    println!("Connecting to relay...");
    let session = MoqSubscribeSession::connect(config).await?;
    println!("Connected. Subscribing to track '{TRACK_NAME}'...");

    let mut track_consumer = session.subscribe_track(TRACK_NAME)?;
    println!("Subscribed. Waiting for frames...");
    println!("Press Ctrl+C to stop.\n");

    let mut frame_count: u64 = 0;
    loop {
        // Wait for the next group
        let mut group_consumer = match track_consumer.next_group().await {
            Ok(Some(group)) => group,
            Ok(None) => {
                println!("Track ended (no more groups).");
                break;
            }
            Err(e) => {
                eprintln!("Error reading next group: {e}");
                break;
            }
        };

        // Read all frames in this group
        loop {
            match group_consumer.read_frame().await {
                Ok(Some(frame_bytes)) => {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;

                    let payload_str = String::from_utf8_lossy(&frame_bytes);

                    // Try to parse "sequence:timestamp_ms" to compute latency
                    let latency_info = payload_str
                        .split_once(':')
                        .and_then(|(_, ts)| ts.parse::<u64>().ok())
                        .map(|send_ts| now_ms.saturating_sub(send_ts));

                    match latency_info {
                        Some(latency_ms) => {
                            println!(
                                "[frame={frame_count}] received {len} bytes: {payload_str} (latency={latency_ms}ms)",
                                len = frame_bytes.len(),
                            );
                        }
                        None => {
                            println!(
                                "[frame={frame_count}] received {len} bytes: {payload_str}",
                                len = frame_bytes.len(),
                            );
                        }
                    }

                    frame_count += 1;
                }
                Ok(None) => break, // Group finished
                Err(e) => {
                    eprintln!("Error reading frame: {e}");
                    break;
                }
            }
        }
    }

    Ok(())
}
