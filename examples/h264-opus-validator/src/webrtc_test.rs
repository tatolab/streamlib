// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// Minimal WebRTC audio-only test with PSYCHO MODE logging
///
/// Tests WebRTC in complete isolation from streamlib to verify it's not a threading bug.
/// Generates synthetic audio and streams via WHIP to Cloudflare Stream.
use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::time::{interval, Duration};
use webrtc::track::track_local::TrackLocalWriter;

#[tokio::main]
pub async fn main() -> Result<()> {
    // Initialize rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize tracing with DEBUG level
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    tracing::info!("🚀🚀🚀 WEBRTC ISOLATION TEST - PSYCHO LOGGING MODE 🚀🚀🚀");
    tracing::info!("========================================================");

    // Cloudflare Stream WHIP endpoint
    let whip_url = "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish";

    // Step 1: Create MediaEngine with H.264 + Opus codecs
    tracing::info!("📦 Step 1: Creating MediaEngine...");
    let mut media_engine = webrtc::api::media_engine::MediaEngine::default();

    // Register H.264 video codec (PT=96)
    media_engine.register_codec(
        webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f".to_owned(),
                ..Default::default()
            },
            payload_type: 96,
            ..Default::default()
        },
        webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
    )?;

    // Register Opus audio codec (PT=111)
    media_engine.register_codec(
        webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
            capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                ..Default::default()
            },
            payload_type: 111,
            ..Default::default()
        },
        webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
    )?;
    tracing::info!("✅ MediaEngine created with H.264 (PT=96) + Opus (PT=111)");

    // Step 2: Create InterceptorRegistry for RTCP
    tracing::info!("📦 Step 2: Creating InterceptorRegistry...");
    let mut registry = webrtc::interceptor::registry::Registry::new();
    registry = webrtc::api::interceptor_registry::register_default_interceptors(registry, &mut media_engine)?;
    tracing::info!("✅ InterceptorRegistry created with default interceptors (NACK, RTCP reports)");

    // Step 3: Create API
    tracing::info!("📦 Step 3: Creating WebRTC API...");
    let api = webrtc::api::APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();
    tracing::info!("✅ WebRTC API created");

    // Step 4: Create PeerConnection
    tracing::info!("📦 Step 4: Creating RTCPeerConnection...");
    let config = webrtc::peer_connection::configuration::RTCConfiguration {
        ice_servers: vec![],
        ..Default::default()
    };

    let peer_connection = Arc::new(api.new_peer_connection(config).await?);
    tracing::info!("✅ RTCPeerConnection created");

    // Step 5: Register ALL event handlers with MAXIMUM logging
    tracing::info!("📦 Step 5: Registering event handlers (PSYCHO MODE)...");

    // 5.1: ICE candidate handler with trickle-ice support
    let ice_connected = Arc::new(AtomicBool::new(false));
    let ice_connected_clone = Arc::clone(&ice_connected);

    // Store full candidate JSON for later sending
    let candidates = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let candidates_clone = Arc::clone(&candidates);
    let session_url = Arc::new(tokio::sync::Mutex::new(None::<String>));
    let session_url_for_candidates = Arc::clone(&session_url);

    peer_connection.on_ice_candidate(Box::new(move |candidate_opt| {
        let candidates = Arc::clone(&candidates_clone);
        let session_url = Arc::clone(&session_url_for_candidates);
        Box::pin(async move {
            if let Some(candidate) = candidate_opt {
                let json = candidate.to_json().unwrap();
                tracing::info!("🧊 [ICE] Candidate discovered: {}", json.candidate);
                tracing::debug!("🧊 [ICE]   - Protocol: {}", candidate.protocol);
                tracing::debug!("🧊 [ICE]   - Address: {}", candidate.address);
                tracing::debug!("🧊 [ICE]   - Port: {}", candidate.port);
                tracing::debug!("🧊 [ICE]   - Priority: {}", candidate.priority);

                // Store full JSON for later trickle-ice sending
                let mut c = candidates.lock().await;
                c.push(json.clone());
                tracing::debug!("🧊 [ICE] Total candidates: {}", c.len());
                drop(c); // Release lock before PATCH

                // Send candidate via PATCH if we have a session URL (WHIP trickle-ice)
                if let Some(url) = session_url.lock().await.as_ref() {
                    let mid_line = format!("a=mid:{}", json.sdp_mid.clone().unwrap_or("0".to_string()));
                    let mline_index = json.sdp_mline_index.unwrap_or(0);

                    // WHIP trickle-ice format: SDP fragment with candidate
                    let sdp_frag = format!("m-line-index:{}\n{}\na={}\n",
                        mline_index, mid_line, json.candidate);

                    tracing::debug!("🧊 [TRICKLE ICE] Sending candidate via PATCH to {}", url);
                    match send_ice_candidate(url, &sdp_frag).await {
                        Ok(()) => tracing::info!("✅ [TRICKLE ICE] Candidate sent successfully"),
                        Err(e) => tracing::error!("❌ [TRICKLE ICE] Failed to send candidate: {:?}", e),
                    }
                } else {
                    tracing::debug!("🧊 [TRICKLE ICE] Session URL not yet available, candidate queued");
                }
            } else {
                tracing::info!("🧊 [ICE] Candidate gathering COMPLETE");
            }
        })
    }));

    // 5.2: Signaling state handler
    peer_connection.on_signaling_state_change(Box::new(move |state| {
        Box::pin(async move {
            tracing::info!("🔄 [SIGNALING] State changed: {:?}", state);
        })
    }));

    // 5.3: ICE connection state handler (CRITICAL)
    let pc_for_ice = Arc::clone(&peer_connection);
    peer_connection.on_ice_connection_state_change(Box::new(move |state| {
        let flag = Arc::clone(&ice_connected_clone);
        let pc = Arc::clone(&pc_for_ice);
        Box::pin(async move {
            tracing::info!("===============================================");
            tracing::info!("🧊 [ICE CONNECTION] State: {:?}", state);
            tracing::info!("===============================================");

            match state {
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::New => {
                    tracing::info!("🧊 [ICE] NEW - Starting ICE checks");
                }
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Checking => {
                    tracing::info!("🧊 [ICE] CHECKING - Performing connectivity checks");
                }
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected => {
                    tracing::info!("🧊 [ICE] ✅✅✅ CONNECTED - Media can flow! ✅✅✅");
                    flag.store(true, Ordering::SeqCst);

                    // Log transceiver info
                    let transceivers = pc.get_transceivers().await;
                    tracing::info!("🔍 [ICE CONNECTED] Checking {} transceivers:", transceivers.len());
                    for (i, t) in transceivers.iter().enumerate() {
                        let sender = t.sender().await;
                        if let Some(track) = sender.track().await {
                            tracing::info!("🔍 [ICE CONNECTED]   Transceiver {}: track={}, kind={:?}",
                                i, track.id(), track.kind());
                        }

                        // Check parameters
                        let params = sender.get_parameters().await;
                        tracing::info!("🔍 [ICE CONNECTED]   RTP Parameters:");
                        for (idx, encoding) in params.encodings.iter().enumerate() {
                            tracing::info!("🔍 [ICE CONNECTED]     Encoding {}: ssrc={:?}, pt={}",
                                idx, encoding.ssrc, encoding.payload_type);
                        }
                        for (idx, codec) in params.rtp_parameters.codecs.iter().enumerate() {
                            tracing::info!("🔍 [ICE CONNECTED]     Codec {}: mime={}, pt={}, clock={}",
                                idx, codec.capability.mime_type, codec.payload_type, codec.capability.clock_rate);
                        }
                    }
                }
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Completed => {
                    tracing::info!("🧊 [ICE] COMPLETED - All checks done");
                }
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Failed => {
                    tracing::error!("🧊 [ICE] ❌ FAILED - Connection failed!");
                }
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Disconnected => {
                    tracing::warn!("🧊 [ICE] DISCONNECTED - Connection lost");
                    flag.store(false, Ordering::SeqCst);
                }
                webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Closed => {
                    tracing::info!("🧊 [ICE] CLOSED - Connection closed");
                    flag.store(false, Ordering::SeqCst);
                }
                _ => {}
            }
        })
    }));

    // 5.4: ICE gathering state handler
    peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
        Box::pin(async move {
            tracing::info!("🧊 [ICE GATHERING] State: {:?}", state);
            match state {
                webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::New => {
                    tracing::debug!("🧊 [ICE GATHERING] NEW");
                }
                webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Gathering => {
                    tracing::info!("🧊 [ICE GATHERING] GATHERING - Collecting candidates");
                }
                webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete => {
                    tracing::info!("🧊 [ICE GATHERING] ✅ COMPLETE - All candidates collected");
                }
                _ => {}
            }
        })
    }));

    // 5.5: Peer connection state handler (CRITICAL for DTLS)
    peer_connection.on_peer_connection_state_change(Box::new(move |state| {
        Box::pin(async move {
            tracing::info!("===============================================");
            tracing::info!("🔗 [PEER CONNECTION] State: {:?}", state);
            tracing::info!("===============================================");

            match state {
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::New => {
                    tracing::info!("🔗 [PEER] NEW");
                }
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connecting => {
                    tracing::info!("🔗 [PEER] CONNECTING - DTLS handshake in progress");
                }
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected => {
                    tracing::info!("🔗 [PEER] ✅✅✅ CONNECTED - DTLS complete, RTP can flow! ✅✅✅");
                }
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected => {
                    tracing::warn!("🔗 [PEER] ⚠️  DISCONNECTED");
                }
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed => {
                    tracing::error!("🔗 [PEER] ❌ FAILED");
                }
                webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed => {
                    tracing::info!("🔗 [PEER] CLOSED");
                }
                _ => {}
            }
        })
    }));

    // 5.6: Track handler (data channel, etc.)
    peer_connection.on_track(Box::new(move |track, _receiver, _transceiver| {
        Box::pin(async move {
            tracing::info!("📻 [TRACK] New track: id={}, kind={:?}", track.id(), track.kind());
        })
    }));

    // 5.7: Data channel handler
    peer_connection.on_data_channel(Box::new(move |channel| {
        Box::pin(async move {
            tracing::info!("📡 [DATA CHANNEL] New channel: label={}", channel.label());
        })
    }));

    // 5.8: Negotiation needed handler
    peer_connection.on_negotiation_needed(Box::new(move || {
        Box::pin(async move {
            tracing::warn!("🔄 [NEGOTIATION] Renegotiation needed!");
        })
    }));

    tracing::info!("✅ All event handlers registered");

    // Step 6: Create audio track (using TrackLocalStaticRTP for WHIP)
    tracing::info!("📦 Step 6: Creating audio track...");
    tracing::info!("🔧 Using TrackLocalStaticRTP for WHIP (unidirectional) streaming");

    let codec_capability = webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
        mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        ..Default::default()
    };

    let audio_track = Arc::new(
        webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
            codec_capability,
            "audio".to_owned(),
            "webrtc-test-audio".to_owned(),
        ),
    );
    tracing::info!("✅ Audio track created: id=audio, stream=webrtc-test-audio");
    tracing::info!("   Track type: TrackLocalStaticRTP (manual RTP packet construction)");

    // Step 6b: Create video track (using TrackLocalStaticRTP for WHIP)
    tracing::info!("📦 Step 6b: Creating video track...");
    let video_codec_capability = webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
        mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
        clock_rate: 90000,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f".to_owned(),
        ..Default::default()
    };

    let video_track = Arc::new(
        webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
            video_codec_capability,
            "video".to_owned(),
            "webrtc-test-video".to_owned(),
        ),
    );
    tracing::info!("✅ Video track created: id=video, stream=webrtc-test-video");

    // Step 7: Add tracks to peer connection
    tracing::info!("📦 Step 7: Adding video + audio tracks to PeerConnection...");

    let video_sender = peer_connection
        .add_track(Arc::clone(&video_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
        .await?;
    tracing::info!("✅ Video track added");
    tracing::debug!("🔍 [TRACK] Video RTPSender created: {:?}", video_sender);

    let audio_sender = peer_connection
        .add_track(Arc::clone(&audio_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
        .await?;
    tracing::info!("✅ Audio track added");
    tracing::debug!("🔍 [TRACK] Audio RTPSender created: {:?}", audio_sender);

    // Step 8: Set transceivers to SendOnly
    tracing::info!("📦 Step 8: Configuring transceivers for SendOnly (WHIP ingestion)...");
    for transceiver in peer_connection.get_transceivers().await {
        if transceiver.sender().await.track().await.is_some() {
            transceiver
                .set_direction(webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Sendonly)
                .await;
            tracing::debug!("🔍 [TRANSCEIVER] Set to SendOnly");
        }
    }
    tracing::info!("✅ All transceivers set to SendOnly");

    // Step 9: Create SDP offer
    tracing::info!("📦 Step 9: Creating SDP offer...");
    let offer = peer_connection.create_offer(None).await?;
    tracing::info!("✅ SDP offer created");
    tracing::debug!("🔍 [SDP OFFER] Type: {:?}", offer.sdp_type);

    // Step 10: Set local description (triggers ICE gathering)
    tracing::info!("📦 Step 10: Setting local description (triggers ICE gathering)...");
    peer_connection.set_local_description(offer.clone()).await?;
    tracing::info!("✅ Local description set - ICE gathering started");

    // Step 11: Wait for ICE gathering to complete
    tracing::info!("⏳ Step 11: Waiting for ICE gathering to complete...");
    let mut done_rx = peer_connection.gathering_complete_promise().await;
    let _ = done_rx.recv().await;
    tracing::info!("✅ ICE gathering completed");

    // Step 12: Get updated SDP with ICE candidates
    let final_offer = peer_connection.local_description().await
        .expect("Local description should be set");
    tracing::info!("📦 Step 12: Retrieved final SDP offer with ICE candidates");

    let candidate_count = final_offer.sdp.matches("a=candidate:").count();
    tracing::info!("🧊 [ICE] Final offer contains {} candidates", candidate_count);

    // Step 13: Send WHIP request with candidates included
    tracing::info!("📦 Step 13: Sending WHIP POST request...");
    let (whip_session_url, answer_sdp) = whip_publish(whip_url, &final_offer.sdp).await?;
    tracing::info!("✅ WHIP session created: {}", whip_session_url);

    // NOTE: Cloudflare Stream does NOT support WHIP trickle-ICE (returns 400 Bad Request).
    // All ICE candidates must be included in the initial SDP offer above.

    // Step 13: Set remote description
    tracing::info!("📦 Step 15: Setting remote description...");
    let answer = webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(answer_sdp)?;
    peer_connection.set_remote_description(answer).await?;
    tracing::info!("✅ Remote description set - ICE checks starting");

    // Step 16: Wait for ICE connection
    tracing::info!("⏳ Step 16: Waiting for ICE connection...");
    for i in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if ice_connected.load(Ordering::SeqCst) {
            tracing::info!("✅ ICE connected after {} seconds", i + 1);
            break;
        }
        tracing::debug!("⏳ Still waiting for ICE... ({}/30)", i + 1);
    }

    if !ice_connected.load(Ordering::SeqCst) {
        tracing::error!("❌ ICE connection failed after 30 seconds");
        return Ok(());
    }

    // Step 17: SKIP track binding check - we're using TrackLocalStaticRTP with manual PT=96/111
    tracing::info!("📦 Step 17: Skipping track binding check (using manual RTP with PT=96/111)");
    tracing::info!("🔍 [TRACK BINDING] TrackLocalStaticRTP doesn't need binding - PT set manually in packets");

    // Step 18: Initialize Opus encoder for audio
    tracing::info!("📦 Step 18: Initializing Opus encoder...");
    let mut audio_encoder = opus::Encoder::new(48000, opus::Channels::Stereo, opus::Application::Audio)?;
    audio_encoder.set_bitrate(opus::Bitrate::Bits(128_000))?;
    audio_encoder.set_vbr(false)?; // CBR for consistent streaming
    audio_encoder.set_inband_fec(true)?;
    tracing::info!("✅ Opus encoder initialized: 48kHz stereo, 128kbps CBR, FEC enabled");

    // Step 19: Start concurrent video + audio streaming
    tracing::info!("📦 Step 19: Starting video + audio streaming...");

    let audio_packets_sent = Arc::new(AtomicU64::new(0));
    let video_packets_sent = Arc::new(AtomicU64::new(0));

    // Spawn audio task
    let audio_task = {
        let audio_track = Arc::clone(&audio_track);
        let ice_connected = Arc::clone(&ice_connected);
        let audio_packets_sent = Arc::clone(&audio_packets_sent);

        tokio::spawn(async move {
            tracing::info!("🎵 [AUDIO TASK] Starting (440Hz stereo sine wave)");

            let sample_rate = 48000;
            let frame_duration_ms = 20; // 20ms frames
            let frame_size = (sample_rate * frame_duration_ms) / 1000; // 960 samples
            let frequency_l = 440.0; // Left channel: A4
            let frequency_r = 554.37; // Right channel: C#5

            let mut sample_index = 0u64;
            let mut frame_count = 0u64;
            let mut ticker = interval(Duration::from_millis(frame_duration_ms as u64));

            loop {
                ticker.tick().await;

                if !ice_connected.load(Ordering::SeqCst) {
                    tracing::warn!("⚠️  [AUDIO] ICE disconnected, stopping");
                    break;
                }

                // Generate stereo audio samples
                let mut samples = vec![0i16; (frame_size * 2) as usize];
                for i in 0..frame_size {
                    let t = (sample_index + i as u64) as f32 / sample_rate as f32;
                    let sample_l = (2.0 * std::f32::consts::PI * frequency_l * t).sin() * 16000.0;
                    let sample_r = (2.0 * std::f32::consts::PI * frequency_r * t).sin() * 16000.0;
                    samples[(i * 2) as usize] = sample_l as i16;
                    samples[(i * 2 + 1) as usize] = sample_r as i16;
                }
                sample_index += frame_size as u64;

                // Encode with Opus
                let mut encoded = vec![0u8; 4000];
                let encoded_len = match audio_encoder.encode(&samples, &mut encoded) {
                    Ok(len) => len,
                    Err(e) => {
                        tracing::error!("❌ [AUDIO] Opus encoding failed: {:?}", e);
                        continue;
                    }
                };
                encoded.truncate(encoded_len);

                if frame_count % 50 == 0 {
                    tracing::info!("🎵 [AUDIO] Frame {}: {} samples → {} bytes Opus",
                        frame_count, frame_size * 2, encoded_len);
                }

                // Construct RTP packet with PT=111
                use webrtc::rtp::packet::Packet as RtpPacket;
                use webrtc::rtp::header::Header as RtpHeader;

                let rtp_packet = RtpPacket {
                    header: RtpHeader {
                        version: 2,
                        padding: false,
                        extension: false,
                        marker: false,
                        payload_type: 111,  // Opus
                        sequence_number: (frame_count & 0xFFFF) as u16,
                        timestamp: (sample_index * 48000 / frame_size as u64) as u32,
                        ssrc: 0,
                        ..Default::default()
                    },
                    payload: encoded.into(),
                };

                match audio_track.write_rtp(&rtp_packet).await {
                    Ok(bytes_written) => {
                        let count = audio_packets_sent.fetch_add(1, Ordering::SeqCst) + 1;
                        if count % 50 == 0 {
                            tracing::info!("📤 [AUDIO RTP] Sent packet {} with PT=111 ({} bytes, {} total)",
                                           frame_count, bytes_written, count);
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ [AUDIO RTP] Write failed: {:?}", e);
                    }
                }

                frame_count += 1;
                if frame_count >= 12000 {
                    tracing::info!("✅ [AUDIO] Complete: sent {} frames", frame_count);
                    break;
                }
            }
        })
    };

    // Spawn video task (simple color bars test pattern)
    let video_task = {
        let video_track = Arc::clone(&video_track);
        let ice_connected = Arc::clone(&ice_connected);
        let video_packets_sent = Arc::clone(&video_packets_sent);

        tokio::spawn(async move {
            tracing::info!("📹 [VIDEO TASK] Starting (synthetic H.264 frames)");

            let fps = 30u32;
            let frame_duration_ms = 1000 / fps;
            let mut ticker = interval(Duration::from_millis(frame_duration_ms as u64));
            let mut frame_count = 0u64;
            let video_clock_rate = 90000u32; // H.264 uses 90kHz clock
            let timestamp_increment = video_clock_rate / fps;

            loop {
                ticker.tick().await;

                if !ice_connected.load(Ordering::SeqCst) {
                    tracing::warn!("⚠️  [VIDEO] ICE disconnected, stopping");
                    break;
                }

                // Generate a minimal valid H.264 NAL unit (IDR frame every 60 frames, P-frames otherwise)
                let is_keyframe = frame_count % 60 == 0;
                let nal_unit = if is_keyframe {
                    // IDR frame: Start code + IDR NAL unit (type 5)
                    vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, 0x10]
                } else {
                    // P-frame: Start code + Non-IDR NAL unit (type 1)
                    vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9a, 0x20, 0x08, 0x0a]
                };

                if frame_count % 30 == 0 {
                    let frame_type = if is_keyframe { "IDR" } else { "P" };
                    tracing::info!("📹 [VIDEO] Frame {}: {} frame, {} bytes",
                        frame_count, frame_type, nal_unit.len());
                }

                // Construct RTP packet with PT=96
                use webrtc::rtp::packet::Packet as RtpPacket;
                use webrtc::rtp::header::Header as RtpHeader;

                let rtp_packet = RtpPacket {
                    header: RtpHeader {
                        version: 2,
                        padding: false,
                        extension: false,
                        marker: true,  // Mark end of frame
                        payload_type: 96,  // H.264
                        sequence_number: (frame_count & 0xFFFF) as u16,
                        timestamp: (frame_count * timestamp_increment as u64) as u32,
                        ssrc: 0,
                        ..Default::default()
                    },
                    payload: nal_unit.into(),
                };

                match video_track.write_rtp(&rtp_packet).await {
                    Ok(bytes_written) => {
                        let count = video_packets_sent.fetch_add(1, Ordering::SeqCst) + 1;
                        if count % 30 == 0 {
                            tracing::info!("📤 [VIDEO RTP] Sent packet {} with PT=96 ({} bytes, {} total)",
                                           frame_count, bytes_written, count);
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ [VIDEO RTP] Write failed: {:?}", e);
                    }
                }

                frame_count += 1;
                if frame_count >= 7200 { // 7200 frames @ 30fps = 240 seconds (4 minutes)
                    tracing::info!("✅ [VIDEO] Complete: sent {} frames", frame_count);
                    break;
                }
            }
        })
    };

    tracing::info!("🚀 Both audio and video tasks started");

    // Wait for both tasks to complete
    let (audio_result, video_result) = tokio::join!(audio_task, video_task);

    if let Err(e) = audio_result {
        tracing::error!("❌ Audio task panicked: {:?}", e);
    }
    if let Err(e) = video_result {
        tracing::error!("❌ Video task panicked: {:?}", e);
    }

    let total_audio_sent = audio_packets_sent.load(Ordering::SeqCst);
    let total_video_sent = video_packets_sent.load(Ordering::SeqCst);
    tracing::info!("📊 FINAL STATS:");
    tracing::info!("   Audio RTP packets sent: {}", total_audio_sent);
    tracing::info!("   Video RTP packets sent: {}", total_video_sent);
    tracing::info!("   Total RTP packets: {}", total_audio_sent + total_video_sent);

    // Cleanup
    tracing::info!("🧹 Closing peer connection...");
    peer_connection.close().await?;
    tracing::info!("✅ Test complete!");

    Ok(())
}

/// Sends WHIP POST request to create session
async fn whip_publish(endpoint_url: &str, offer_sdp: &str) -> Result<(String, String)> {
    use hyper::{Request, header, StatusCode};
    use http_body_util::{BodyExt, Full};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    tracing::debug!("🌐 [WHIP] Building HTTPS connector...");
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()?
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();

    let client: Client<_, Full<bytes::Bytes>> = Client::builder(TokioExecutor::new()).build(https);
    tracing::debug!("🌐 [WHIP] HTTPS client created");

    let body = Full::new(bytes::Bytes::from(offer_sdp.to_owned()));

    let req = Request::builder()
        .method("POST")
        .uri(endpoint_url)
        .header(header::CONTENT_TYPE, "application/sdp")
        .body(body)?;

    tracing::info!("🌐 [WHIP] Sending POST to {}", endpoint_url);

    let response = tokio::time::timeout(
        Duration::from_secs(10),
        client.request(req),
    )
    .await
    .context("WHIP POST timeout")??;

    let status = response.status();
    tracing::info!("🌐 [WHIP] Response status: {}", status);

    if status != StatusCode::CREATED {
        let body_bytes = response.into_body().collect().await?.to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);
        tracing::error!("🌐 [WHIP] Error response: {}", body_str);
        anyhow::bail!("WHIP POST failed: {} - {}", status, body_str);
    }

    // Extract Location header for session URL
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .context("Missing Location header")?;

    let session_url = if location.starts_with("http") {
        location.to_string()
    } else {
        // Relative URL - construct absolute
        let base = url::Url::parse(endpoint_url)?;
        base.join(location)?.to_string()
    };

    tracing::info!("🌐 [WHIP] Session URL: {}", session_url);

    // Extract SDP answer from body
    let body_bytes = response.into_body().collect().await?.to_bytes();
    let answer_sdp = String::from_utf8(body_bytes.to_vec())
        .context("Invalid UTF-8 in SDP answer")?;

    tracing::debug!("🌐 [WHIP] Received {} bytes SDP answer", answer_sdp.len());

    Ok((session_url, answer_sdp))
}

/// Sends ICE candidate via PATCH request (WHIP trickle-ice)
async fn send_ice_candidate(session_url: &str, sdp_fragment: &str) -> Result<()> {
    use hyper::{Request, header, StatusCode};
    use http_body_util::{BodyExt, Full};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()?
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();

    let client: Client<_, Full<bytes::Bytes>> = Client::builder(TokioExecutor::new()).build(https);

    let body = Full::new(bytes::Bytes::from(sdp_fragment.to_owned()));

    let req = Request::builder()
        .method("PATCH")
        .uri(session_url)
        .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag")
        .body(body)?;

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client.request(req),
    )
    .await
    .context("PATCH timeout")??;

    let status = response.status();

    if status != StatusCode::NO_CONTENT && status != StatusCode::OK {
        let body_bytes = response.into_body().collect().await?.to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);
        tracing::error!("🌐 [TRICKLE ICE] PATCH failed: {} - {}", status, body_str);
        anyhow::bail!("ICE candidate PATCH failed: {}", status);
    }

    Ok(())
}
