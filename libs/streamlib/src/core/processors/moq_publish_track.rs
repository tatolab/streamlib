// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Publish Track
//
// Forwards raw bytes from a single graph input to a named MoQ track.
// Type-agnostic: uses read_raw() to receive any serialized data type
// and publishes the bytes as-is to the MoQ relay.

use crate::core::streaming::{MoqPublishSession, MoqRelayConfig};
use crate::core::{Result, RuntimeContext, StreamError};

/// Default MoQ relay (Cloudflare draft-14).
const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_publish_track")]
pub struct MoqPublishTrackProcessor {
    /// MoQ publish session (connected to relay).
    moq_publish_session: Option<MoqPublishSession>,

    /// Resolved track name (from config or auto-generated).
    track_name: String,

    /// Frames published counter.
    frames_published: u64,
}

impl crate::core::ReactiveProcessor for MoqPublishTrackProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        // Track name: use config value or auto-generate from processor ID
        self.track_name = self.config.track_name.clone().unwrap_or_else(|| {
            ctx.processor_id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "default".to_string())
        });

        // Build relay config: use provided URL or default relay + auto-generated broadcast path
        let relay_config = match &self.config.url {
            Some(url) => {
                let parsed_url = url::Url::parse(url)
                    .map_err(|e| StreamError::Configuration(format!("Invalid MoQ URL: {e}")))?;
                let broadcast_path = parsed_url.path().trim_start_matches('/').to_string();
                let mut relay_base = parsed_url.clone();
                relay_base.set_path("");
                MoqRelayConfig {
                    relay_endpoint_url: relay_base.to_string().trim_end_matches('/').to_string(),
                    broadcast_path,
                    tls_disable_verify: false,
                    timeout_ms: 10000,
                }
            }
            None => {
                // Auto-generate broadcast path from runtime ID
                let broadcast_path = format!("streamlib/{}", ctx.runtime_id());
                MoqRelayConfig {
                    relay_endpoint_url: DEFAULT_RELAY_URL.to_string(),
                    broadcast_path,
                    tls_disable_verify: false,
                    timeout_ms: 10000,
                }
            }
        };

        let session = MoqPublishSession::connect(relay_config.clone()).await?;

        tracing::info!(
            relay = %relay_config.relay_endpoint_url,
            broadcast = %relay_config.broadcast_path,
            track = %self.track_name,
            "[MoqPublishTrack] Connected to relay"
        );

        self.moq_publish_session = Some(session);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_published = self.frames_published,
            "[MoqPublishTrack] Shutting down"
        );
        self.moq_publish_session.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        let raw_data = match self.inputs.read_raw("data_in") {
            Ok(Some(data)) => data,
            Ok(None) => return Ok(()),
            Err(e) => {
                tracing::warn!("[MoqPublishTrack] Failed to read input: {}", e);
                return Err(e);
            }
        };

        let (bytes, _timestamp_ns) = raw_data;

        let session = self
            .moq_publish_session
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("MoQ session not connected".into()))?;

        session.publish_frame(&self.track_name, &bytes, false)?;

        self.frames_published += 1;
        if self.frames_published == 1 {
            tracing::info!(
                track = %self.track_name,
                "[MoqPublishTrack] First frame published"
            );
        } else if self.frames_published % 100 == 0 {
            tracing::info!(
                frames = self.frames_published,
                "[MoqPublishTrack] Publish progress"
            );
        }

        Ok(())
    }
}
