// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ (Media over QUIC) Session
//
// Manages a QUIC/WebTransport connection to a MoQ relay for publish/subscribe.
// Uses moq-lite for the pub/sub protocol and moq-native for QUIC transport.

use crate::core::{Result, StreamError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// MOQ SESSION CONFIGURATION
// ============================================================================

/// Configuration for connecting to a MoQ relay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoqRelayConfig {
    /// MoQ relay endpoint URL (e.g., "https://draft-14.cloudflare.mediaoverquic.com").
    pub relay_endpoint_url: String,
    /// Broadcast namespace path (e.g., "my-broadcast-name"). Case-sensitive, no trailing slash.
    pub broadcast_path: String,
    /// Disable TLS certificate verification (for development only).
    pub tls_disable_verify: bool,
    /// Connection timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for MoqRelayConfig {
    fn default() -> Self {
        Self {
            relay_endpoint_url: "https://draft-14.cloudflare.mediaoverquic.com".to_string(),
            broadcast_path: String::new(),
            tls_disable_verify: false,
            timeout_ms: 10000,
        }
    }
}

impl MoqRelayConfig {
    fn full_url(&self) -> Result<url::Url> {
        let raw = format!(
            "{}/{}",
            self.relay_endpoint_url.trim_end_matches('/'),
            self.broadcast_path.trim_start_matches('/')
        );
        url::Url::parse(&raw)
            .map_err(|e| StreamError::Configuration(format!("Invalid MoQ relay URL '{raw}': {e}")))
    }
}

// ============================================================================
// MOQ PUBLISH SESSION
// ============================================================================

/// Publishes data to a MoQ relay. Wraps moq-lite Origin + Broadcast producers.
pub struct MoqPublishSession {
    _config: MoqRelayConfig,
    broadcast_producer: moq_lite::BroadcastProducer,
    track_producers: HashMap<String, MoqTrackPublishState>,
    /// Keeps the session alive — dropped when this struct is dropped.
    _session: moq_lite::Session,
}

/// Per-track publish state including the current group producer.
struct MoqTrackPublishState {
    track_producer: moq_lite::TrackProducer,
    current_group_producer: Option<moq_lite::GroupProducer>,
}

impl MoqPublishSession {
    /// Connect to a MoQ relay and prepare to publish a broadcast.
    pub async fn connect(config: MoqRelayConfig) -> Result<Self> {
        let url = config.full_url()?;

        let mut client_config = moq_native::ClientConfig::default();
        if config.tls_disable_verify {
            client_config.tls.disable_verify = Some(true);
        }

        let client = client_config
            .init()
            .map_err(|e| StreamError::Runtime(format!("MoQ client init failed: {e}")))?;

        let origin = moq_lite::Origin::produce();
        let broadcast_producer = origin
            .create_broadcast(&config.broadcast_path)
            .ok_or_else(|| {
                StreamError::Configuration(format!(
                    "Failed to create broadcast '{}'",
                    config.broadcast_path
                ))
            })?;

        let session = client
            .with_publish(origin.consume())
            .connect(url)
            .await
            .map_err(|e| StreamError::Runtime(format!("MoQ relay connection failed: {e}")))?;

        tracing::info!(
            version = %session.version(),
            broadcast = %config.broadcast_path,
            "MoQ publish session connected"
        );

        Ok(Self {
            _config: config,
            broadcast_producer,
            track_producers: HashMap::new(),
            _session: session,
        })
    }

    /// Publish a frame (opaque bytes) to a track. Automatically manages groups.
    ///
    /// - `track_name`: MoQ track name (typically the schema_name from FramePayload).
    /// - `payload`: Raw bytes to publish (MessagePack-serialized FramePayload).
    /// - `is_keyframe`: If true, starts a new group (MoQ Group = GOP boundary).
    pub fn publish_frame(
        &mut self,
        track_name: &str,
        payload: &[u8],
        is_keyframe: bool,
    ) -> Result<()> {
        let state = self.ensure_track_producer(track_name)?;

        // Start a new group on keyframe or if no group exists yet
        if is_keyframe || state.current_group_producer.is_none() {
            let group = state
                .track_producer
                .append_group()
                .map_err(|e| StreamError::Runtime(format!("Failed to create MoQ group: {e}")))?;
            state.current_group_producer = Some(group);
        }

        let group = state
            .current_group_producer
            .as_mut()
            .expect("group ensured above");

        group
            .write_frame(bytes::Bytes::copy_from_slice(payload))
            .map_err(|e| StreamError::Runtime(format!("Failed to write MoQ frame: {e}")))?;

        Ok(())
    }

    fn ensure_track_producer(
        &mut self,
        track_name: &str,
    ) -> Result<&mut MoqTrackPublishState> {
        if !self.track_producers.contains_key(track_name) {
            let track = moq_lite::Track::new(track_name);
            let track_producer = self
                .broadcast_producer
                .create_track(track)
                .map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to create MoQ track '{track_name}': {e}"
                    ))
                })?;

            self.track_producers.insert(
                track_name.to_string(),
                MoqTrackPublishState {
                    track_producer,
                    current_group_producer: None,
                },
            );
        }

        Ok(self.track_producers.get_mut(track_name).unwrap())
    }
}

// ============================================================================
// MOQ SUBSCRIBE SESSION
// ============================================================================

/// Subscribes to data from a MoQ relay. Wraps moq-lite Origin consumers.
pub struct MoqSubscribeSession {
    _config: MoqRelayConfig,
    origin_producer: moq_lite::OriginProducer,
    /// Keeps the session alive — dropped when this struct is dropped.
    _session: moq_lite::Session,
}

impl MoqSubscribeSession {
    /// Connect to a MoQ relay and prepare to subscribe.
    pub async fn connect(config: MoqRelayConfig) -> Result<Self> {
        let url = config.full_url()?;

        let mut client_config = moq_native::ClientConfig::default();
        if config.tls_disable_verify {
            client_config.tls.disable_verify = Some(true);
        }

        let client = client_config
            .init()
            .map_err(|e| StreamError::Runtime(format!("MoQ client init failed: {e}")))?;

        let origin = moq_lite::Origin::produce();

        let session = client
            .with_consume(origin.clone())
            .connect(url)
            .await
            .map_err(|e| {
                StreamError::Runtime(format!("MoQ subscribe connection failed: {e}"))
            })?;

        tracing::info!(
            version = %session.version(),
            broadcast = %config.broadcast_path,
            "MoQ subscribe session connected"
        );

        Ok(Self {
            _config: config,
            origin_producer: origin,
            _session: session,
        })
    }

    /// Subscribe to a specific track within a broadcast.
    ///
    /// Returns a consumer that can read groups and frames from the track.
    pub fn subscribe_track(
        &self,
        broadcast_path: &str,
        track_name: &str,
    ) -> Result<moq_lite::TrackConsumer> {
        let broadcast_consumer = self
            .origin_producer
            .consume_broadcast(broadcast_path)
            .ok_or_else(|| {
                StreamError::Runtime(format!(
                    "Broadcast '{broadcast_path}' not found on relay"
                ))
            })?;

        let track = moq_lite::Track::new(track_name);
        broadcast_consumer
            .subscribe_track(&track)
            .map_err(|e| {
                StreamError::Runtime(format!(
                    "Failed to subscribe to track '{track_name}': {e}"
                ))
            })
    }

    /// Get an origin consumer that receives broadcast announcements.
    pub fn consume_announcements(&self) -> moq_lite::OriginConsumer {
        self.origin_producer.consume()
    }
}
