// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// WHIP (WebRTC-HTTP Ingestion Protocol) Client
//
// Unified client that owns both HTTP signaling and WebRTC session management.
// Implements RFC 9725 WHIP signaling for WebRTC streaming.

use crate::core::{Result, StreamError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use webrtc::track::track_local::TrackLocalWriter;

// ============================================================================
// WHIP CONFIGURATION
// ============================================================================

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WhipConfig {
    pub endpoint_url: String,
    /// Optional Bearer token for authentication.
    pub auth_token: Option<String>,
    pub timeout_ms: u64,
}

impl Default for WhipConfig {
    fn default() -> Self {
        Self {
            endpoint_url: String::new(),
            auth_token: None,
            timeout_ms: 10000,
        }
    }
}

// ============================================================================
// WHIP CLIENT
// ============================================================================

/// Unified WHIP client that owns HTTP signaling and WebRTC session.
///
/// This client is the single source of truth for a WHIP streaming session.
/// It manages:
/// - HTTP signaling (POST offer, PATCH ICE candidates, DELETE terminate)
/// - WebRTC peer connection and media tracks
/// - ICE candidate collection and transmission
///
/// Usage:
/// 1. Create client with `WhipClient::new(config)`
/// 2. Connect with `client.connect().await`
/// 3. Send media with `client.write_video_samples()` / `client.write_audio_sample()`
/// 4. Terminate with `client.terminate().await`
pub struct WhipClient {
    config: WhipConfig,

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

    /// Video track (H.264 @ 90kHz)
    video_track:
        Option<Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>>,

    /// Audio track (Opus @ 48kHz)
    audio_track:
        Option<Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>>,

    /// ICE candidate receiver (candidates collected from callback)
    ice_candidate_rx: Option<mpsc::Receiver<String>>,

    /// Video RTP state
    video_seq_num: u32,
    video_timestamp: u32,
    video_sample_count: u64,

    /// Audio RTP state
    audio_seq_num: u32,
    audio_timestamp: u32,
    audio_sample_count: u64,
}

impl WhipClient {
    /// Creates a new WHIP client.
    pub fn new(config: WhipConfig) -> Result<Self> {
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
            "[WhipClient] Creating client for endpoint: {}",
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
            video_track: None,
            audio_track: None,
            ice_candidate_rx: None,
            // RTP sequence numbers and timestamps should start at random values for security
            // Use simple time-based seeds since we don't have rand crate
            video_seq_num: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u32)
                ^ 0xDEAD_BEEF,
            video_timestamp: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u32)
                ^ 0xCAFE_BABE,
            video_sample_count: 0,
            audio_seq_num: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u32)
                ^ 0xBEEF_CAFE,
            audio_timestamp: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u32)
                ^ 0xFACE_FEED,
            audio_sample_count: 0,
        })
    }

    /// Connects to the WHIP endpoint and establishes WebRTC session.
    ///
    /// This creates the peer connection, generates SDP offer, posts to WHIP endpoint,
    /// and sets the remote answer.
    pub async fn connect(&mut self, video_bitrate_bps: u32, audio_bitrate_bps: u32) -> Result<()> {
        tracing::info!("[WhipClient] Connecting...");

        // Create peer connection and tracks
        let (peer_connection, video_track, audio_track, ice_rx) =
            self.create_peer_connection().await?;

        self.peer_connection = Some(peer_connection.clone());
        self.video_track = Some(video_track);
        self.audio_track = Some(audio_track);
        self.ice_candidate_rx = Some(ice_rx);

        // Create SDP offer
        let offer = self.create_offer(&peer_connection).await?;
        let offer_with_bandwidth =
            Self::add_bandwidth_to_sdp(&offer, video_bitrate_bps, audio_bitrate_bps);

        tracing::info!("[WhipClient] ========== SDP OFFER ==========");
        for (i, line) in offer_with_bandwidth.lines().enumerate() {
            tracing::debug!("[WhipClient] OFFER [{}]: {}", i, line);
        }

        // POST offer to WHIP endpoint
        let answer = self.post_offer(&offer_with_bandwidth).await?;

        tracing::info!("[WhipClient] ========== SDP ANSWER ==========");
        for (i, line) in answer.lines().enumerate() {
            tracing::debug!("[WhipClient] ANSWER [{}]: {}", i, line);
        }

        // Set remote answer
        self.set_remote_answer(&peer_connection, &answer).await?;

        // Send any buffered ICE candidates (trickle ICE)
        if let Err(e) = self.send_ice_candidates().await {
            tracing::debug!("[WhipClient] Trickle ICE not supported: {}", e);
        }

        tracing::info!("[WhipClient] Connected successfully");
        Ok(())
    }

    /// Creates the WebRTC peer connection with video and audio tracks.
    async fn create_peer_connection(
        &self,
    ) -> Result<(
        Arc<webrtc::peer_connection::RTCPeerConnection>,
        Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>,
        Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>,
        mpsc::Receiver<String>,
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

        tracing::info!("[WhipClient] Registered H.264 (PT=102) and Opus (PT=111) codecs");

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

        // Create ICE candidate channel
        let (ice_tx, ice_rx) = mpsc::channel::<String>(100);

        // Subscribe to ICE candidate events
        let ice_tx_clone = ice_tx.clone();
        peer_connection.on_ice_candidate(Box::new(move |candidate_opt| {
            let tx = ice_tx_clone.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate_opt {
                    if let Ok(json) = candidate.to_json() {
                        let sdp_fragment = format!("a={}", json.candidate);
                        tracing::debug!("[WhipClient] ICE candidate: {}", sdp_fragment);
                        let _ = tx.send(sdp_fragment).await;
                    }
                }
            })
        }));

        // Monitor peer connection state
        peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            Box::pin(async move {
                tracing::info!("[WhipClient] Peer connection state: {:?}", state);
            })
        }));

        // Monitor ICE connection state
        peer_connection.on_ice_connection_state_change(Box::new(move |state| {
            Box::pin(async move {
                tracing::info!("[WhipClient] ICE connection state: {:?}", state);
            })
        }));

        // Monitor ICE gathering state
        peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
            Box::pin(async move {
                tracing::info!("[WhipClient] ICE gathering state: {:?}", state);
            })
        }));

        // Create video track
        let video_track = Arc::new(
            webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
                webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                    mime_type: webrtc::api::media_engine::MIME_TYPE_H264.to_owned(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line:
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                            .to_owned(),
                    ..Default::default()
                },
                "video".to_owned(),
                "streamlib-video".to_owned(),
            ),
        );

        peer_connection
            .add_track(Arc::clone(&video_track)
                as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
            .await
            .map_err(|e| StreamError::Configuration(format!("Failed to add video track: {}", e)))?;

        // Create audio track
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

        peer_connection
            .add_track(Arc::clone(&audio_track)
                as Arc<dyn webrtc::track::track_local::TrackLocal + Send + Sync>)
            .await
            .map_err(|e| StreamError::Configuration(format!("Failed to add audio track: {}", e)))?;

        // Set transceivers to send-only
        let transceivers = peer_connection.get_transceivers().await;
        for transceiver in transceivers {
            if transceiver.sender().await.track().await.is_some() {
                transceiver.set_direction(
                    webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection::Sendonly,
                ).await;
            }
        }

        Ok((peer_connection, video_track, audio_track, ice_rx))
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

    /// Adds bandwidth attributes to SDP.
    fn add_bandwidth_to_sdp(sdp: &str, video_bitrate_bps: u32, audio_bitrate_bps: u32) -> String {
        let mut result = String::new();

        for line in sdp.lines() {
            result.push_str(line);
            result.push('\n');

            if line.starts_with("m=video") {
                let bitrate_kbps = video_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));
                result.push_str(&format!("b=TIAS:{}\n", video_bitrate_bps));
            } else if line.starts_with("m=audio") {
                let bitrate_kbps = audio_bitrate_bps / 1000;
                result.push_str(&format!("b=AS:{}\n", bitrate_kbps));
                result.push_str(&format!("b=TIAS:{}\n", audio_bitrate_bps));
            }
        }

        result
    }

    /// POSTs SDP offer to WHIP endpoint.
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

        tracing::debug!("[WhipClient] POST to {}", self.config.endpoint_url);

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.config.timeout_ms),
            self.http_client.request(req),
        )
        .await
        .map_err(|_| {
            StreamError::Runtime(format!(
                "WHIP POST timed out after {}ms",
                self.config.timeout_ms
            ))
        })?
        .map_err(|e| StreamError::Runtime(format!("WHIP POST failed: {}", e)))?;

        let status = response.status();
        let headers = response.headers().clone();

        let body_bytes = BodyExt::collect(response.into_body())
            .await
            .map_err(|e| StreamError::Runtime(format!("Failed to read response: {}", e)))?
            .to_bytes();

        match status {
            StatusCode::CREATED => {
                // Extract Location header
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        StreamError::Runtime("WHIP 201 without Location header".into())
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
                    "[WhipClient] Session created: {}",
                    self.session_url.as_ref().unwrap()
                );

                let sdp_answer = String::from_utf8(body_bytes.to_vec())
                    .map_err(|e| StreamError::Runtime(format!("Invalid UTF-8 in SDP: {}", e)))?;

                Ok(sdp_answer)
            }
            StatusCode::TEMPORARY_REDIRECT => {
                let location = headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| StreamError::Runtime("307 without Location header".into()))?;

                tracing::info!("[WhipClient] Redirecting to: {}", location);
                self.config.endpoint_url = location.to_owned();
                Box::pin(self.post_offer(sdp_offer)).await
            }
            _ => {
                let error_body = String::from_utf8(body_bytes.to_vec())
                    .unwrap_or_else(|_| format!("HTTP {}", status));
                Err(StreamError::Runtime(format!(
                    "WHIP POST failed ({}): {}",
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
        .map_err(|_| StreamError::Runtime("WHIP PATCH timed out".into()))?
        .map_err(|e| StreamError::Runtime(format!("WHIP PATCH failed: {}", e)))?;

        match response.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => {
                tracing::debug!("[WhipClient] Sent {} ICE candidates", candidates.len());
                Ok(())
            }
            status => {
                let body_bytes = BodyExt::collect(response.into_body())
                    .await
                    .ok()
                    .and_then(|b| String::from_utf8(b.to_bytes().to_vec()).ok())
                    .unwrap_or_else(|| format!("HTTP {}", status));
                Err(StreamError::Runtime(format!(
                    "WHIP PATCH failed: {}",
                    body_bytes
                )))
            }
        }
    }

    /// Writes video samples to the WebRTC track.
    pub async fn write_video_samples(&mut self, samples: Vec<webrtc::media::Sample>) -> Result<()> {
        // Clone the Arc to avoid borrowing self while iterating
        let track = self
            .video_track
            .clone()
            .ok_or_else(|| StreamError::Runtime("Video track not initialized".into()))?;

        const MAX_PAYLOAD_SIZE: usize = 1200;
        let timestamp_increment = 90000 / 30; // 90kHz @ 30fps

        for (i, sample) in samples.iter().enumerate() {
            let is_last_nal = i == samples.len() - 1;
            let nal_type = sample.data[0] & 0x1F;

            // Log first few samples and keyframes
            if self.video_sample_count < 10 || nal_type == 5 || nal_type == 7 || nal_type == 8 {
                let nal_name = match nal_type {
                    1 => "P-frame",
                    5 => "IDR",
                    7 => "SPS",
                    8 => "PPS",
                    _ => "Other",
                };
                tracing::info!(
                    "[WhipClient] Video NAL #{}: {} ({} bytes)",
                    self.video_sample_count,
                    nal_name,
                    sample.data.len()
                );
            }

            if sample.data.len() <= MAX_PAYLOAD_SIZE {
                // Single NAL unit mode
                self.write_video_single_nal(&track, &sample.data, is_last_nal)
                    .await?;
            } else {
                // FU-A fragmentation mode
                self.write_video_fua(&track, &sample.data, is_last_nal)
                    .await?;
            }
        }

        self.video_timestamp = self.video_timestamp.wrapping_add(timestamp_increment);
        self.video_sample_count += samples.len() as u64;

        Ok(())
    }

    /// Writes a single NAL unit as one RTP packet.
    async fn write_video_single_nal(
        &mut self,
        track: &Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>,
        nal_data: &[u8],
        is_last_nal: bool,
    ) -> Result<()> {
        use webrtc::rtp::header::Header as RtpHeader;
        use webrtc::rtp::packet::Packet as RtpPacket;

        let rtp_packet = RtpPacket {
            header: RtpHeader {
                version: 2,
                padding: false,
                extension: false,
                marker: is_last_nal,
                payload_type: 102,
                sequence_number: self.video_seq_num as u16,
                timestamp: self.video_timestamp,
                ssrc: 0,
                ..Default::default()
            },
            payload: bytes::Bytes::copy_from_slice(nal_data),
        };

        self.video_seq_num = self.video_seq_num.wrapping_add(1);

        track
            .write_rtp(&rtp_packet)
            .await
            .map_err(|e| StreamError::Runtime(format!("Failed to write video RTP: {}", e)))?;

        Ok(())
    }

    /// Writes a large NAL unit using FU-A fragmentation.
    async fn write_video_fua(
        &mut self,
        track: &Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>,
        nal_data: &[u8],
        is_last_nal: bool,
    ) -> Result<()> {
        use webrtc::rtp::header::Header as RtpHeader;
        use webrtc::rtp::packet::Packet as RtpPacket;

        const MAX_PAYLOAD_SIZE: usize = 1200;

        let nal_header = nal_data[0];
        let nal_payload = &nal_data[1..];

        // FU Indicator: F=0, NRI from NAL header, Type=28 (FU-A)
        let fu_indicator = (nal_header & 0xE0) | 28;
        let nal_type = nal_header & 0x1F;

        let mut offset = 0;

        while offset < nal_payload.len() {
            let remaining = nal_payload.len() - offset;
            let payload_size = remaining.min(MAX_PAYLOAD_SIZE - 2);

            let is_start = offset == 0;
            let is_end = offset + payload_size >= nal_payload.len();

            // FU Header: S | E | R | Type
            let fu_header = (if is_start { 0x80 } else { 0x00 })
                | (if is_end { 0x40 } else { 0x00 })
                | nal_type;

            let mut fu_payload = Vec::with_capacity(2 + payload_size);
            fu_payload.push(fu_indicator);
            fu_payload.push(fu_header);
            fu_payload.extend_from_slice(&nal_payload[offset..offset + payload_size]);

            let rtp_packet = RtpPacket {
                header: RtpHeader {
                    version: 2,
                    padding: false,
                    extension: false,
                    marker: is_end && is_last_nal,
                    payload_type: 102,
                    sequence_number: self.video_seq_num as u16,
                    timestamp: self.video_timestamp,
                    ssrc: 0,
                    ..Default::default()
                },
                payload: fu_payload.into(),
            };

            self.video_seq_num = self.video_seq_num.wrapping_add(1);

            track
                .write_rtp(&rtp_packet)
                .await
                .map_err(|e| StreamError::Runtime(format!("Failed to write video RTP: {}", e)))?;

            offset += payload_size;
        }

        Ok(())
    }

    /// Writes an audio sample to the WebRTC track.
    pub async fn write_audio_sample(&mut self, sample: webrtc::media::Sample) -> Result<()> {
        use webrtc::rtp::header::Header as RtpHeader;
        use webrtc::rtp::packet::Packet as RtpPacket;

        let track = self
            .audio_track
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Audio track not initialized".into()))?;

        const TIMESTAMP_INCREMENT: u32 = 960; // 20ms @ 48kHz

        if self.audio_sample_count == 0 {
            tracing::info!(
                "[WhipClient] First audio sample: {} bytes",
                sample.data.len()
            );
        }

        let rtp_packet = RtpPacket {
            header: RtpHeader {
                version: 2,
                padding: false,
                extension: false,
                marker: false,
                payload_type: 111,
                sequence_number: self.audio_seq_num as u16,
                timestamp: self.audio_timestamp,
                ssrc: 0,
                ..Default::default()
            },
            payload: sample.data,
        };

        self.audio_seq_num = self.audio_seq_num.wrapping_add(1);
        self.audio_timestamp = self.audio_timestamp.wrapping_add(TIMESTAMP_INCREMENT);
        self.audio_sample_count += 1;

        track
            .write_rtp(&rtp_packet)
            .await
            .map_err(|e| StreamError::Runtime(format!("Failed to write audio RTP: {}", e)))?;

        Ok(())
    }

    /// Gets RTCP statistics.
    pub async fn get_stats(&self) -> Option<webrtc::stats::StatsReport> {
        match &self.peer_connection {
            Some(pc) => Some(pc.get_stats().await),
            None => None,
        }
    }

    /// Terminates the WHIP session.
    pub async fn terminate(&mut self) -> Result<()> {
        tracing::info!("[WhipClient] Terminating session...");

        // Close peer connection first
        if let Some(pc) = self.peer_connection.take() {
            if let Err(e) = pc.close().await {
                tracing::warn!("[WhipClient] Error closing peer connection: {}", e);
            }
        }

        // Clear tracks
        self.video_track = None;
        self.audio_track = None;
        self.ice_candidate_rx = None;

        // Send DELETE to WHIP server
        if let Some(session_url) = self.session_url.take() {
            self.send_delete(&session_url).await?;
        }

        tracing::info!("[WhipClient] Session terminated");
        Ok(())
    }

    /// Sends DELETE request to terminate WHIP session.
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

        tracing::debug!("[WhipClient] DELETE to {}", session_url);

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.config.timeout_ms),
            self.http_client.request(req),
        )
        .await
        .map_err(|_| StreamError::Runtime("WHIP DELETE timed out".into()))?
        .map_err(|e| StreamError::Runtime(format!("WHIP DELETE failed: {}", e)))?;

        if response.status().is_success() {
            tracing::info!("[WhipClient] WHIP session deleted: {}", session_url);
        } else {
            tracing::warn!(
                "[WhipClient] DELETE returned {}, session may still exist server-side",
                response.status()
            );
        }

        Ok(())
    }
}
