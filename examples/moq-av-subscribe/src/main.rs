// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use streamlib::core::streaming::{MoqRelayConfig, MoqSubscribeSession, MoqTrackReader};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "streamlib-test";

const VIDEO_TRACK: &str = "video";
const AUDIO_TRACK: &str = "audio";

/// Per-track statistics accumulated across frames.
struct TrackStats {
    frame_count: AtomicU64,
    total_bytes: AtomicU64,
    total_latency_ms: AtomicU64,
}

impl TrackStats {
    fn new() -> Self {
        Self {
            frame_count: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            total_latency_ms: AtomicU64::new(0),
        }
    }

    fn record(&self, bytes: u64, latency_ms: u64) {
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.total_latency_ms.fetch_add(latency_ms, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (u64, u64, u64) {
        let frames = self.frame_count.load(Ordering::Relaxed);
        let bytes = self.total_bytes.load(Ordering::Relaxed);
        let latency = self.total_latency_ms.load(Ordering::Relaxed);
        (frames, bytes, latency)
    }
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Read frames from a subscribed track, printing each frame and recording stats.
async fn read_track_frames(
    mut track_reader: MoqTrackReader,
    track_name: &str,
    stats: Arc<TrackStats>,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<()> {

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let mut subgroup = match track_reader.next_subgroup().await {
            Ok(Some(sg)) => sg,
            Ok(None) => {
                println!("[{track_name}] Track ended.");
                break;
            }
            Err(e) => {
                eprintln!("[{track_name}] Error reading subgroup: {e}");
                break;
            }
        };

        loop {
            match subgroup.read_frame().await {
                Ok(Some(frame_bytes)) => {
                    let now_ms = timestamp_ms();
                    let payload_str = String::from_utf8_lossy(&frame_bytes);
                    let len = frame_bytes.len() as u64;

                    // Parse "{type}:{seq}:{timestamp_ms}" to extract latency
                    let (seq, latency_ms) = parse_frame_payload(&payload_str, now_ms);

                    stats.record(len, latency_ms.unwrap_or(0));
                    let frames = stats.frame_count.load(Ordering::Relaxed);

                    match latency_ms {
                        Some(lat) => {
                            println!(
                                "[{track_name} frame={frames} seq={seq}] {len} bytes (latency={lat}ms)"
                            );
                        }
                        None => {
                            println!(
                                "[{track_name} frame={frames} seq={seq}] {len} bytes: {payload_str}"
                            );
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[{track_name}] Error reading frame: {e}");
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Parse a frame payload like "video:42:1711234567890" and return (seq_str, latency).
fn parse_frame_payload(payload: &str, now_ms: u64) -> (&str, Option<u64>) {
    let mut parts = payload.splitn(3, ':');
    let _track_type = parts.next();
    let seq = parts.next().unwrap_or("?");
    let latency = parts
        .next()
        .and_then(|ts| ts.parse::<u64>().ok())
        .map(|send_ts| now_ms.saturating_sub(send_ts));
    (seq, latency)
}

fn print_summary(video_stats: &TrackStats, audio_stats: &TrackStats, elapsed: Duration) {
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let (v_frames, v_bytes, v_latency) = video_stats.snapshot();
    let (a_frames, a_bytes, a_latency) = audio_stats.snapshot();

    println!("\n=== Summary ({elapsed_secs:.1}s) ===");

    if v_frames > 0 {
        let v_avg_lat = v_latency / v_frames;
        let v_rate = v_bytes as f64 / elapsed_secs;
        println!(
            "  video: {v_frames} frames, {v_bytes} bytes ({v_rate:.0} B/s), avg latency {v_avg_lat}ms"
        );
    } else {
        println!("  video: no frames received");
    }

    if a_frames > 0 {
        let a_avg_lat = a_latency / a_frames;
        let a_rate = a_bytes as f64 / elapsed_secs;
        println!(
            "  audio: {a_frames} frames, {a_bytes} bytes ({a_rate:.0} B/s), avg latency {a_avg_lat}ms"
        );
    } else {
        println!("  audio: no frames received");
    }

    let total = v_frames + a_frames;
    println!("  total: {total} frames");
}

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

    println!("=== MoQ Audio + Video Subscriber ===");
    println!("Relay:     {relay_url}");
    println!("Broadcast: {broadcast_path}");
    println!("Tracks:    {VIDEO_TRACK}, {AUDIO_TRACK}");
    println!();

    println!("Connecting to relay...");
    let session = MoqSubscribeSession::connect(config).await?;
    println!("Connected.");

    // Subscribe to both tracks before spawning reader tasks
    let video_reader = session.subscribe_track(VIDEO_TRACK)?;
    let audio_reader = session.subscribe_track(AUDIO_TRACK)?;
    println!("Subscribed to both tracks.\n");

    let video_stats = Arc::new(TrackStats::new());
    let audio_stats = Arc::new(TrackStats::new());
    let shutdown = Arc::new(AtomicBool::new(false));
    let start_time = std::time::Instant::now();

    let video_handle = {
        let stats = video_stats.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = read_track_frames(video_reader, VIDEO_TRACK, stats, shutdown).await {
                eprintln!("[video] Task error: {e}");
            }
        })
    };

    let audio_handle = {
        let stats = audio_stats.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = read_track_frames(audio_reader, AUDIO_TRACK, stats, shutdown).await {
                eprintln!("[audio] Task error: {e}");
            }
        })
    };

    // Periodic stats reporter
    let video_stats_periodic = video_stats.clone();
    let audio_stats_periodic = audio_stats.clone();
    let shutdown_periodic = shutdown.clone();
    let stats_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            if shutdown_periodic.load(Ordering::Relaxed) {
                break;
            }
            let elapsed = start_time.elapsed();
            print_summary(&video_stats_periodic, &audio_stats_periodic, elapsed);
        }
    });

    println!("Subscribing to tracks. Press Ctrl+C to stop.\n");

    // Wait for Ctrl+C or both tracks to end
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!("\nShutting down...");
            shutdown.store(true, Ordering::Relaxed);
        }
        _ = video_handle => {
            println!("[video] Track task finished.");
        }
        _ = audio_handle => {
            println!("[audio] Track task finished.");
        }
    }

    stats_handle.abort();
    print_summary(&video_stats, &audio_stats, start_time.elapsed());

    Ok(())
}
