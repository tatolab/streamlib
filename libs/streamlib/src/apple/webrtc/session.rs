// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WebRTC Session Management
//
// Manages WebRTC PeerConnection, tracks, and RTP packetization using webrtc-rs.
// Supports both send (WHIP) and receive (WHEP) modes.

use crate::core::{Result, StreamError};
use std::sync::Arc;
use webrtc::track::track_local::TrackLocalWriter;

/// WebRTC session mode (send-only vs receive-only)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRtcSessionMode {
    /// WHIP mode: Send video/audio to remote peer (sendonly transceivers)
    SendOnly,
    /// WHEP mode: Receive video/audio from remote peer (recvonly transceivers)
    ReceiveOnly,
}

/// Callback type for receiving RTP samples in WHEP mode
/// Arguments: (mime_type, payload, timestamp_rtp)
#[allow(dead_code)] // Used by WHEP mode (future implementation)
pub type SampleCallback = Arc<dyn Fn(String, bytes::Bytes, u32) + Send + Sync>;

pub struct WebRtcSession {
    /// Session mode (send-only for WHIP, receive-only for WHEP)
    #[allow(dead_code)] // Used for mode validation (future WHEP implementation)
    mode: WebRtcSessionMode,

    /// RTCPeerConnection (handles ICE, DTLS, RTP/RTCP)
    peer_connection: Arc<webrtc::peer_connection::RTCPeerConnection>,

    /// Video track (H.264 @ 90kHz) - using TrackLocalStaticRTP for manual PT control (WHIP only)
    video_track:
        Option<Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>>,

    /// Audio track (Opus @ 48kHz) - using TrackLocalStaticRTP for manual PT control (WHIP only)
    audio_track:
        Option<Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>>,

    /// Flag indicating ICE connection is ready (tracks are bound with correct PT)
    /// CRITICAL: Must be true before calling write_sample() to ensure PT is set correctly
    /// Set to true when ICE connection state becomes Connected
    #[allow(dead_code)] // Read by ICE callback handler
    ice_connected: Arc<std::sync::atomic::AtomicBool>,

    /// Tokio runtime for WebRTC background tasks
    /// CRITICAL: Must stay alive for session lifetime (ICE gathering, DTLS, stats)
    _runtime: tokio::runtime::Runtime,
}

impl WebRtcSession {
    /// Creates a new WebRTC session in SEND mode (WHIP).
    pub fn new<F>(on_ice_candidate: F) -> Result<Self>
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        // Create Tokio runtime for WebRTC background tasks
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2) // Minimal threads: 1 for blocking ops, 1 for background tasks
            .thread_name("webrtc-tokio")
            .enable_all()
            .build()
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to create Tokio runtime for WebRTC: {}", e))
            })?;

        tracing::info!("[WebRTC] Created Tokio runtime with 2 worker threads");

        // Block on async initialization within Tokio context
        let init_result = runtime.block_on(async {
            tracing::debug!("[WebRTC] Creating MediaEngine and registering codecs...");

            // Create MediaEngine and register only the codecs we use
            let mut media_engine = webrtc::api::media_engine::MediaEngine::default();

            // Register H.264 video codec (Baseline profile)
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                            clock_rate: 90000,
                            channels: 0,
                            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 102,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register H.264 codec: {}", e)))?;

            // Register Opus audio codec
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                            clock_rate: 48000,
                            channels: 2,
                            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 111,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register Opus codec: {}", e)))?;

            tracing::info!("[WebRTC] Registered ONLY H.264 (PT=102) and Opus (PT=111) codecs");

            tracing::debug!("[WebRTC] Creating interceptor registry...");

            // Create InterceptorRegistry for RTCP feedback (NACK, reports, stats)
            let mut registry = webrtc::interceptor::registry::Registry::new();
            registry = webrtc::api::interceptor_registry::register_default_interceptors(registry, &mut media_engine)
                .map_err(|e| StreamError::Configuration(format!("Failed to register interceptors: {}", e)))?;

            tracing::debug!("[WebRTC] Creating WebRTC API...");

            // Create API with MediaEngine and InterceptorRegistry
            let api = webrtc::api::APIBuilder::new()
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build();

            tracing::debug!("[WebRTC] Creating RTCPeerConnection...");

            // Create RTCPeerConnection
            let config = webrtc::peer_connection::configuration::RTCConfiguration::default();
            let peer_connection = Arc::new(
                api
                    .new_peer_connection(config)
                    .await
                    .map_err(|e| StreamError::Configuration(format!("Failed to create PeerConnection: {}", e)))?
            );

            tracing::debug!("[WebRTC] RTCPeerConnection created successfully");

            // Subscribe to ICE candidate events
            let on_candidate = Arc::new(on_ice_candidate);
            let pc_for_ice_candidate = Arc::clone(&peer_connection);
            pc_for_ice_candidate.on_ice_candidate(Box::new(move |candidate_opt| {
                let callback = Arc::clone(&on_candidate);
                Box::pin(async move {
                    if let Some(candidate) = candidate_opt {
                        if let Ok(json) = candidate.to_json() {
                            let sdp_fragment = format!("a={}", json.candidate);
                            tracing::debug!("ICE candidate discovered: {}", sdp_fragment);
                            callback(sdp_fragment);
                        }
                    } else {
                        tracing::debug!("ICE candidate gathering complete");
                    }
                })
            }));

            // Monitor signaling state changes
            peer_connection.on_signaling_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] 🔄 Signaling state: {:?}", state);
                })
            }));

            // Monitor peer connection state (includes DTLS handshake)
            peer_connection.on_peer_connection_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] ========================================");
                    tracing::info!("[WebRTC] 🔗 Peer connection state: {:?}", state);
                    tracing::info!("[WebRTC] ========================================");
                    match state {
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::New => {
                            tracing::debug!("[WebRTC] Peer connection: New");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connecting => {
                            tracing::info!("[WebRTC] Peer connection: Connecting... (DTLS handshake in progress)");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected => {
                            tracing::info!("[WebRTC] ✅✅✅ Peer connection: CONNECTED! ✅✅✅");
                            tracing::info!("[WebRTC] DTLS handshake completed successfully!");
                            tracing::info!("[WebRTC] RTP packets can now be sent/received!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected => {
                            tracing::warn!("[WebRTC] ⚠️  Peer connection: DISCONNECTED!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed => {
                            tracing::error!("[WebRTC] ❌❌❌ Peer connection: FAILED! ❌❌❌");
                            tracing::error!("[WebRTC] This means DTLS handshake or ICE failed!");
                        }
                        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed => {
                            tracing::info!("[WebRTC] Peer connection: Closed");
                        }
                        _ => {}
                    }
                })
            }));

            // Monitor ICE gathering state
            peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC] 🧊 ICE gathering state: {:?}", state);
                })
            }));

            // Monitor ICE connection state
            let ice_connected_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ice_connected_clone = Arc::clone(&ice_connected_flag);
            let pc_for_ice_handler = Arc::clone(&peer_connection);

            peer_connection.on_ice_connection_state_change(Box::new(move |connection_state| {
                let flag = Arc::clone(&ice_connected_clone);
                let pc = Arc::clone(&pc_for_ice_handler);
                Box::pin(async move {
                    tracing::info!("[WebRTC] ========================================");
                    tracing::info!("[WebRTC] ICE connection state changed: {:?}", connection_state);
                    tracing::info!("[WebRTC] ========================================");

                    if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected {
                        tracing::info!("[WebRTC] ✅ ICE Connected!");

                        // Verify transceiver payload types
                        let transceivers = pc.get_transceivers().await;
                        tracing::info!("[WebRTC] Verifying PT values for {} transceivers after ICE connection:", transceivers.len());

                        for (i, transceiver) in transceivers.iter().enumerate() {
                            let sender = transceiver.sender().await;
                            let params = sender.get_parameters().await;

                            tracing::info!("[WebRTC] Transceiver #{}: {} codec(s), PT={:?}",
                                i,
                                params.rtp_parameters.codecs.len(),
                                params.encodings.first().map(|e| e.payload_type));
                        }

                        flag.store(true, std::sync::atomic::Ordering::Release);
                        tracing::info!("[WebRTC] 🚀 Ready to send samples!");

                    } else if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Disconnected
                           || connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Failed {
                        tracing::warn!("[WebRTC] ❌ ICE connection lost: {:?}", connection_state);
                        flag.store(false, std::sync::atomic::Ordering::Release);
                    }
                })
            }));

            // Create video track (H.264)
            let video_track = Arc::new(
                webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                        clock_rate: 90000,
                        channels: 0,
                        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                        ..Default::default()
                    },
                    "video".to_owned(),
                    "streamlib-video".to_owned(),
                ),
            );

            // Add video track to PeerConnection
            let video_rtp_sender = peer_connection
                .add_track(Arc::clone(&video_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
                .await
                .map_err(|e| StreamError::Configuration(format!("Failed to add video track: {}", e)))?;

            let video_params = video_rtp_sender.get_parameters().await;

            // Log all codec parameters
            for (idx, codec) in video_params.rtp_parameters.codecs.iter().enumerate() {
                tracing::info!("[TELEMETRY:VIDEO_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                    idx,
                    codec.capability.mime_type,
                    codec.payload_type,
                    codec.capability.clock_rate,
                    codec.capability.channels,
                    codec.capability.sdp_fmtp_line);
            }

            // Log encoding parameters
            if let Some(enc) = video_params.encodings.first() {
                tracing::info!("[TELEMETRY:VIDEO_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                    enc.payload_type,
                    enc.ssrc,
                    enc.rid);
            } else {
                tracing::warn!("[TELEMETRY:VIDEO_ENCODING] NO_ENCODING_FOUND");
            }

            // Create audio track (Opus)
            let audio_track = Arc::new(
                webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                        clock_rate: 48000,
                        channels: 2,
                        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                        ..Default::default()
                    },
                    "audio".to_owned(),
                    "streamlib-audio".to_owned(),
                ),
            );

            // Add audio track to PeerConnection
            let audio_rtp_sender = peer_connection
                .add_track(Arc::clone(&audio_track) as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
                .await
                .map_err(|e| StreamError::Configuration(format!("Failed to add audio track: {}", e)))?;

            let audio_params = audio_rtp_sender.get_parameters().await;

            // Log all codec parameters
            for (idx, codec) in audio_params.rtp_parameters.codecs.iter().enumerate() {
                tracing::info!("[TELEMETRY:AUDIO_CODEC_{}] mime_type={}, pt={}, clock_rate={}, channels={}, fmtp='{}'",
                    idx,
                    codec.capability.mime_type,
                    codec.payload_type,
                    codec.capability.clock_rate,
                    codec.capability.channels,
                    codec.capability.sdp_fmtp_line);
            }

            // Log encoding parameters
            if let Some(enc) = audio_params.encodings.first() {
                tracing::info!("[TELEMETRY:AUDIO_ENCODING] pt={}, ssrc={:?}, rid={:?}",
                    enc.payload_type,
                    enc.ssrc,
                    enc.rid);
            } else {
                tracing::warn!("[TELEMETRY:AUDIO_ENCODING] NO_ENCODING_FOUND");
            }

            // Set transceivers to send-only (WHIP unidirectional publishing)
            let transceivers = peer_connection.get_transceivers().await;
            for transceiver in transceivers {
                if transceiver.sender().await.track().await.is_some() {
                    transceiver.set_direction(webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Sendonly).await;
                }
            }

            // Return tuple of (peer_connection, video_track, audio_track, ice_connected_flag)
            Ok::<_, StreamError>((peer_connection, video_track, audio_track, ice_connected_flag))
        })?;

        let (peer_connection, video_track, audio_track, ice_connected) = init_result;

        Ok(Self {
            mode: WebRtcSessionMode::SendOnly,
            peer_connection,
            video_track: Some(video_track),
            audio_track: Some(audio_track),
            ice_connected,
            _runtime: runtime,
        })
    }

    /// Creates a new WebRTC session in RECEIVE mode (WHEP).
    pub fn new_receive<F, S>(on_ice_candidate: F, on_sample: S) -> Result<Self>
    where
        F: Fn(String) + Send + Sync + 'static,
        S: Fn(String, bytes::Bytes, u32) + Send + Sync + 'static,
    {
        // Create Tokio runtime for WebRTC background tasks
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("webrtc-tokio-recv")
            .enable_all()
            .build()
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to create Tokio runtime for WebRTC: {}", e))
            })?;

        tracing::info!("[WebRTC WHEP] Created Tokio runtime with 2 worker threads");

        let on_sample = Arc::new(on_sample);

        // Block on async initialization
        let init_result = runtime.block_on(async {
            tracing::debug!("[WebRTC WHEP] Creating MediaEngine and registering codecs...");

            // Create MediaEngine and register only the codecs we use
            let mut media_engine = webrtc::api::media_engine::MediaEngine::default();

            // Register H.264 video codec (same as WHIP)
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                            clock_rate: 90000,
                            channels: 0,
                            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 102,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register H.264 codec: {}", e)))?;

            // Register Opus audio codec (same as WHIP)
            media_engine
                .register_codec(
                    webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                        capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                            mime_type: webrtc::api::media_engine::MIME_TYPE_OPUS.to_owned(),
                            clock_rate: 48000,
                            channels: 2,
                            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                            rtcp_feedback: vec![],
                        },
                        payload_type: 111,
                        ..Default::default()
                    },
                    webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
                )
                .map_err(|e| StreamError::Configuration(format!("Failed to register Opus codec: {}", e)))?;

            tracing::info!("[WebRTC WHEP] Registered H.264 (PT=102) and Opus (PT=111) codecs for receive");

            // Create InterceptorRegistry for RTCP feedback
            let mut registry = webrtc::interceptor::registry::Registry::new();
            registry = webrtc::api::interceptor_registry::register_default_interceptors(registry, &mut media_engine)
                .map_err(|e| StreamError::Configuration(format!("Failed to register interceptors: {}", e)))?;

            // Create API with MediaEngine and InterceptorRegistry
            let api = webrtc::api::APIBuilder::new()
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build();

            // Create RTCPeerConnection
            let config = webrtc::peer_connection::configuration::RTCConfiguration::default();
            let peer_connection = Arc::new(
                api
                    .new_peer_connection(config)
                    .await
                    .map_err(|e| StreamError::Configuration(format!("Failed to create PeerConnection: {}", e)))?
            );

            tracing::debug!("[WebRTC WHEP] RTCPeerConnection created successfully");

            // Subscribe to ICE candidate events
            let on_candidate = Arc::new(on_ice_candidate);
            let pc_for_ice_candidate = Arc::clone(&peer_connection);
            pc_for_ice_candidate.on_ice_candidate(Box::new(move |candidate_opt| {
                let callback = Arc::clone(&on_candidate);
                Box::pin(async move {
                    if let Some(candidate) = candidate_opt {
                        if let Ok(json) = candidate.to_json() {
                            let sdp_fragment = format!("a={}", json.candidate);
                            tracing::debug!("[WHEP] ICE candidate discovered: {}", sdp_fragment);
                            callback(sdp_fragment);
                        }
                    } else {
                        tracing::debug!("[WHEP] ICE candidate gathering complete");
                    }
                })
            }));

            // Monitor connection state changes (same as WHIP)
            peer_connection.on_signaling_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC WHEP] Signaling state: {:?}", state);
                })
            }));

            peer_connection.on_peer_connection_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC WHEP] Peer connection state: {:?}", state);
                })
            }));

            peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
                Box::pin(async move {
                    tracing::info!("[WebRTC WHEP] ICE gathering state: {:?}", state);
                })
            }));

            // Monitor ICE connection state
            let ice_connected_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ice_connected_clone = Arc::clone(&ice_connected_flag);

            peer_connection.on_ice_connection_state_change(Box::new(move |connection_state| {
                let flag = Arc::clone(&ice_connected_clone);
                Box::pin(async move {
                    tracing::info!("[WebRTC WHEP] ICE connection state: {:?}", connection_state);

                    if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected {
                        tracing::info!("[WebRTC WHEP] ICE Connected - ready to receive samples!");
                        flag.store(true, std::sync::atomic::Ordering::Release);
                    } else if connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Disconnected
                           || connection_state == webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Failed {
                        tracing::warn!("[WebRTC WHEP] ICE connection lost: {:?}", connection_state);
                        flag.store(false, std::sync::atomic::Ordering::Release);
                    }
                })
            }));

            // CRITICAL: Register on_track handler to receive incoming media streams
            //
            // NOTE: webrtc-rs read_rtp() returns RAW RTP payloads, NOT depacketized frames.
            // For H.264, this means FU-A fragmented packets that require reassembly.
            // For Opus, payloads are complete and can be decoded directly.
            //
            // The WHEP processor must handle:
            // - H.264: FU-A reassembly (RFC 6184) to reconstruct NAL units
            // - Opus: Direct decoding of RTP payload
            let on_sample_clone = Arc::clone(&on_sample);
            peer_connection.on_track(Box::new(move |track, _receiver, _transceiver| {
                let on_sample = Arc::clone(&on_sample_clone);
                let mime_type = track.codec().capability.mime_type.clone();

                tracing::info!(
                    "[WebRTC WHEP] Received track: kind={:?}, id={}, mime_type={}",
                    track.kind(),
                    track.id(),
                    mime_type
                );

                Box::pin(async move {
                    // Spawn task to read RTP samples from track
                    tokio::spawn(async move {
                        loop {
                            // Read RTP packet (webrtc-rs returns (Packet, attributes))
                            // IMPORTANT: Payload is RAW RTP data, not depacketized!
                            match track.read_rtp().await {
                                Ok((rtp_packet, _attributes)) => {
                                    // Extract payload and timestamp
                                    let payload = rtp_packet.payload.clone();
                                    let timestamp = rtp_packet.header.timestamp;

                                    tracing::trace!(
                                        "[WHEP] Received RTP: mime={}, size={}, ts={}, seq={}",
                                        mime_type,
                                        payload.len(),
                                        timestamp,
                                        rtp_packet.header.sequence_number
                                    );

                                    // Deliver RAW RTP payload to callback
                                    // Processor must handle FU-A reassembly for H.264
                                    on_sample(mime_type.clone(), payload, timestamp);
                                }
                                Err(e) => {
                                    tracing::warn!("[WHEP] Track read error: {}", e);
                                    break;
                                }
                            }
                        }
                    });
                })
            }));

            // Add recvonly transceivers for H.264 video and Opus audio
            // WHEP spec: Client creates SDP offer with recvonly direction
            peer_connection.add_transceiver_from_kind(
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
                Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                    direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                })
            ).await.map_err(|e| StreamError::Configuration(format!("Failed to add video transceiver: {}", e)))?;

            peer_connection.add_transceiver_from_kind(
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
                Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                    direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                })
            ).await.map_err(|e| StreamError::Configuration(format!("Failed to add audio transceiver: {}", e)))?;

            tracing::info!("[WebRTC WHEP] Added recvonly transceivers for video and audio");

            Ok::<_, StreamError>((peer_connection, ice_connected_flag))
        })?;

        let (peer_connection, ice_connected) = init_result;

        Ok(Self {
            mode: WebRtcSessionMode::ReceiveOnly,
            peer_connection,
            video_track: None, // No local tracks in receive mode
            audio_track: None,
            ice_connected,
            _runtime: runtime,
        })
    }

    /// Adds bandwidth attributes to SDP for WHIP compatibility.
    pub fn add_bandwidth_to_sdp(
        sdp: &str,
        video_bitrate_bps: u32,
        audio_bitrate_bps: u32,
    ) -> String {
        let mut result = String::new();
        let lines: Vec<&str> = sdp.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            result.push_str(line);
            result.push('\n');

            // Add bandwidth attributes after m=video line
            if line.starts_with("m=video") {
                // Add b=AS (Application-Specific bandwidth in kbps)
                let bitrate_kbps = video_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));

                // Add b=TIAS (Transport-Independent Application-Specific in bps)
                result.push_str(&format!("b=TIAS:{}\n", video_bitrate_bps));

                tracing::debug!(
                    "[WebRTC] Added video bandwidth: b=AS:{} b=TIAS:{}",
                    bitrate_kbps,
                    video_bitrate_bps
                );
            }
            // Add bandwidth attributes after m=audio line
            else if line.starts_with("m=audio") {
                // Add b=AS (Application-Specific bandwidth in kbps)
                let bitrate_kbps = audio_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));

                // Add b=TIAS (Transport-Independent Application-Specific in bps)
                result.push_str(&format!("b=TIAS:{}\n", audio_bitrate_bps));

                tracing::debug!(
                    "[WebRTC] Added audio bandwidth: b=AS:{} b=TIAS:{}",
                    bitrate_kbps,
                    audio_bitrate_bps
                );
            }

            i += 1;
        }

        result
    }

    /// Creates SDP offer for WHIP signaling.
    pub fn create_offer(&self) -> Result<String> {
        self._runtime.block_on(async {
            tracing::debug!("[WebRTC] Creating SDP offer...");

            let offer = self
                .peer_connection
                .create_offer(None)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to create offer: {}", e)))?;

            tracing::debug!("[WebRTC] Setting local description (starts ICE gathering)...");

            // Set local description (triggers ICE candidate gathering via mDNS)
            self.peer_connection
                .set_local_description(offer)
                .await
                .map_err(|e| {
                    StreamError::Runtime(format!("Failed to set local description: {}", e))
                })?;

            // Wait for ICE gathering to complete
            tracing::debug!("[WebRTC] Waiting for ICE gathering to complete...");

            let mut done_rx = self.peer_connection.gathering_complete_promise().await;
            let _ = done_rx.recv().await;

            tracing::debug!("[WebRTC] ICE gathering completed");

            // Get updated SDP with ICE candidates included
            let local_desc = self
                .peer_connection
                .local_description()
                .await
                .ok_or_else(|| StreamError::Runtime("No local description".into()))?;

            let candidate_count = local_desc.sdp.matches("a=candidate:").count();
            tracing::debug!(
                "[WebRTC] SDP offer created successfully with {} ICE candidates",
                candidate_count
            );
            Ok(local_desc.sdp)
        })
    }

    /// Sets remote SDP answer from WHIP/WHEP server.
    pub fn set_remote_answer(&mut self, sdp: &str) -> Result<()> {
        self._runtime.block_on(async {
            let mode_str = match self.mode {
                WebRtcSessionMode::SendOnly => "WHIP",
                WebRtcSessionMode::ReceiveOnly => "WHEP",
            };

            tracing::debug!("[WebRTC {}] Setting remote SDP answer...", mode_str);

            let answer = webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp.to_owned())
                .map_err(|e| StreamError::Runtime(format!("Failed to parse SDP answer: {}", e)))?;

            self.peer_connection
                .set_remote_description(answer)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to set remote description: {}", e)))?;

            tracing::debug!("[WebRTC {}] Remote SDP answer set successfully", mode_str);

            // Verify negotiated codecs
            let transceivers = self.peer_connection.get_transceivers().await;
            tracing::info!("[WebRTC {}] Configured {} transceivers after SDP negotiation", mode_str, transceivers.len());

            for (idx, transceiver) in transceivers.iter().enumerate() {
                let direction = transceiver.direction();

                if self.mode == WebRtcSessionMode::ReceiveOnly {
                    // WHEP mode: verify we're receiving the codecs we requested
                    let receiver = transceiver.receiver().await;
                    let params = receiver.get_parameters().await;

                    for codec in params.codecs.iter() {
                        tracing::info!(
                            "[WebRTC WHEP] Transceiver #{}: direction={:?}, mime={}, pt={}, clock={}, channels={}",
                            idx,
                            direction,
                            codec.capability.mime_type,
                            codec.payload_type,
                            codec.capability.clock_rate,
                            codec.capability.channels
                        );

                        // Verify we got H.264 or Opus as expected
                        match codec.capability.mime_type.as_str() {
                            "video/H264" => {
                                if codec.payload_type != 102 {
                                    tracing::warn!(
                                        "[WebRTC WHEP] Server negotiated H.264 with PT={}, we requested PT=102",
                                        codec.payload_type
                                    );
                                }
                            }
                            "audio/opus" => {
                                if codec.payload_type != 111 {
                                    tracing::warn!(
                                        "[WebRTC WHEP] Server negotiated Opus with PT={}, we requested PT=111",
                                        codec.payload_type
                                    );
                                }
                            }
                            other => {
                                tracing::warn!(
                                    "[WebRTC WHEP] Server negotiated unexpected codec: {} (PT={})",
                                    other,
                                    codec.payload_type
                                );
                            }
                        }
                    }
                } else {
                    // WHIP mode: log sender parameters
                    let sender = transceiver.sender().await;
                    let params = sender.get_parameters().await;

                    for codec in params.rtp_parameters.codecs.iter() {
                        tracing::info!(
                            "[WebRTC WHIP] Transceiver #{}: direction={:?}, mime={}, pt={}",
                            idx,
                            direction,
                            codec.capability.mime_type,
                            codec.payload_type
                        );
                    }
                }
            }

            Ok(())
        })
    }

    /// Validate and log H.264 NAL unit format
    #[allow(dead_code)]
    fn validate_and_log_h264_nal(sample_data: &[u8], sample_idx: usize) {
        if sample_data.len() < 5 {
            tracing::error!(
                "[H264 Validation] ❌ Sample {}: Too short ({} bytes, need ≥5)",
                sample_idx,
                sample_data.len()
            );
            return;
        }

        // Log first 8 bytes to identify format
        tracing::info!(
            "[H264 Validation] Sample {}: First 8 bytes: {:02X?}",
            sample_idx,
            &sample_data[..sample_data.len().min(8)]
        );

        // Check for Annex-B start codes (0x00 0x00 0x00 0x01 or 0x00 0x00 0x01)
        let is_annex_b = (sample_data.len() >= 4
            && sample_data[0] == 0x00
            && sample_data[1] == 0x00
            && sample_data[2] == 0x00
            && sample_data[3] == 0x01)
            || (sample_data.len() >= 3
                && sample_data[0] == 0x00
                && sample_data[1] == 0x00
                && sample_data[2] == 0x01);

        if is_annex_b {
            tracing::error!(
                "[H264 Validation] ❌❌❌ Sample {}: ANNEX-B FORMAT DETECTED!",
                sample_idx
            );
            tracing::error!(
                "[H264 Validation] WebRTC requires AVCC format (length-prefixed), not Annex-B!"
            );
            tracing::error!("[H264 Validation] This explains why Cloudflare receives no packets!");

            // Extract NAL unit type from after start code
            let nal_offset = if sample_data.len() >= 4 && sample_data[3] == 0x01 {
                4
            } else {
                3
            };
            if sample_data.len() > nal_offset {
                let nal_unit_type = sample_data[nal_offset] & 0x1F;
                tracing::error!(
                    "[H264 Validation] NAL type: {} (after Annex-B start code)",
                    nal_unit_type
                );
            }
            return;
        }

        // AVCC format: [4-byte length][NAL unit data]
        let nal_length = u32::from_be_bytes([
            sample_data[0],
            sample_data[1],
            sample_data[2],
            sample_data[3],
        ]) as usize;

        if nal_length + 4 != sample_data.len() {
            tracing::warn!(
                "[H264 Validation] ⚠️  Sample {}: NAL length mismatch (prefix says {}, actual {})",
                sample_idx,
                nal_length,
                sample_data.len() - 4
            );
        }

        // Extract NAL unit type from first byte of NAL data (after 4-byte length)
        let nal_unit_type = sample_data[4] & 0x1F;

        // Log NAL unit type
        match nal_unit_type {
            1 => tracing::trace!("[H264] Sample {}: Coded slice (non-IDR)", sample_idx),
            5 => tracing::info!("[H264] Sample {}: IDR (keyframe) ✅", sample_idx),
            6 => tracing::trace!("[H264] Sample {}: SEI", sample_idx),
            7 => tracing::info!(
                "[H264] Sample {}: SPS (Sequence Parameter Set) ✅",
                sample_idx
            ),
            8 => tracing::info!(
                "[H264] Sample {}: PPS (Picture Parameter Set) ✅",
                sample_idx
            ),
            9 => tracing::trace!("[H264] Sample {}: AUD (Access Unit Delimiter)", sample_idx),
            _ => tracing::debug!("[H264] Sample {}: NAL type {}", sample_idx, nal_unit_type),
        }
    }

    /// Writes video samples to the video track.
    pub fn write_video_samples(&mut self, samples: Vec<webrtc::media::Sample>) -> Result<()> {
        let track = self
            .video_track
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Video track not initialized".into()))?;

        // Track first write and periodic telemetry
        static VIDEO_SAMPLE_COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        static VIDEO_SEQ_NUM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static VIDEO_TIMESTAMP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

        let counter = VIDEO_SAMPLE_COUNTER
            .fetch_add(samples.len() as u64, std::sync::atomic::Ordering::Relaxed);

        if counter == 0 {
            tracing::info!("[WebRTC] 🎬 FIRST VIDEO WRITE after ICE Connected!");
            tracing::info!(
                "[WebRTC]    NAL units: {}, First NAL bytes: {}",
                samples.len(),
                samples.first().map(|s| s.data.len()).unwrap_or(0)
            );
        } else if counter.is_multiple_of(30) {
            tracing::debug!(
                "[TELEMETRY:VIDEO_SAMPLE_WRITE] sample_num={}, nal_count={}, total_bytes={}",
                counter,
                samples.len(),
                samples.iter().map(|s| s.data.len()).sum::<usize>()
            );
        }

        // RFC 6184 H.264 RTP Packetization
        // - Single NAL Unit mode: NAL < MTU (send as-is)
        // - FU-A mode: NAL >= MTU (fragment into multiple packets)
        use webrtc::rtp::header::Header as RtpHeader;
        use webrtc::rtp::packet::Packet as RtpPacket;

        const MAX_PAYLOAD_SIZE: usize = 1200; // Conservative MTU minus headers
        let timestamp_increment = 90000 / 30; // H.264 @ 90kHz, 30fps
        let current_timestamp = VIDEO_TIMESTAMP.load(std::sync::atomic::Ordering::Relaxed);

        for (i, sample) in samples.iter().enumerate() {
            let is_last_nal_in_frame = i == samples.len() - 1;

            // Decode NAL unit type (bits 0-4 of first byte)
            let nal_type = sample.data[0] & 0x1F;
            let nal_type_name = match nal_type {
                1 => "P-frame (non-IDR)",
                5 => "IDR (keyframe)",
                6 => "SEI",
                7 => "SPS",
                8 => "PPS",
                _ => "Other",
            };

            // Log NAL unit types for debugging decoder issues
            if counter <= 10 || nal_type == 5 || nal_type == 7 || nal_type == 8 {
                tracing::info!(
                    "[WebRTC] 🎬 NAL unit #{}: type={} ({}), size={} bytes",
                    counter,
                    nal_type,
                    nal_type_name,
                    sample.data.len()
                );
            }

            if sample.data.len() <= MAX_PAYLOAD_SIZE {
                // Single NAL Unit mode - send entire NAL as one RTP packet
                let seq_num = VIDEO_SEQ_NUM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                let rtp_packet = RtpPacket {
                    header: RtpHeader {
                        version: 2,
                        padding: false,
                        extension: false,
                        marker: is_last_nal_in_frame, // Mark last NAL of frame
                        payload_type: 102,            // H.264 (registered as PT=102)
                        sequence_number: seq_num as u16,
                        timestamp: current_timestamp,
                        ssrc: 0, // Will be set by track
                        ..Default::default()
                    },
                    payload: sample.data.clone(),
                };

                self._runtime.block_on(async {
                    track.write_rtp(&rtp_packet).await.map_err(|e| {
                        StreamError::Runtime(format!("Failed to write video RTP: {}", e))
                    })
                })?;

                if counter == 0 && i == 0 {
                    tracing::info!("[WebRTC] ✅ Successfully wrote first video RTP packet (Single NAL, {} bytes)", sample.data.len());
                } else if counter.is_multiple_of(30) && i == 0 {
                    tracing::info!(
                        "[WebRTC] 📊 Video RTP packet #{} sent (Single NAL, {} bytes)",
                        counter,
                        sample.data.len()
                    );
                }
            } else {
                // FU-A (Fragmentation Unit) mode - split NAL into multiple RTP packets
                // RFC 6184 Section 5.8: FU-A format

                let nal_header = sample.data[0]; // First byte is NAL header
                let nal_payload = &sample.data[1..]; // Rest is NAL payload

                // FU Indicator: F=0, NRI from NAL header, Type=28 (FU-A)
                let fu_indicator = (nal_header & 0xE0) | 28;

                // FU Header: S (start), E (end), R=0, Type from NAL header
                let nal_type = nal_header & 0x1F;

                let mut offset = 0;
                let mut frag_count = 0;

                while offset < nal_payload.len() {
                    let remaining = nal_payload.len() - offset;
                    let payload_size = remaining.min(MAX_PAYLOAD_SIZE - 2); // -2 for FU indicator + header

                    let is_start = offset == 0;
                    let is_end = offset + payload_size >= nal_payload.len();

                    // FU Header: S | E | R | Type
                    let fu_header = (if is_start { 0x80 } else { 0x00 }) |  // S bit
                        (if is_end { 0x40 } else { 0x00 }) |    // E bit
                        nal_type; // Type

                    // Build FU-A payload: FU indicator + FU header + NAL fragment
                    let mut fu_payload = Vec::with_capacity(2 + payload_size);
                    fu_payload.push(fu_indicator);
                    fu_payload.push(fu_header);
                    fu_payload.extend_from_slice(&nal_payload[offset..offset + payload_size]);

                    let seq_num = VIDEO_SEQ_NUM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    let rtp_packet = RtpPacket {
                        header: RtpHeader {
                            version: 2,
                            padding: false,
                            extension: false,
                            marker: is_end && is_last_nal_in_frame, // Mark last fragment of last NAL
                            payload_type: 102,
                            sequence_number: seq_num as u16,
                            timestamp: current_timestamp,
                            ssrc: 0,
                            ..Default::default()
                        },
                        payload: fu_payload.into(),
                    };

                    self._runtime.block_on(async {
                        track.write_rtp(&rtp_packet).await.map_err(|e| {
                            StreamError::Runtime(format!("Failed to write video RTP: {}", e))
                        })
                    })?;

                    if counter == 0 && i == 0 && frag_count == 0 {
                        tracing::info!("[WebRTC] ✅ Successfully wrote first video RTP packet (FU-A mode, NAL size {} bytes, fragments ~{})",
                            sample.data.len(),
                            sample.data.len().div_ceil(MAX_PAYLOAD_SIZE));
                    } else if counter.is_multiple_of(30) && i == 0 && frag_count == 0 {
                        tracing::info!(
                            "[WebRTC] 📊 Video RTP packet #{} sent (FU-A mode, NAL size {} bytes)",
                            counter,
                            sample.data.len()
                        );
                    }

                    offset += payload_size;
                    frag_count += 1;
                }
            }
        }

        // Increment timestamp for next frame
        VIDEO_TIMESTAMP.fetch_add(timestamp_increment, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }

    /// Writes audio sample to the audio track.
    pub fn write_audio_sample(&mut self, sample: webrtc::media::Sample) -> Result<()> {
        let track = self
            .audio_track
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Audio track not initialized".into()))?;

        // Track first write and periodic telemetry
        static AUDIO_SAMPLE_COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        static AUDIO_SEQ_NUM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static AUDIO_TIMESTAMP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

        let counter = AUDIO_SAMPLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if counter == 0 {
            tracing::info!("[WebRTC] 🎵 FIRST AUDIO WRITE after ICE Connected!");
            tracing::info!(
                "[WebRTC]    Bytes: {}, Duration: {:?}",
                sample.data.len(),
                sample.duration
            );
        } else if counter.is_multiple_of(50) {
            tracing::debug!(
                "[TELEMETRY:AUDIO_SAMPLE_WRITE] sample_num={}, bytes={}, duration_ms={:?}",
                counter,
                sample.data.len(),
                sample.duration.as_millis()
            );
        }

        use webrtc::rtp::header::Header as RtpHeader;
        use webrtc::rtp::packet::Packet as RtpPacket;

        let timestamp_increment = 960;

        let rtp_packet = RtpPacket {
            header: RtpHeader {
                version: 2,
                padding: false,
                extension: false,
                marker: false,
                payload_type: 111, // Opus (registered as PT=111)
                sequence_number: AUDIO_SEQ_NUM.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    as u16,
                timestamp: AUDIO_TIMESTAMP
                    .fetch_add(timestamp_increment, std::sync::atomic::Ordering::Relaxed),
                ssrc: 0, // Will be set by track
                ..Default::default()
            },
            payload: sample.data,
        };

        let result = self._runtime.block_on(async {
            track
                .write_rtp(&rtp_packet)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to write audio RTP: {}", e)))
        });

        if let Err(ref e) = result {
            tracing::error!("[WebRTC] ❌ Failed to write audio RTP {}: {}", counter, e);
        } else if counter == 0 {
            tracing::info!("[WebRTC] ✅ Successfully wrote first audio RTP packet with PT=111");
        } else if counter.is_multiple_of(50) {
            tracing::info!(
                "[WebRTC] 📊 Audio RTP packet #{} sent (PT=111, {} bytes)",
                counter,
                rtp_packet.payload.len()
            );
        }

        result.map(|_| ())
    }

    /// Gets RTCP statistics from the peer connection.
    pub fn get_stats(&self) -> Result<webrtc::stats::StatsReport> {
        self._runtime
            .block_on(async { Ok(self.peer_connection.get_stats().await) })
    }

    /// Closes the WebRTC session.
    pub fn close(&self) -> Result<()> {
        self._runtime.block_on(async {
            self.peer_connection.close().await.map_err(|e| {
                StreamError::Runtime(format!("Failed to close peer connection: {}", e))
            })
        })
    }
}
