// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Subscribe Processor
//
// Receives data from a MoQ relay by subscribing to tracks within a broadcast.
// Outputs received frame bytes (raw MessagePack) through the processor graph,
// the same format used by iceoryx2 payloads.

use crate::core::media_clock::MediaClock;
use crate::core::streaming::{MoqRelayConfig, MoqSubscribeSession};
use crate::core::{Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use std::future::Future;
use std::sync::Arc;

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_subscribe")]
pub struct MoqSubscribeProcessor {
    /// Runtime context for tokio handle.
    runtime_context: Option<RuntimeContext>,

    /// MoQ subscribe session (connected to relay).
    moq_subscribe_session: Option<MoqSubscribeSession>,

    /// Shutdown signaling for the async receive loop.
    shutdown_signal_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for MoqSubscribeProcessor::Processor {
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
                tracks = ?self.config.track_names,
                "[MoqSubscribeProcessor] Connected to relay"
            );

            self.moq_subscribe_session = Some(session);
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[MoqSubscribeProcessor] Shutting down");

        // Signal shutdown to the async receive loop
        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }

        // Drop session to close QUIC connection
        self.moq_subscribe_session.take();

        tracing::info!("[MoqSubscribeProcessor] Shutdown complete");
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

        // Subscribe to each configured track, waiting for broadcast announcement
        let timeout = std::time::Duration::from_secs(30);
        let mut track_consumers = Vec::new();
        for track_name in &self.config.track_names {
            let consumer = ctx.tokio_handle().block_on(session.subscribe_track(
                &self.config.broadcast_path,
                track_name,
                timeout,
            ))?;
            tracing::info!(
                track = %track_name,
                "[MoqSubscribeProcessor] Subscribed to track"
            );
            track_consumers.push((track_name.clone(), consumer));
        }

        let outputs = self.outputs.clone();

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_signal_sender = Some(shutdown_tx);

        // Spawn the async receive loop
        ctx.tokio_handle().spawn(async move {
            run_moq_subscribe_receive_loop(track_consumers, outputs, shutdown_rx).await;
        });

        tracing::info!("[MoqSubscribeProcessor] Started async receive loop");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }
        tracing::info!("[MoqSubscribeProcessor] Stopped");
        Ok(())
    }
}

// ============================================================================
// ASYNC RECEIVE LOOP
// ============================================================================

/// Async loop that receives MoQ groups/frames and outputs raw bytes to the graph.
async fn run_moq_subscribe_receive_loop(
    track_consumers: Vec<(String, moq_lite::TrackConsumer)>,
    outputs: Arc<OutputWriter>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    // Spawn a receive task per track, funneling frames into a shared channel
    let (moq_frame_sender, mut moq_frame_receiver) =
        tokio::sync::mpsc::channel::<MoqReceivedFrame>(256);

    for (track_name, track_consumer) in track_consumers {
        let sender = moq_frame_sender.clone();
        tokio::spawn(run_moq_track_receive_loop(
            track_name,
            track_consumer,
            sender,
        ));
    }
    // Drop the original sender so the channel closes when all track tasks finish
    drop(moq_frame_sender);

    let mut frame_count: u64 = 0;

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("[MoqSubscribeProcessor] Shutdown signal received");
                break;
            }
            received = moq_frame_receiver.recv() => {
                match received {
                    Some(moq_frame) => {
                        let timestamp_ns = MediaClock::now().as_nanos() as i64;
                        if let Err(e) = outputs.write_raw("data_out", &moq_frame.payload, timestamp_ns) {
                            tracing::warn!(
                                track = %moq_frame.track_name,
                                %e,
                                "[MoqSubscribeProcessor] Failed to write received frame"
                            );
                        }
                        frame_count += 1;
                        if frame_count == 1 {
                            tracing::info!(
                                track = %moq_frame.track_name,
                                "[MoqSubscribeProcessor] First frame received"
                            );
                        } else if frame_count.is_multiple_of(100) {
                            tracing::info!(
                                frame_count,
                                "[MoqSubscribeProcessor] Receive progress"
                            );
                        }
                    }
                    None => {
                        tracing::info!("[MoqSubscribeProcessor] All track channels closed");
                        break;
                    }
                }
            }
        }
    }
}

/// A frame received from a MoQ track.
struct MoqReceivedFrame {
    track_name: String,
    payload: Vec<u8>,
}

/// Per-track receive loop: reads groups then frames from a single TrackConsumer.
async fn run_moq_track_receive_loop(
    track_name: String,
    mut track_consumer: moq_lite::TrackConsumer,
    sender: tokio::sync::mpsc::Sender<MoqReceivedFrame>,
) {
    loop {
        // Wait for the next group in this track
        let group_consumer = match track_consumer.next_group().await {
            Ok(Some(group)) => group,
            Ok(None) => {
                tracing::info!(
                    track = %track_name,
                    "[MoqSubscribeProcessor] Track ended (no more groups)"
                );
                break;
            }
            Err(e) => {
                tracing::warn!(
                    track = %track_name,
                    %e,
                    "[MoqSubscribeProcessor] Error reading next group"
                );
                break;
            }
        };

        // Read all frames within this group
        if let Err(e) =
            read_moq_group_frames(&track_name, group_consumer, &sender).await
        {
            tracing::warn!(
                track = %track_name,
                %e,
                "[MoqSubscribeProcessor] Error reading group frames"
            );
        }
    }
}

/// Read all frames from a single MoQ group and send them through the channel.
async fn read_moq_group_frames(
    track_name: &str,
    mut group_consumer: moq_lite::GroupConsumer,
    sender: &tokio::sync::mpsc::Sender<MoqReceivedFrame>,
) -> std::result::Result<(), moq_lite::Error> {
    loop {
        match group_consumer.read_frame().await? {
            Some(frame_bytes) => {
                let received_frame = MoqReceivedFrame {
                    track_name: track_name.to_string(),
                    payload: frame_bytes.to_vec(),
                };
                if sender.send(received_frame).await.is_err() {
                    // Receiver dropped (processor shutting down)
                    return Ok(());
                }
            }
            None => {
                // Group finished (no more frames)
                return Ok(());
            }
        }
    }
}
