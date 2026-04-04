// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Subscribe Track
//
// Subscribes to a single named MoQ track on the shared relay session
// and forwards received bytes to the graph output. Type-agnostic:
// uses write_raw() to pass through raw bytes without deserialization.

use crate::core::media_clock::MediaClock;
use crate::core::streaming::MoqTrackReader;
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

    /// Track reader (from shared subscribe session).
    track_reader: Option<MoqTrackReader>,

    /// Shutdown signaling for the async receive loop.
    shutdown_signal_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for MoqSubscribeTrackProcessor::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        self.runtime_context = Some(ctx.clone());

        async move {
            // Get the shared subscribe session from the runtime
            let session = ctx.moq_sessions().get_subscribe_session().await?;

            // Subscribe to the specific track
            let track_reader = session.subscribe_track(&self.config.track_name)?;

            tracing::info!(
                broadcast = %ctx.moq_sessions().broadcast_path(),
                track = %self.config.track_name,
                "[MoqSubscribeTrack] Using shared session"
            );

            self.track_reader = Some(track_reader);
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[MoqSubscribeTrack] Shutting down");

        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }

        self.track_reader.take();
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

        let track_reader = self
            .track_reader
            .take()
            .ok_or_else(|| StreamError::Runtime("Track reader not initialized".into()))?;

        let outputs = self.outputs.clone();
        let track_name = self.config.track_name.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_signal_sender = Some(shutdown_tx);

        ctx.tokio_handle().spawn(async move {
            run_moq_subscribe_track_receive_loop(track_name, track_reader, outputs, shutdown_rx)
                .await;
        });

        tracing::info!(
            track = %self.config.track_name,
            "[MoqSubscribeTrack] Started async receive loop"
        );
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
    let has_output_port = outputs.has_port("data_out");

    if !has_output_port {
        tracing::info!(
            track = %track_name,
            "[MoqSubscribeTrack] No downstream connection — will log received frames only"
        );
    }

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
                                    frame_count += 1;

                                    if has_output_port {
                                        let timestamp_ns = MediaClock::now().as_nanos() as i64;
                                        if let Err(e) = outputs.write_raw("data_out", &frame_bytes, timestamp_ns) {
                                            tracing::warn!(
                                                track = %track_name,
                                                %e,
                                                "[MoqSubscribeTrack] Failed to write received frame"
                                            );
                                        }
                                    }

                                    if frame_count == 1 {
                                        tracing::info!(
                                            track = %track_name,
                                            bytes = frame_bytes.len(),
                                            "[MoqSubscribeTrack] First frame received"
                                        );
                                    } else if frame_count % 100 == 0 {
                                        tracing::info!(
                                            frame_count,
                                            "[MoqSubscribeTrack] Receive progress"
                                        );
                                    }
                                }
                                Ok(None) => break,
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
