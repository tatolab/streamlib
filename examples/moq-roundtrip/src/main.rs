// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Roundtrip Example
//!
//! Demonstrates byte-level publish/subscribe through a MoQ relay.
//! Publishes fake sensor data as MessagePack, subscribes to the same
//! track, deserializes, and logs received values.
//!
//! Usage:
//!   cargo run -p moq-roundtrip -- <relay_url> <broadcast_path>
//!
//! Example:
//!   cargo run -p moq-roundtrip -- https://relay.quic.video my-test-broadcast

use serde::{Deserialize, Serialize};
use streamlib::core::streaming::{MoqPublishSession, MoqRelayConfig, MoqSubscribeSession};
use std::time::Duration;

/// Fake sensor telemetry payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SensorReading {
    timestamp_ms: u64,
    temperature: f32,
    humidity: f32,
    pressure: f32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <relay_url> <broadcast_path>", args[0]);
        eprintln!("Example: {} https://relay.quic.video my-test-broadcast", args[0]);
        std::process::exit(1);
    }

    let relay_url = args[1].clone();
    let broadcast_path = args[2].clone();
    let track_name = "sensor-data";

    println!("MoQ Roundtrip Test");
    println!("  Relay: {}", relay_url);
    println!("  Broadcast: {}", broadcast_path);
    println!("  Track: {}", track_name);
    println!();

    // Connect publisher
    let publish_config = MoqRelayConfig {
        relay_endpoint_url: relay_url.clone(),
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify: false,
        timeout_ms: 10000,
    };

    let mut publish_session = MoqPublishSession::connect(publish_config).await?;
    println!("[Publisher] Connected to relay");

    // Allow the announce to propagate before subscribing
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish a few frames first to ensure the track exists
    for i in 0..3 {
        let reading = SensorReading {
            timestamp_ms: i * 100,
            temperature: 22.5 + (i as f32 * 0.1),
            humidity: 45.0,
            pressure: 1013.25,
        };
        let bytes = rmp_serde::to_vec(&reading)?;
        publish_session.publish_frame(track_name, &bytes, false)?;
    }
    println!("[Publisher] Published 3 initial frames");

    // Connect subscriber
    let subscribe_config = MoqRelayConfig {
        relay_endpoint_url: relay_url,
        broadcast_path,
        tls_disable_verify: false,
        timeout_ms: 10000,
    };

    let subscribe_session = MoqSubscribeSession::connect(subscribe_config).await?;
    println!("[Subscriber] Connected to relay");

    let mut track_reader = subscribe_session.subscribe_track(track_name)?;
    println!("[Subscriber] Subscribed to track '{}'", track_name);

    // Spawn publisher that sends frames periodically
    let publish_handle = tokio::spawn(async move {
        for i in 3..20u64 {
            let reading = SensorReading {
                timestamp_ms: i * 100,
                temperature: 22.5 + (i as f32 * 0.1),
                humidity: 45.0 + (i as f32 * 0.05),
                pressure: 1013.25 - (i as f32 * 0.01),
            };
            let bytes = rmp_serde::to_vec(&reading).unwrap();
            if let Err(e) = publish_session.publish_frame(track_name, &bytes, false) {
                tracing::error!("[Publisher] Error: {}", e);
                break;
            }
            if i % 5 == 0 {
                println!("[Publisher] Sent frame #{}", i);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("[Publisher] Done sending");
    });

    // Receive frames with timeout
    let receive_handle = tokio::spawn(async move {
        let mut received_count = 0u64;

        let receive_deadline = tokio::time::Instant::now() + Duration::from_secs(10);

        loop {
            let subgroup_result = tokio::time::timeout_at(
                receive_deadline,
                track_reader.next_subgroup(),
            )
            .await;

            match subgroup_result {
                Ok(Ok(Some(mut subgroup_reader))) => {
                    loop {
                        match subgroup_reader.read_frame().await {
                            Ok(Some(frame_bytes)) => {
                                match rmp_serde::from_slice::<SensorReading>(&frame_bytes) {
                                    Ok(reading) => {
                                        received_count += 1;
                                        println!(
                                            "[Subscriber] Frame #{}: temp={:.1}°C humidity={:.1}% pressure={:.2}hPa (ts={}ms)",
                                            received_count,
                                            reading.temperature,
                                            reading.humidity,
                                            reading.pressure,
                                            reading.timestamp_ms,
                                        );
                                    }
                                    Err(e) => {
                                        println!(
                                            "[Subscriber] Received {} bytes but failed to deserialize: {}",
                                            frame_bytes.len(),
                                            e,
                                        );
                                    }
                                }
                            }
                            Ok(None) => break, // Subgroup finished
                            Err(e) => {
                                tracing::warn!("[Subscriber] Frame read error: {}", e);
                                break;
                            }
                        }
                    }
                }
                Ok(Ok(None)) => {
                    println!("[Subscriber] Track ended");
                    break;
                }
                Ok(Err(e)) => {
                    tracing::warn!("[Subscriber] Subgroup error: {}", e);
                    break;
                }
                Err(_) => {
                    println!("[Subscriber] Timeout after 10s");
                    break;
                }
            }
        }

        println!("[Subscriber] Total received: {} frames", received_count);
    });

    // Wait for both to finish
    let _ = tokio::join!(publish_handle, receive_handle);

    println!("\nRoundtrip test complete.");
    Ok(())
}
