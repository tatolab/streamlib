// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use streamlib::core::streaming::{MoqPublishSession, MoqRelayConfig, MoqSubscribeSession};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "moq-data-example";
const TRACK_NAME: &str = "sensor-telemetry";
const READING_COUNT: usize = 10;

/// A telemetry reading from a temperature sensor, serialized with MessagePack.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SensorReading {
    sensor_id: String,
    temperature: f64,
    timestamp_ns: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| DEFAULT_BROADCAST_PATH.to_string());
    let tls_disable_verify = std::env::var("TLS_DISABLE_VERIFY")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    let config = MoqRelayConfig {
        relay_endpoint_url: relay_url.clone(),
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify,
        timeout_ms: 10000,
    };

    println!("=== MoQ Schema-Agnostic Data Example ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Track:     {TRACK_NAME}");
    println!("Format:    MessagePack (rmp-serde)");
    println!();

    // --- Publisher: serialize and send sensor readings ---
    println!("Connecting publisher...");
    let mut publisher = MoqPublishSession::connect(config.clone()).await?;
    println!("Publisher connected. Sending {READING_COUNT} sensor readings.\n");

    for i in 0..READING_COUNT {
        let reading = SensorReading {
            sensor_id: format!("sensor-{}", i % 3),
            temperature: 20.0 + (i as f64 * 0.5),
            timestamp_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i64,
        };

        let payload = rmp_serde::to_vec(&reading)?;
        let is_keyframe = i == 0;
        publisher.publish_frame(TRACK_NAME, &payload, is_keyframe)?;

        println!(
            "  Published: sensor_id={} temperature={:.1}C ({} bytes msgpack)",
            reading.sensor_id,
            reading.temperature,
            payload.len(),
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("\nAll readings published.");

    // --- Subscriber: receive and deserialize sensor readings ---
    println!("\nConnecting subscriber...");
    let subscriber = MoqSubscribeSession::connect(config).await?;
    let mut track_consumer = subscriber.subscribe_track(&broadcast_path, TRACK_NAME)?;
    println!("Subscribed to '{TRACK_NAME}'. Reading frames...\n");

    let mut received = 0usize;
    'outer: loop {
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

        loop {
            match group_consumer.read_frame().await {
                Ok(Some(frame_bytes)) => {
                    let reading: SensorReading = rmp_serde::from_slice(&frame_bytes)?;
                    println!(
                        "  Received: sensor_id={} temperature={:.1}C timestamp_ns={}",
                        reading.sensor_id, reading.temperature, reading.timestamp_ns,
                    );
                    received += 1;
                    if received >= READING_COUNT {
                        break 'outer;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Error reading frame: {e}");
                    break 'outer;
                }
            }
        }
    }

    println!("\n--- Done: {received}/{READING_COUNT} readings received ---");
    Ok(())
}
