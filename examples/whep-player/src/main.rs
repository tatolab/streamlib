//! WHEP (WebRTC-HTTP Egress Protocol) Player
//!
//! Connects to a WHEP endpoint to receive video/audio streams and logs detailed diagnostics.
//! This helps debug issues with the WHIP publisher.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error, debug};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::track::track_remote::TrackRemote;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    info!("=== WHEP Player - WebRTC Stream Receiver ===\n");

    // Get WHEP endpoint URL from environment or use default
    let whep_url = std::env::var("WHEP_URL")
        .unwrap_or_else(|_| {
            "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/play".to_string()
        });

    info!("📡 Connecting to WHEP endpoint:");
    info!("   {}\n", whep_url);

    // Create MediaEngine with H.264 and Opus codecs
    let mut media_engine = MediaEngine::default();

    // Register H.264 video codec
    media_engine.register_codec(
        webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: "video/H264".to_owned(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 102,
            ..Default::default()
        },
        webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
    )?;

    // Register Opus audio codec
    media_engine.register_codec(
        webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: "audio/opus".to_owned(),
                clock_rate: 48000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                rtcp_feedback: vec![],
            },
            payload_type: 111,
            ..Default::default()
        },
        webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
    )?;

    // Create InterceptorRegistry with default interceptors
    let mut registry = webrtc::api::interceptor_registry::Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)?;

    // Create API with MediaEngine
    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();

    // Create PeerConnection
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };

    let peer_connection = Arc::new(api.new_peer_connection(config).await?);
    info!("✅ Created PeerConnection\n");

    // Set up connection state handlers
    let pc = Arc::clone(&peer_connection);
    peer_connection.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
        info!("🔄 Peer connection state changed: {:?}", state);
        Box::pin(async move {})
    })).await;

    let pc = Arc::clone(&peer_connection);
    peer_connection.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
        info!("🧊 ICE connection state changed: {:?}", state);
        Box::pin(async move {})
    })).await;

    // Track counters
    let video_packet_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let audio_packet_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let video_bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let audio_bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Set up track handler to receive media
    let vpc = Arc::clone(&video_packet_count);
    let vb = Arc::clone(&video_bytes);
    let apc = Arc::clone(&audio_packet_count);
    let ab = Arc::clone(&audio_bytes);

    peer_connection.on_track(Box::new(move |track: Arc<TrackRemote>, _receiver: Arc<RTCRtpReceiver>| {
        let codec = track.codec();
        let track_kind = track.kind();
        let track_id = track.id().to_string();

        info!("\n📺 Received track:");
        info!("   Kind: {:?}", track_kind);
        info!("   ID: {}", track_id);
        info!("   Codec: {}", codec.capability.mime_type);
        info!("   Clock rate: {}", codec.capability.clock_rate);
        info!("   Payload type: {}", codec.payload_type);
        info!("   FMTP: {}\n", codec.capability.sdp_fmtp_line);

        let is_video = codec.capability.mime_type.contains("video");
        let packet_count = if is_video { Arc::clone(&vpc) } else { Arc::clone(&apc) };
        let byte_count = if is_video { Arc::clone(&vb) } else { Arc::clone(&ab) };
        let media_type = if is_video { "VIDEO" } else { "AUDIO" };

        tokio::spawn(async move {
            let mut last_seq: Option<u16> = None;
            let mut last_ts: Option<u32> = None;

            loop {
                match track.read_rtp().await {
                    Ok((rtp_packet, _)) => {
                        let count = packet_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        let bytes = byte_count.fetch_add(rtp_packet.payload.len() as u64, std::sync::atomic::Ordering::Relaxed) + rtp_packet.payload.len() as u64;

                        // Log first 10 packets with full details
                        if count <= 10 {
                            info!("[{}] 📦 Packet #{}: seq={}, ts={}, pt={}, marker={}, size={} bytes",
                                media_type,
                                count,
                                rtp_packet.header.sequence_number,
                                rtp_packet.header.timestamp,
                                rtp_packet.header.payload_type,
                                rtp_packet.header.marker,
                                rtp_packet.payload.len()
                            );

                            // For H.264 video, decode NAL unit type
                            if is_video && !rtp_packet.payload.is_empty() {
                                let first_byte = rtp_packet.payload[0];
                                let nal_type = first_byte & 0x1F;

                                let nal_desc = match nal_type {
                                    1 => "P-frame (non-IDR)",
                                    5 => "IDR (keyframe)",
                                    6 => "SEI",
                                    7 => "SPS",
                                    8 => "PPS",
                                    28 => {
                                        // FU-A fragmentation
                                        if rtp_packet.payload.len() >= 2 {
                                            let fu_header = rtp_packet.payload[1];
                                            let start = (fu_header & 0x80) != 0;
                                            let end = (fu_header & 0x40) != 0;
                                            let frag_type = fu_header & 0x1F;
                                            format!("FU-A fragment (type={}, S={}, E={})", frag_type, start, end)
                                        } else {
                                            "FU-A (malformed)".to_string()
                                        }
                                    }
                                    _ => format!("Unknown ({})", nal_type),
                                };

                                info!("   └─ NAL unit type: {} (0x{:02x})", nal_desc, nal_type);
                            }
                        } else if count % 100 == 0 {
                            // Log every 100th packet
                            info!("[{}] 📊 Packet #{}: {} KB received", media_type, count, bytes / 1024);
                        }

                        // Check for sequence number gaps
                        if let Some(last) = last_seq {
                            let expected = last.wrapping_add(1);
                            if rtp_packet.header.sequence_number != expected {
                                warn!("[{}] ⚠️  Sequence gap detected: expected {}, got {} (lost {} packets)",
                                    media_type, expected, rtp_packet.header.sequence_number,
                                    rtp_packet.header.sequence_number.wrapping_sub(expected));
                            }
                        }
                        last_seq = Some(rtp_packet.header.sequence_number);

                        // Check timestamp jumps
                        if let Some(last) = last_ts {
                            let delta = rtp_packet.header.timestamp.wrapping_sub(last);
                            if is_video {
                                // For H.264 @ 90kHz, 30fps = 3000 ticks
                                if delta > 10000 {
                                    warn!("[{}] ⚠️  Large timestamp jump: {} ticks ({:.1}ms)",
                                        media_type, delta, delta as f64 / 90.0);
                                }
                            } else {
                                // For Opus @ 48kHz, 20ms = 960 ticks
                                if delta > 5000 {
                                    warn!("[{}] ⚠️  Large timestamp jump: {} ticks ({:.1}ms)",
                                        media_type, delta, delta as f64 / 48.0);
                                }
                            }
                        }
                        last_ts = Some(rtp_packet.header.timestamp);
                    }
                    Err(e) => {
                        error!("[{}] ❌ Error reading RTP packet: {}", media_type, e);
                        break;
                    }
                }
            }
        });

        Box::pin(async {})
    })).await;

    // Add transceivers for video and audio (recvonly)
    peer_connection.add_transceiver_from_kind(
        webrtc::rtp_transceiver::RTCRtpTransceiverInit {
            direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        },
        webrtc::rtp_transceiver::RTPCodecType::Video,
    ).await?;

    peer_connection.add_transceiver_from_kind(
        webrtc::rtp_transceiver::RTCRtpTransceiverInit {
            direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        },
        webrtc::rtp_transceiver::RTPCodecType::Audio,
    ).await?;

    info!("✅ Added recvonly transceivers for video and audio\n");

    // Create offer
    let offer = peer_connection.create_offer(None).await?;
    info!("📝 Created offer SDP:");
    debug!("{}\n", offer.sdp);

    // Set local description
    peer_connection.set_local_description(offer.clone()).await?;
    info!("✅ Set local description\n");

    // Send offer to WHEP endpoint
    info!("📤 Sending offer to WHEP endpoint...");
    let client = reqwest::Client::new();
    let response = client
        .post(&whep_url)
        .header("Content-Type", "application/sdp")
        .body(offer.sdp)
        .send()
        .await
        .context("Failed to send offer to WHEP endpoint")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await?;
        anyhow::bail!("WHEP endpoint returned error {}: {}", status, body);
    }

    // Get answer SDP
    let answer_sdp = response.text().await?;
    info!("✅ Received answer from WHEP endpoint\n");
    info!("📝 Answer SDP:");
    debug!("{}\n", answer_sdp);

    // Set remote description
    let answer = RTCSessionDescription::answer(answer_sdp)?;
    peer_connection.set_remote_description(answer).await?;
    info!("✅ Set remote description\n");

    info!("🎬 Waiting for media packets...\n");

    // Wait for connection and media
    let mut last_video = 0u64;
    let mut last_audio = 0u64;
    let mut no_data_count = 0u8;

    for i in 0..120 {
        sleep(Duration::from_secs(1)).await;

        let video = video_packet_count.load(std::sync::atomic::Ordering::Relaxed);
        let audio = audio_packet_count.load(std::sync::atomic::Ordering::Relaxed);
        let vbytes = video_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let abytes = audio_bytes.load(std::sync::atomic::Ordering::Relaxed);

        if i > 0 && i % 10 == 0 {
            info!("\n📊 Status after {}s:", i);
            info!("   Video: {} packets, {} KB", video, vbytes / 1024);
            info!("   Audio: {} packets, {} KB\n", audio, abytes / 1024);
        }

        // Check if we're receiving data
        if video == last_video && audio == last_audio {
            no_data_count += 1;
            if no_data_count >= 10 {
                warn!("⚠️  No data received for 10 seconds!");
                if video == 0 && audio == 0 {
                    error!("❌ No packets received at all. Check:");
                    error!("   1. Is the WHIP publisher running?");
                    error!("   2. Is ICE connection established?");
                    error!("   3. Are tracks properly attached?");
                }
                no_data_count = 0;
            }
        } else {
            no_data_count = 0;
        }

        last_video = video;
        last_audio = audio;
    }

    info!("\n✅ Test complete!");
    info!("Final stats:");
    info!("   Video: {} packets, {} KB",
        video_packet_count.load(std::sync::atomic::Ordering::Relaxed),
        video_bytes.load(std::sync::atomic::Ordering::Relaxed) / 1024);
    info!("   Audio: {} packets, {} KB",
        audio_packet_count.load(std::sync::atomic::Ordering::Relaxed),
        audio_bytes.load(std::sync::atomic::Ordering::Relaxed) / 1024);

    peer_connection.close().await?;
    Ok(())
}
