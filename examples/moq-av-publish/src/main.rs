// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streamlib::core::streaming::{MoqPublishSession, MoqRelayConfig};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";

const VIDEO_TRACK: &str = "video";
const AUDIO_TRACK: &str = "audio";

const VIDEO_FRAME_INTERVAL: Duration = Duration::from_millis(33); // ~30 fps
const AUDIO_FRAME_INTERVAL: Duration = Duration::from_millis(20); // 50 fps (20ms Opus frames)
const VIDEO_KEYFRAME_EVERY: u64 = 30;

fn generate_broadcast_path() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("streamlib-av-{ts}")
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| generate_broadcast_path());

    let config = MoqRelayConfig {
        relay_endpoint_url: relay_url.clone(),
        broadcast_path: broadcast_path.clone(),
        tls_disable_verify: std::env::var("TLS_DISABLE_VERIFY")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        timeout_ms: 10000,
    };

    println!("=== MoQ Audio + Video Publisher ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Tracks:    {VIDEO_TRACK} (30fps), {AUDIO_TRACK} (50fps)");
    println!();

    println!("Connecting to relay...");
    let mut session = MoqPublishSession::connect(config).await?;
    println!("Connected. Publishing A/V frames.");
    println!("Press Ctrl+C to stop.\n");

    let mut video_interval = tokio::time::interval(VIDEO_FRAME_INTERVAL);
    let mut audio_interval = tokio::time::interval(AUDIO_FRAME_INTERVAL);

    let mut video_seq: u64 = 0;
    let mut audio_seq: u64 = 0;

    loop {
        tokio::select! {
            _ = video_interval.tick() => {
                let ts = timestamp_ms();
                let payload = format!("video:{video_seq}:{ts}");
                let is_keyframe = video_seq % VIDEO_KEYFRAME_EVERY == 0;

                session.publish_frame(VIDEO_TRACK, payload.as_bytes(), is_keyframe)?;

                if is_keyframe {
                    println!("[video seq={video_seq}] KEYFRAME {len} bytes",
                        len = payload.len(),
                    );
                }

                video_seq += 1;
            }
            _ = audio_interval.tick() => {
                let ts = timestamp_ms();
                let payload = format!("audio:{audio_seq}:{ts}");

                session.publish_frame(AUDIO_TRACK, payload.as_bytes(), false)?;

                if audio_seq % 50 == 0 {
                    println!("[audio seq={audio_seq}] {len} bytes",
                        len = payload.len(),
                    );
                }

                audio_seq += 1;
            }
        }
    }
}
