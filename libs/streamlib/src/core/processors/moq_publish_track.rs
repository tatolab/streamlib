// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Publish Track
//
// Forwards raw bytes from a single graph input to a named MoQ track.
// Type-agnostic: uses read_raw() to receive any serialized data type
// and publishes the bytes as-is to the MoQ relay.

use crate::core::streaming::{MoqPublishSession, MoqRelayConfig};
use crate::core::{Result, RuntimeContext, StreamError};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_publish_track")]
pub struct MoqPublishTrackProcessor {
    /// Runtime context for tokio handle.
    runtime_context: Option<RuntimeContext>,

    /// MoQ publish session (connected to relay).
    moq_publish_session: Option<MoqPublishSession>,

    /// Frames published counter.
    frames_published: u64,
}

impl crate::core::ReactiveProcessor for MoqPublishTrackProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.runtime_context = Some(ctx.clone());

        let relay_config = MoqRelayConfig {
            relay_endpoint_url: self.config.relay_endpoint_url.clone(),
            broadcast_path: self.config.broadcast_path.clone(),
            tls_disable_verify: self.config.tls_disable_verify.unwrap_or(false),
            timeout_ms: 10000,
        };

        let session = MoqPublishSession::connect(relay_config).await?;

        tracing::info!(
            broadcast = %self.config.broadcast_path,
            track = %self.config.track_name,
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
        self.runtime_context.take();
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

        session.publish_frame(&self.config.track_name, &bytes, false)?;

        self.frames_published += 1;
        if self.frames_published == 1 {
            tracing::info!(
                track = %self.config.track_name,
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
