// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Subscribe Track
//
// Subscribes to a single named MoQ track on the shared relay session
// and forwards received bytes to the graph output. Type-agnostic:
// uses write_raw() to pass through raw bytes without deserialization.
//
// Automatically retries on connection loss with exponential backoff.

use crate::core::media_clock::MediaClock;
use crate::core::streaming::{MoqSubscribeSession, MoqTrackReader};
use crate::core::{Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Maximum retry attempts before giving up.
const MAX_RETRY_ATTEMPTS: u32 = 60;

/// Initial retry delay (doubles each attempt, capped at 10s).
const INITIAL_RETRY_DELAY_MS: u64 = 500;

/// Maximum retry delay cap.
const MAX_RETRY_DELAY_MS: u64 = 10_000;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_subscribe_track")]
pub struct MoqSubscribeTrackProcessor {
    /// Runtime context for tokio handle and shared sessions.
    runtime_context: Option<RuntimeContext>,

    /// Shutdown signaling for the async receive loop.
    shutdown_signal_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for MoqSubscribeTrackProcessor::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        self.runtime_context = Some(ctx.clone());

        async move {
            tracing::info!(
                broadcast = %ctx.moq_sessions().broadcast_path(),
                track = %self.config.track_name,
                "[MoqSubscribeTrack] Configured (will connect on start)"
            );
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[MoqSubscribeTrack] Shutting down");

        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }

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

        let outputs = self.outputs.clone();
        let track_name = self.config.track_name.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_signal_sender = Some(shutdown_tx);

        let handle = ctx.tokio_handle().clone();
        handle.spawn(async move {
            run_moq_subscribe_track_receive_loop_with_retry(
                track_name, ctx, outputs, shutdown_rx,
            )
            .await;
        });

        tracing::info!(
            track = %self.config.track_name,
            "[MoqSubscribeTrack] Started async receive loop with retry"
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
// ASYNC RECEIVE LOOP WITH RETRY
// ============================================================================

/// Outer loop that handles connection/subscription failures with retry.
async fn run_moq_subscribe_track_receive_loop_with_retry(
    track_name: String,
    ctx: RuntimeContext,
    outputs: Arc<OutputWriter>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let has_output_port = outputs.has_port("data_out");
    if !has_output_port {
        tracing::info!(
            track = %track_name,
            "[MoqSubscribeTrack] No downstream connection — will log received frames only"
        );
    }

    let mut total_frames: u64 = 0;
    let mut retry_count: u32 = 0;

    loop {
        // Check shutdown before attempting connection
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[MoqSubscribeTrack] Shutdown during retry");
            break;
        }

        // Get or create the shared subscribe session
        let session = match ctx.moq_sessions().get_subscribe_session().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    track = %track_name,
                    %e,
                    retry = retry_count,
                    "[MoqSubscribeTrack] Failed to get subscribe session"
                );
                if retry_count >= MAX_RETRY_ATTEMPTS {
                    tracing::error!(
                        track = %track_name,
                        "[MoqSubscribeTrack] Max retries exceeded, giving up"
                    );
                    break;
                }
                let delay = retry_delay(retry_count);
                retry_count += 1;
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        // Subscribe to the track
        let track_reader = match session.subscribe_track(&track_name) {
            Ok(reader) => {
                if retry_count > 0 {
                    tracing::info!(
                        track = %track_name,
                        retry = retry_count,
                        "[MoqSubscribeTrack] Reconnected after {} retries",
                        retry_count
                    );
                }
                retry_count = 0;
                reader
            }
            Err(e) => {
                tracing::warn!(
                    track = %track_name,
                    %e,
                    retry = retry_count,
                    "[MoqSubscribeTrack] Subscribe failed, will retry"
                );
                if retry_count >= MAX_RETRY_ATTEMPTS {
                    tracing::error!(
                        track = %track_name,
                        "[MoqSubscribeTrack] Max retries exceeded, giving up"
                    );
                    break;
                }
                let delay = retry_delay(retry_count);
                retry_count += 1;
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        // Run the receive loop — returns when the track ends or errors
        let result = run_receive_loop(
            &track_name,
            track_reader,
            &outputs,
            has_output_port,
            &mut total_frames,
            &mut shutdown_rx,
        )
        .await;

        match result {
            ReceiveLoopResult::Shutdown => break,
            ReceiveLoopResult::TrackEnded => {
                tracing::info!(
                    track = %track_name,
                    "[MoqSubscribeTrack] Track ended, will retry subscription"
                );
            }
            ReceiveLoopResult::Error(e) => {
                tracing::warn!(
                    track = %track_name,
                    error = %e,
                    retry = retry_count,
                    "[MoqSubscribeTrack] Connection lost, will retry"
                );
            }
        }

        if retry_count >= MAX_RETRY_ATTEMPTS {
            tracing::error!(
                track = %track_name,
                "[MoqSubscribeTrack] Max retries exceeded, giving up"
            );
            break;
        }

        let delay = retry_delay(retry_count);
        retry_count += 1;
        tracing::info!(
            track = %track_name,
            delay_ms = delay.as_millis() as u64,
            "[MoqSubscribeTrack] Retrying in {}ms...",
            delay.as_millis()
        );
        tokio::time::sleep(delay).await;
    }
}

enum ReceiveLoopResult {
    Shutdown,
    TrackEnded,
    Error(String),
}

/// Inner receive loop — reads frames until the track ends, errors, or shutdown.
async fn run_receive_loop(
    track_name: &str,
    mut track_reader: MoqTrackReader,
    outputs: &Arc<OutputWriter>,
    has_output_port: bool,
    total_frames: &mut u64,
    shutdown_rx: &mut tokio::sync::oneshot::Receiver<()>,
) -> ReceiveLoopResult {
    loop {
        tokio::select! {
            _ = &mut *shutdown_rx => {
                return ReceiveLoopResult::Shutdown;
            }
            subgroup_result = track_reader.next_subgroup() => {
                match subgroup_result {
                    Ok(Some(mut subgroup_reader)) => {
                        loop {
                            match subgroup_reader.read_frame().await {
                                Ok(Some(frame_bytes)) => {
                                    *total_frames += 1;

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

                                    if *total_frames == 1 {
                                        tracing::info!(
                                            track = %track_name,
                                            bytes = frame_bytes.len(),
                                            "[MoqSubscribeTrack] First frame received"
                                        );
                                    } else if *total_frames % 100 == 0 {
                                        tracing::info!(
                                            total_frames,
                                            "[MoqSubscribeTrack] Receive progress"
                                        );
                                    }
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    return ReceiveLoopResult::Error(e.to_string());
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        return ReceiveLoopResult::TrackEnded;
                    }
                    Err(e) => {
                        return ReceiveLoopResult::Error(e.to_string());
                    }
                }
            }
        }
    }
}

/// Calculate retry delay with exponential backoff.
fn retry_delay(attempt: u32) -> Duration {
    let delay_ms = INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt.min(10));
    Duration::from_millis(delay_ms.min(MAX_RETRY_DELAY_MS))
}
