// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Subscribe Track
//
// Subscribes to a single named MoQ track on a relay and forwards
// received bytes to the graph output. Type-agnostic: uses write_raw()
// to pass through raw bytes without deserialization.

use crate::core::media_clock::MediaClock;
use crate::core::streaming::{MoqRelayConfig, MoqSubscribeSession, MoqTrackReader};
use crate::core::{Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use std::future::Future;
use std::sync::Arc;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_subscribe_track")]
pub struct MoqSubscribeTrackProcessor {
    /// Runtime context for tokio handle.
    runtime_context: Option<RuntimeContext>,

    /// MoQ subscribe session (connected to relay).
    moq_subscribe_session: Option<MoqSubscribeSession>,

    /// Shutdown signaling for the async receive loop.
    shutdown_signal_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for MoqSubscribeTrackProcessor::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        self.runtime_context = Some(ctx);

        async move {
            let relay_config = MoqRelayConfig {
                relay_endpoint_url: self.config.relay_endpoint_url.clone(),
                broadcast_path: self.config.broadcast_path.clone(),
                tls_disable_verify: self.config.tls_disable_verify.unwrap_or(false),
                timeout_ms: 10000,
            };

            let session = MoqSubscribeSession::connect(relay_config).await?;

            tracing::info!(
                broadcast = %self.config.broadcast_path,
                track = %self.config.track_name,
                "[MoqSubscribeTrack] Connected to relay"
            );

            self.moq_subscribe_session = Some(session);
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[MoqSubscribeTrack] Shutting down");

        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }

        self.moq_subscribe_session.take();
        self.runtime_context.take();

        tracing::info!("[MoqSubscribeTrack] Shutdown complete");
        Ok(())
    }

    fn on_pause(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn on_resume(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        let ctx = self
            .runtime_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("RuntimeContext not available".into()))?
            .clone();

        let session = self
            .moq_subscribe_session
            .as_ref()
            .ok_or_else(|| {
                StreamError::Runtime("MoqSubscribeSession not initialized".into())
            })?;

        let track_reader = session.subscribe_track(&self.config.track_name)?;

        tracing::info!(
            track = %self.config.track_name,
            "[MoqSubscribeTrack] Subscribed to track"
        );

        let outputs = self.outputs.clone();
        let track_name = self.config.track_name.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_signal_sender = Some(shutdown_tx);

        ctx.tokio_handle().spawn(async move {
            run_moq_subscribe_track_receive_loop(track_name, track_reader, outputs, shutdown_rx)
                .await;
        });

        tracing::info!("[MoqSubscribeTrack] Started async receive loop");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }
        tracing::info!("[MoqSubscribeTrack] Stopped");
        Ok(())
    }
}

// ============================================================================
// ASYNC RECEIVE LOOP
// ============================================================================

/// Async loop that receives MoQ subgroups/frames and outputs raw bytes to the graph.
async fn run_moq_subscribe_track_receive_loop(
    track_name: String,
    mut track_reader: MoqTrackReader,
    outputs: Arc<OutputWriter>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let mut frame_count: u64 = 0;

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("[MoqSubscribeTrack] Shutdown signal received");
                break;
            }
            subgroup_result = track_reader.next_subgroup() => {
                match subgroup_result {
                    Ok(Some(mut subgroup_reader)) => {
                        loop {
                            match subgroup_reader.read_frame().await {
                                Ok(Some(frame_bytes)) => {
                                    let timestamp_ns = MediaClock::now().as_nanos() as i64;
                                    if let Err(e) = outputs.write_raw("data_out", &frame_bytes, timestamp_ns) {
                                        tracing::warn!(
                                            track = %track_name,
                                            %e,
                                            "[MoqSubscribeTrack] Failed to write received frame"
                                        );
                                    }
                                    frame_count += 1;
                                    if frame_count == 1 {
                                        tracing::info!(
                                            track = %track_name,
                                            "[MoqSubscribeTrack] First frame received"
                                        );
                                    } else if frame_count % 100 == 0 {
                                        tracing::info!(
                                            frame_count,
                                            "[MoqSubscribeTrack] Receive progress"
                                        );
                                    }
                                }
                                Ok(None) => break, // Subgroup finished
                                Err(e) => {
                                    tracing::warn!(
                                        track = %track_name,
                                        %e,
                                        "[MoqSubscribeTrack] Error reading subgroup frame"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::info!(
                            track = %track_name,
                            "[MoqSubscribeTrack] Track ended (no more subgroups)"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            track = %track_name,
                            %e,
                            "[MoqSubscribeTrack] Error reading next subgroup"
                        );
                        break;
                    }
                }
            }
        }
    }
}
