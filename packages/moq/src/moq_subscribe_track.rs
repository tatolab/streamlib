// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Subscribe Track — subscribes to a named MoQ track and forwards bytes.
//!
//! Type-agnostic: uses `write_raw()` to pass through bytes without
//! deserialization. Reconnects on connection loss with exponential backoff.

use streamlib_moq::{sessions_for_runtime, MoqTrackReader};
use std::sync::Arc;
use std::time::Duration;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::media_clock::MediaClock;

const MAX_RETRY_ATTEMPTS: u32 = 60;
const INITIAL_RETRY_DELAY_MS: u64 = 500;
const MAX_RETRY_DELAY_MS: u64 = 10_000;

#[streamlib::sdk::processor("MoqSubscribeTrack")]
pub struct MoqSubscribeTrackProcessor {
    runtime_id: Option<String>,
    /// Plugin-owned tokio runtime. Constructed in `setup()`; the host's
    /// runtime is not reachable across the plugin ABI per #885.
    /// MoQ uses QUIC transport whose futures require this runtime's TLS.
    tokio_runtime: Option<tokio::runtime::Runtime>,
    tokio_handle: Option<tokio::runtime::Handle>,
    shutdown_signal_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl streamlib::sdk::processors::ManualProcessor for MoqSubscribeTrackProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| {
                Error::Runtime(format!(
                    "MoqSubscribeTrack: failed to build tokio runtime: {e}"
                ))
            })?;
        self.tokio_handle = Some(runtime.handle().clone());
        self.tokio_runtime = Some(runtime);
        self.runtime_id = Some(ctx.runtime_id().to_string());

        let sessions = sessions_for_runtime(self.runtime_id.as_ref().unwrap());
        tracing::info!(
            broadcast = %sessions.broadcast_path(),
            track = %self.config.track_name,
            "[MoqSubscribeTrack] Configured (will connect on start)"
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("[MoqSubscribeTrack] Shutting down");

        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }

        self.runtime_id.take();
        self.tokio_handle.take();
        self.tokio_runtime.take();
        tracing::info!("[MoqSubscribeTrack] Shutdown complete");
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let runtime_id = self
            .runtime_id
            .clone()
            .ok_or_else(|| Error::Runtime("runtime_id not captured in setup()".into()))?;
        let handle = self
            .tokio_handle
            .clone()
            .ok_or_else(|| Error::Runtime("tokio handle not captured in setup()".into()))?;

        let outputs = self.outputs.clone();
        let track_name = self.config.track_name.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_signal_sender = Some(shutdown_tx);

        handle.clone().spawn(async move {
            run_moq_subscribe_track_receive_loop_with_retry(
                track_name, runtime_id, outputs, shutdown_rx,
            )
            .await;
        });

        tracing::info!(
            track = %self.config.track_name,
            "[MoqSubscribeTrack] Started async receive loop with retry"
        );
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }
        tracing::info!("[MoqSubscribeTrack] Stopped");
        Ok(())
    }
}

/// Outer loop — opens / re-opens the subscribe session on failure.
async fn run_moq_subscribe_track_receive_loop_with_retry(
    track_name: String,
    runtime_id: String,
    outputs: OutputWriter,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let has_output_port = outputs.has_port("data_out");
    if !has_output_port {
        tracing::info!(
            track = %track_name,
            "[MoqSubscribeTrack] No downstream connection — will log received frames only"
        );
    }

    let sessions = sessions_for_runtime(&runtime_id);
    let mut total_frames: u64 = 0;
    let mut retry_count: u32 = 0;

    loop {
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[MoqSubscribeTrack] Shutdown during retry");
            break;
        }

        let session = match sessions.get_subscribe_session().await {
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

/// Inner loop — reads frames until the track ends, errors, or shutdown.
async fn run_receive_loop(
    track_name: &str,
    mut track_reader: MoqTrackReader,
    outputs: &OutputWriter,
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
                                    // Skip to next subgroup; don't kill the connection.
                                    tracing::debug!(
                                        track = %track_name,
                                        "[MoqSubscribeTrack] Subgroup frame read error, moving to next: {e}"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        return ReceiveLoopResult::TrackEnded;
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("cancelled") {
                            tracing::debug!(
                                track = %track_name,
                                "[MoqSubscribeTrack] Cancelled subgroup, skipping: {e}"
                            );
                            continue;
                        }
                        return ReceiveLoopResult::Error(err_str);
                    }
                }
            }
        }
    }
}

fn retry_delay(attempt: u32) -> Duration {
    let delay_ms = INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt.min(10));
    Duration::from_millis(delay_ms.min(MAX_RETRY_DELAY_MS))
}
