// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WHEP (WebRTC-HTTP Egress Protocol) Client
//
// Unified client that owns both HTTP signaling and WebRTC session management.
// Implements IETF WHEP specification for WebRTC playback/egress.

use crate::core::{Result, StreamError};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

// ============================================================================
// WHEP CONFIGURATION
// ============================================================================

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WhepConfig {
    pub endpoint_url: String,
    /// Optional Bearer token for authentication.
    pub auth_token: Option<String>,
    pub timeout_ms: u64,
}

impl Default for WhepConfig {
    fn default() -> Self {
        Self {
            endpoint_url: String::new(),
            auth_token: None,
            timeout_ms: 10000,
        }
    }
}

// ============================================================================
// RTP SAMPLE
// ============================================================================

/// RTP sample received from WebRTC track.
#[derive(Debug, Clone)]
pub struct RtpSample {
    /// MIME type (e.g., "video/H264", "audio/opus")
    pub media_type: String,
    /// RTP payload data
    pub payload: Bytes,
    /// RTP timestamp
    pub timestamp: u32,
    /// RTP sequence number (for packet loss detection)
    pub sequence_number: u16,
}

// ============================================================================
// WHEP CLIENT
// ============================================================================

/// Unified WHEP client that owns HTTP signaling and WebRTC session.
///
/// This client is the single source of truth for a WHEP playback session.
/// It manages:
/// - HTTP signaling (POST offer, PATCH ICE candidates, DELETE terminate)
/// - WebRTC peer connection (receive-only mode)
/// - Media sample delivery via channels
///
/// Usage:
/// 1. Create client with `WhepClient::new(config)`
/// 2. Connect with `client.connect().await`
/// 3. Receive media with `client.try_recv_video()` / `try_recv_audio()`
/// 4. Terminate with `client.terminate().await`
pub struct WhepClient {
    config: WhepConfig,

    /// HTTP client with HTTPS support
    http_client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::combinators::BoxBody<
            bytes::Bytes,
            Box<dyn std::error::Error + Send + Sync>,
        >,
    >,

    /// Session URL (from Location header after POST success)
    session_url: Option<String>,

    /// RTCPeerConnection
    peer_connection: Option<Arc<webrtc::peer_connection::RTCPeerConnection>>,

    /// ICE candidate receiver (candidates collected from callback)
    ice_candidate_rx: Option<mpsc::Receiver<String>>,

    /// Video sample receiver (RTP packets from video track)
    video_sample_rx: Option<mpsc::Receiver<RtpSample>>,

    /// Audio sample receiver (RTP packets from audio track)
    audio_sample_rx: Option<mpsc::Receiver<RtpSample>>,

    /// Audio configuration from SDP negotiation
    audio_sample_rate: Option<u32>,
    audio_channels: Option<usize>,
}

impl WhepClient {
    /// Creates a new WHEP client.
    pub fn new(config: WhepConfig) -> Result<Self> {
        // Install rustls crypto provider if needed
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to install rustls crypto provider: {:?}",
                        e
                    ))
                })?;
        }

        tracing::info!(
            "[WhepClient] Creating client for endpoint: {}",
            config.endpoint_url
        );

        // Build HTTPS connector
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| StreamError::Configuration(format!("Failed to load CA roots: {}", e)))?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let http_client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .pool_idle_timeout(std::time::Duration::from_secs(30))
                .build(https);

        Ok(Self {
            config,
            http_client,
            session_url: None,
            peer_connection: None,
            ice_candidate_rx: None,
            video_sample_rx: None,
            audio_sample_rx: None,
            audio_sample_rate: None,
            audio_channels: None,
        })
    }

    /// Connects to the WHEP endpoint and establishes WebRTC session.
    pub async fn connect(&mut self) -> Result<()> {
        tracing::info!("[WhepClient] Connecting...");

        // Clean up any existing peer connection from a previous failed attempt
        if let Some(pc) = self.peer_connection.take() {
            tracing::debug!("[WhepClient] Closing previous peer connection before retry");
            let _ = pc.close().await;
        }
        self.ice_candidate_rx = None;
        self.video_sample_rx = None;
        self.audio_sample_rx = None;

        // Create peer connection and tracks
        let (peer_connection, ice_rx, video_rx, audio_rx) = self.create_peer_connection().await?;

        self.peer_connection = Some(peer_connection.clone());
        self.ice_candidate_rx = Some(ice_rx);
        self.video_sample_rx = Some(video_rx);
        self.audio_sample_rx = Some(audio_rx);

        // Create SDP offer
        let offer = match self.create_offer(&peer_connection).await {
            Ok(o) => o,
            Err(e) => {
                self.cleanup_peer_connection().await;
                return Err(e);
            }
        };

        tracing::debug!("[WhepClient] SDP offer:\n{}", offer);

        // POST offer to WHEP endpoint
        let answer = match self.post_offer(&offer).await {
            Ok(a) => a,
            Err(e) => {
                self.cleanup_peer_connection().await;
                return Err(e);
            }
        };

        tracing::debug!("[WhepClient] SDP answer:\n{}", answer);

        // Parse audio configuration from SDP answer
        self.parse_audio_config(&answer);

        // Set remote answer
        if let Err(e) = self.set_remote_answer(&peer_connection, &answer).await {
            self.cleanup_peer_connection().await;
            return Err(e);
        }

        // Wait briefly for ICE candidates to gather, then send them
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Send ICE candidates (trickle ICE)
        if let Err(e) = self.send_ice_candidates().await {
            tracing::debug!("[WhepClient] Trickle ICE not supported: {}", e);
        }

        tracing::info!("[WhepClient] Connected successfully");
        Ok(())
    }

    /// Cleans up peer connection and related resources on connection failure.
    async fn cleanup_peer_connection(&mut self) {
        if let Some(pc) = self.peer_connection.take() {
            let _ = pc.close().await;
        }
        self.ice_candidate_rx = None;
        self.video_sample_rx = None;
        self.audio_sample_rx = None;
    }

    /// Creates the WebRTC peer connection with receive-only transceivers.
    async fn create_peer_connection(
        &self,
    ) -> Result<(
        Arc<webrtc::peer_connection::RTCPeerConnection>,
        mpsc::Receiver<String>,
        mpsc::Receiver<RtpSample>,
        mpsc::Receiver<RtpSample>,
    )> {
        // Create MediaEngine and register codecs
        let mut media_engine = webrtc::api::media_engine::MediaEngine::default();

        // Register H.264 video codec
        media_engine
            .register_codec(
                webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                    capability: webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                        mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                        clock_rate: 90000,
                        channels: 0,
                        sdp_fmtp_line:
                            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                                .to_owned(),
                        rtcp_feedback: vec![],
                    },
                    payload_type: 102,
                    ..Default::default()
                },
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
            )
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to register H.264 codec: {}", e))
            })?;

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
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to register Opus codec: {}", e))
            })?;

        tracing::info!("[WhepClient] Registered H.264 (PT=102) and Opus (PT=111) codecs");

        // Create interceptor registry for RTCP
        let mut registry = webrtc::interceptor::registry::Registry::new();
        registry = webrtc::api::interceptor_registry::register_default_interceptors(
            registry,
            &mut media_engine,
        )
        .map_err(|e| {
            StreamError::Configuration(format!("Failed to register interceptors: {}", e))
        })?;

        // Create API
        let api = webrtc::api::APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // Create peer connection
        let config = webrtc::peer_connection::configuration::RTCConfiguration::default();
        let peer_connection = Arc::new(api.new_peer_connection(config).await.map_err(|e| {
            StreamError::Configuration(format!("Failed to create PeerConnection: {}", e))
        })?);

        // Create channels for ICE candidates and media samples
        let (ice_tx, ice_rx) = mpsc::channel::<String>(100);
        let (video_tx, video_rx) = mpsc::channel::<RtpSample>(1000);
        let (audio_tx, audio_rx) = mpsc::channel::<RtpSample>(1000);

        // Subscribe to ICE candidate events
        let ice_tx_clone = ice_tx.clone();
        peer_connection.on_ice_candidate(Box::new(move |candidate_opt| {
            let tx = ice_tx_clone.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate_opt {
                    if let Ok(json) = candidate.to_json() {
                        let sdp_fragment = format!("a={}", json.candidate);
                        tracing::debug!("[WhepClient] ICE candidate: {}", sdp_fragment);
                        let _ = tx.send(sdp_fragment).await;
                    }
                }
            })
        }));

        // Monitor peer connection state
        peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            Box::pin(async move {
                tracing::info!("[WhepClient] Peer connection state: {:?}", state);
            })
        }));

        // Monitor ICE connection state
        peer_connection.on_ice_connection_state_change(Box::new(move |state| {
            Box::pin(async move {
                tracing::info!("[WhepClient] ICE connection state: {:?}", state);
            })
        }));

        // Subscribe to on_track for receiving media
        let video_tx_clone = video_tx.clone();
        let audio_tx_clone = audio_tx.clone();
        peer_connection.on_track(Box::new(move |track, _receiver, _transceiver| {
            let mime_type = track.codec().capability.mime_type.clone();
            let is_video = mime_type.to_lowercase().contains("video");
            let tx = if is_video {
                video_tx_clone.clone()
            } else {
                audio_tx_clone.clone()
            };

            tracing::info!(
                "[WhepClient] Received track: {} ({})",
                mime_type,
                if is_video { "video" } else { "audio" }
            );

            Box::pin(async move {
                // Spawn a task to read RTP packets from this track
                let track_clone = track.clone();
                let mime = mime_type.clone();
                tokio::spawn(async move {
                    loop {
                        match track_clone.read_rtp().await {
                            Ok((rtp_packet, _attributes)) => {
                                let sample = RtpSample {
                                    media_type: mime.clone(),
                                    payload: rtp_packet.payload.clone(),
                                    timestamp: rtp_packet.header.timestamp,
                                    sequence_number: rtp_packet.header.sequence_number,
                                };
                                if tx.send(sample).await.is_err() {
                                    // Channel closed, exit
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "[WhepClient] Track read error (normal on close): {}",
                                    e
                                );
                                break;
                            }
                        }
                    }
                    tracing::debug!("[WhepClient] Track reader task exited: {}", mime);
                });
            })
        }));

        // Add recvonly transceivers for video and audio
        peer_connection
            .add_transceiver_from_kind(
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video,
                Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                    direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to add video transceiver: {}", e))
            })?;

        peer_connection
            .add_transceiver_from_kind(
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio,
                Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit {
                    direction: webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to add audio transceiver: {}", e))
            })?;

        Ok((peer_connection, ice_rx, video_rx, audio_rx))
    }

    /// Creates SDP offer.
    async fn create_offer(
        &self,
        peer_connection: &Arc<webrtc::peer_connection::RTCPeerConnection>,
    ) -> Result<String> {
        let offer = peer_connection
            .create_offer(None)
            .await
            .map_err(|e| StreamError::Runtime(format!("Failed to create offer: {}", e)))?;

        peer_connection
            .set_local_description(offer)
            .await
            .map_err(|e| StreamError::Runtime(format!("Failed to set local description: {}", e)))?;

        // Wait for ICE gathering to complete
        let mut done_rx = peer_connection.gathering_complete_promise().await;
        let _ = done_rx.recv().await;

        let local_desc = peer_connection
            .local_description()
            .await
            .ok_or_else(|| StreamError::Runtime("No local description".into()))?;

        Ok(local_desc.sdp)
    }

    /// Sets remote SDP answer.
    async fn set_remote_answer(
        &self,
        peer_connection: &Arc<webrtc::peer_connection::RTCPeerConnection>,
        sdp: &str,
    ) -> Result<()> {
        let answer =
            webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(
                sdp.to_owned(),
            )
            .map_err(|e| StreamError::Runtime(format!("Failed to parse SDP answer: {}", e)))?;

        peer_connection
            .set_remote_description(answer)
            .await
            .map_err(|e| {
                StreamError::Runtime(format!("Failed to set remote description: {}", e))
            })?;

        Ok(())
    }

    /// Parses audio configuration from SDP answer.
    fn parse_audio_config(&mut self, sdp: &str) {
        // Look for rtpmap line for Opus
        // Format: a=rtpmap:111 opus/48000/2
        if let Some(rtpmap_line) = sdp
            .lines()
            .find(|line| line.contains("rtpmap") && line.to_lowercase().contains("opus"))
        {
            tracing::info!("[WhepClient] Audio rtpmap: {}", rtpmap_line);

            if let Some(codec_info) = rtpmap_line.split_whitespace().nth(1) {
                let parts: Vec<&str> = codec_info.split('/').collect();
                if parts.len() >= 2 {
                    if let Ok(sample_rate) = parts[1].parse::<u32>() {
                        self.audio_sample_rate = Some(sample_rate);
                        tracing::info!("[WhepClient] Audio sample rate: {} Hz", sample_rate);
                    }

                    if parts.len() >= 3 {
                        if let Ok(channels) = parts[2].parse::<usize>() {
                            self.audio_channels = Some(channels);
                            tracing::info!("[WhepClient] Audio channels: {}", channels);
                        }
                    } else {
                        // RFC 7587: defaults to 1 (mono) if not specified
                        self.audio_channels = Some(1);
                        tracing::info!("[WhepClient] Audio channels: 1 (default)");
                    }
                }
            }
        }
    }

    /// POSTs SDP offer to WHEP endpoint.
    async fn post_offer(&mut self, sdp_offer: &str) -> Result<String> {
        use http_body_util::{BodyExt, Full};
        use hyper::{header, Request, StatusCode};

        let body = Full::new(bytes::Bytes::from(sdp_offer.to_owned()));
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let mut req_builder = Request::builder()
            .method("POST")
            .uri(&self.config.endpoint_url)
            .header(header::CONTENT_TYPE, "application/sdp");

        if let Some(token) = &self.config.auth_token {
            req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
        }

        let req = req_builder
            .body(boxed_body)
            .map_err(|e| StreamError::Runtime(format!("Failed to build request: {}", e)))?;

        tracing::debug!("[WhepClient] POST to {}", self.config.endpoint_url);

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.config.timeout_ms),
            self.http_client.request(req),
        )
        .await
        .map_err(|_| {
            StreamError::Runtime(format!(
                "WHEP POST timed out after {}ms",
                self.config.timeout_ms
            ))
        })?
        .map_err(|e| StreamError::Runtime(format!("WHEP POST failed: {}", e)))?;

        let status = response.status();
        let headers = response.headers().clone();

        let body_bytes = BodyExt::collect(response.into_body())
            .await
            .map_err(|e| StreamError::Runtime(format!("Failed to read response: {}", e)))?
            .to_bytes();

        match status {
            StatusCode::CREATED | StatusCode::NOT_ACCEPTABLE => {
                // Extract Location header
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        StreamError::Runtime(format!(
                            "WHEP {} without Location header",
                            status.as_u16()
                        ))
                    })?;

                // Convert relative to absolute URL
                self.session_url = if location.starts_with('/') {
                    let base_url = self
                        .config
                        .endpoint_url
                        .split('/')
                        .take(3)
                        .collect::<Vec<_>>()
                        .join("/");
                    Some(format!("{}{}", base_url, location))
                } else {
                    Some(location.to_owned())
                };

                tracing::info!(
                    "[WhepClient] Session created: {}",
                    self.session_url.as_ref().unwrap()
                );

                let sdp_answer = String::from_utf8(body_bytes.to_vec())
                    .map_err(|e| StreamError::Runtime(format!("Invalid UTF-8 in SDP: {}", e)))?;

                Ok(sdp_answer)
            }
            _ => {
                let error_body = String::from_utf8(body_bytes.to_vec())
                    .unwrap_or_else(|_| format!("HTTP {}", status));
                Err(StreamError::Runtime(format!(
                    "WHEP POST failed ({}): {}",
                    status, error_body
                )))
            }
        }
    }

    /// Sends pending ICE candidates via PATCH.
    async fn send_ice_candidates(&mut self) -> Result<()> {
        use http_body_util::{BodyExt, Full};
        use hyper::{header, Request, StatusCode};

        let session_url = match &self.session_url {
            Some(url) => url.clone(),
            None => return Ok(()),
        };

        // Collect all pending candidates from channel
        let mut candidates = Vec::new();
        if let Some(rx) = &mut self.ice_candidate_rx {
            while let Ok(candidate) = rx.try_recv() {
                candidates.push(candidate);
            }
        }

        if candidates.is_empty() {
            return Ok(());
        }

        tracing::info!("[WhepClient] Sending {} ICE candidates", candidates.len());

        let sdp_fragment = candidates.join("\r\n");

        let body = Full::new(bytes::Bytes::from(sdp_fragment));
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let mut req_builder = Request::builder()
            .method("PATCH")
            .uri(&session_url)
            .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag");

        if let Some(token) = &self.config.auth_token {
            req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
        }

        let req = req_builder
            .body(boxed_body)
            .map_err(|e| StreamError::Runtime(format!("Failed to build PATCH request: {}", e)))?;

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.config.timeout_ms),
            self.http_client.request(req),
        )
        .await
        .map_err(|_| StreamError::Runtime("WHEP PATCH timed out".into()))?
        .map_err(|e| StreamError::Runtime(format!("WHEP PATCH failed: {}", e)))?;

        match response.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => {
                tracing::debug!("[WhepClient] ICE candidates sent successfully");
                Ok(())
            }
            status => {
                let body_bytes = BodyExt::collect(response.into_body())
                    .await
                    .ok()
                    .and_then(|b| String::from_utf8(b.to_bytes().to_vec()).ok())
                    .unwrap_or_else(|| format!("HTTP {}", status));
                Err(StreamError::Runtime(format!(
                    "WHEP PATCH failed: {}",
                    body_bytes
                )))
            }
        }
    }

    /// Try to receive a video sample (non-blocking).
    pub fn try_recv_video(&mut self) -> Option<RtpSample> {
        self.video_sample_rx.as_mut()?.try_recv().ok()
    }

    /// Try to receive an audio sample (non-blocking).
    pub fn try_recv_audio(&mut self) -> Option<RtpSample> {
        self.audio_sample_rx.as_mut()?.try_recv().ok()
    }

    /// Get audio configuration from SDP negotiation.
    pub fn audio_config(&self) -> (Option<u32>, Option<usize>) {
        (self.audio_sample_rate, self.audio_channels)
    }

    /// Terminates the WHEP session.
    pub async fn terminate(&mut self) -> Result<()> {
        tracing::info!("[WhepClient] Terminating session...");

        // Close peer connection first (stops media flow)
        if let Some(pc) = self.peer_connection.take() {
            if let Err(e) = pc.close().await {
                tracing::warn!("[WhepClient] Error closing peer connection: {}", e);
            }
        }

        // Clear receivers (signals sender tasks to stop)
        self.ice_candidate_rx = None;
        self.video_sample_rx = None;
        self.audio_sample_rx = None;

        // Send DELETE to WHEP server
        if let Some(session_url) = self.session_url.take() {
            self.send_delete(&session_url).await?;
        }

        tracing::info!("[WhepClient] Session terminated");
        Ok(())
    }

    /// Sends DELETE request to terminate WHEP session.
    async fn send_delete(&self, session_url: &str) -> Result<()> {
        use http_body_util::{BodyExt, Empty};
        use hyper::{header, Request};

        let body = Empty::<bytes::Bytes>::new();
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let mut req_builder = Request::builder().method("DELETE").uri(session_url);

        if let Some(token) = &self.config.auth_token {
            req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
        }

        let req = req_builder
            .body(boxed_body)
            .map_err(|e| StreamError::Runtime(format!("Failed to build DELETE request: {}", e)))?;

        tracing::debug!("[WhepClient] DELETE to {}", session_url);

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.config.timeout_ms),
            self.http_client.request(req),
        )
        .await
        .map_err(|_| StreamError::Runtime("WHEP DELETE timed out".into()))?
        .map_err(|e| StreamError::Runtime(format!("WHEP DELETE failed: {}", e)))?;

        if response.status().is_success() {
            tracing::info!("[WhepClient] WHEP session deleted: {}", session_url);
        } else {
            tracing::warn!(
                "[WhepClient] DELETE returned {}, session may still exist server-side",
                response.status()
            );
        }

        Ok(())
    }
}
