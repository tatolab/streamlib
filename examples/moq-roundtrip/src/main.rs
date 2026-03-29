// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streamlib::core::streaming::{MoqPublishSession, MoqRelayConfig, MoqSubscribeSession};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const TRACK_NAME: &str = "roundtrip";
const PUBLISH_INTERVAL: Duration = Duration::from_millis(500);
const TOTAL_FRAMES: u64 = 60;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path = std::env::var("BROADCAST_PATH").unwrap_or_else(|_| {
        let id = std::process::id();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        format!("streamlib-roundtrip-{id}-{ts}")
    });
    let tls_disable_verify = std::env::var("TLS_DISABLE_VERIFY")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    println!("=== MoQ Roundtrip Example ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Track:     {TRACK_NAME}");
    println!("Frames:    {TOTAL_FRAMES} @ {}ms interval", PUBLISH_INTERVAL.as_millis());
    println!();

    // Connect publisher
    let publish_config = MoqRelayConfig {
        relay_endpoint_url: relay_url.clone(),
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify,
        timeout_ms: 10000,
    };
    println!("Connecting publisher...");
    let mut publish_session = MoqPublishSession::connect(publish_config).await?;
    println!("Publisher connected.");

    // Connect subscriber
    let subscribe_config = MoqRelayConfig {
        relay_endpoint_url: relay_url,
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify,
        timeout_ms: 10000,
    };
    println!("Connecting subscriber...");
    let subscribe_session = MoqSubscribeSession::connect(subscribe_config).await?;
    let mut track_consumer = subscribe_session.subscribe_track(TRACK_NAME)?;
    println!("Subscriber connected.\n");

    // Spawn publisher task
    let publish_handle = tokio::spawn(async move {
        for sequence in 0..TOTAL_FRAMES {
            let timestamp_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            let payload = format!("{sequence}:{timestamp_ms}");
            let is_keyframe = sequence % 30 == 0;

            if let Err(e) = publish_session.publish_frame(TRACK_NAME, payload.as_bytes(), is_keyframe) {
                tracing::error!(%e, "Failed to publish frame {sequence}");
                break;
            }

            tracing::debug!(sequence, "published frame");
            tokio::time::sleep(PUBLISH_INTERVAL).await;
        }

        // Keep session alive briefly so subscriber can drain
        tokio::time::sleep(Duration::from_secs(2)).await;
        tracing::info!("Publisher finished");
    });

    // Receive frames and measure latency
    let subscribe_handle = tokio::spawn(async move {
        let mut frame_count: u64 = 0;
        let mut latencies_ms: Vec<u64> = Vec::new();

        loop {
            let mut group_consumer = match track_consumer.next_group().await {
                Ok(Some(group)) => group,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(%e, "Error reading next group");
                    break;
                }
            };

            loop {
                match group_consumer.read_frame().await {
                    Ok(Some(frame_bytes)) => {
                        let now_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;

                        let payload_str = String::from_utf8_lossy(&frame_bytes);

                        if let Some((seq_str, ts_str)) = payload_str.split_once(':') {
                            let seq: u64 = seq_str.parse().unwrap_or(0);
                            if let Ok(send_ts) = ts_str.parse::<u64>() {
                                let latency = now_ms.saturating_sub(send_ts);
                                latencies_ms.push(latency);
                                println!(
                                    "[seq={seq:>4}] {len:>4} bytes | latency {latency:>6}ms",
                                    len = frame_bytes.len(),
                                );
                            }
                        }

                        frame_count += 1;
                        if frame_count >= TOTAL_FRAMES {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(%e, "Error reading frame");
                        break;
                    }
                }
            }

            if frame_count >= TOTAL_FRAMES {
                break;
            }
        }

        // Print latency stats
        println!("\n=== Latency Summary ===");
        println!("Frames received: {frame_count}");

        if !latencies_ms.is_empty() {
            latencies_ms.sort();
            let sum: u64 = latencies_ms.iter().sum();
            let avg = sum / latencies_ms.len() as u64;
            let min = latencies_ms[0];
            let max = *latencies_ms.last().unwrap();
            let p50 = latencies_ms[latencies_ms.len() / 2];
            let p99_idx = (latencies_ms.len() as f64 * 0.99) as usize;
            let p99 = latencies_ms[p99_idx.min(latencies_ms.len() - 1)];

            println!("Min:  {min}ms");
            println!("Max:  {max}ms");
            println!("Avg:  {avg}ms");
            println!("P50:  {p50}ms");
            println!("P99:  {p99}ms");
        }

        Ok::<(), anyhow::Error>(())
    });

    let (pub_result, sub_result) = tokio::join!(publish_handle, subscribe_handle);
    pub_result?;
    sub_result??;

    Ok(())
}
