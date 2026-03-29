// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streamlib::core::streaming::{MoqPublishSession, MoqRelayConfig};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "streamlib-test";
const TRACK_NAME: &str = "counter";
const PUBLISH_INTERVAL: Duration = Duration::from_millis(500);

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

    println!("=== MoQ Publish Example ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Track:     {TRACK_NAME}");
    println!();

    println!("Connecting to relay...");
    let mut session = MoqPublishSession::connect(config).await?;
    println!("Connected. Publishing frames every {}ms", PUBLISH_INTERVAL.as_millis());
    println!("Press Ctrl+C to stop.\n");

    let mut sequence: u64 = 0;
    loop {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Payload: simple "{sequence}:{timestamp_ms}" encoded as bytes
        let payload = format!("{sequence}:{timestamp_ms}");
        let is_keyframe = sequence % 30 == 0;

        session.publish_frame(TRACK_NAME, payload.as_bytes(), is_keyframe)?;

        println!("[seq={sequence}] published {len} bytes (keyframe={is_keyframe})",
            len = payload.len(),
        );

        sequence += 1;
        tokio::time::sleep(PUBLISH_INTERVAL).await;
    }
}
