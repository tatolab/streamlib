// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ Decode Subscribe Processor
//
// Subscribes to "video" and "audio" MoQ tracks from a relay, decodes
// H.264 video (via VideoDecoder) and Opus audio (via OpusDecoder),
// and outputs Videoframe + Audioframe to the processor graph.

use crate::_generated_::{Audioframe, Videoframe};
use crate::core::codec::VideoDecoder;
use crate::core::media_clock::MediaClock;
use crate::core::streaming::{MoqRelayConfig, MoqSubscribeSession, MoqTrackReader};
use crate::core::{GpuContext, Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use bytes::Bytes;
use std::future::Future;
use std::sync::Arc;

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
use crate::core::streaming::OpusDecoder;

const VIDEO_TRACK_NAME: &str = "video";
const AUDIO_TRACK_NAME: &str = "audio";

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.moq_decode_subscribe")]
pub struct MoqDecodeSubscribeProcessor {
    runtime_context: Option<RuntimeContext>,
    moq_subscribe_session: Option<MoqSubscribeSession>,
    shutdown_signal_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for MoqDecodeSubscribeProcessor::Processor {
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
                "[MoqDecodeSubscribeProcessor] Connected to relay"
            );

            self.moq_subscribe_session = Some(session);
            Ok(())
        }
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!("[MoqDecodeSubscribeProcessor] Shutting down");
        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }
        self.moq_subscribe_session.take();
        tracing::info!("[MoqDecodeSubscribeProcessor] Shutdown complete");
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

        // Subscribe to video and audio tracks with retry
        // The publisher may not have created tracks yet (lazy init on first frame),
        // so we retry subscription until the tracks appear on the relay.
        let max_retries = 10;
        let retry_delay = std::time::Duration::from_secs(2);

        let mut video_reader = None;
        let mut audio_reader = None;

        for attempt in 1..=max_retries {
            if video_reader.is_none() {
                match session.subscribe_track(VIDEO_TRACK_NAME) {
                    Ok(r) => {
                        tracing::info!("[MoqDecodeSubscribeProcessor] Subscribed to 'video' track (attempt {attempt})");
                        video_reader = Some(r);
                    }
                    Err(e) => {
                        tracing::warn!("[MoqDecodeSubscribeProcessor] Video subscribe attempt {attempt} failed: {e}");
                    }
                }
            }
            if audio_reader.is_none() {
                match session.subscribe_track(AUDIO_TRACK_NAME) {
                    Ok(r) => {
                        tracing::info!("[MoqDecodeSubscribeProcessor] Subscribed to 'audio' track (attempt {attempt})");
                        audio_reader = Some(r);
                    }
                    Err(e) => {
                        tracing::warn!("[MoqDecodeSubscribeProcessor] Audio subscribe attempt {attempt} failed: {e}");
                    }
                }
            }

            if video_reader.is_some() && audio_reader.is_some() {
                break;
            }

            if attempt < max_retries {
                tracing::info!("[MoqDecodeSubscribeProcessor] Waiting {retry_delay:?} before retry...");
                std::thread::sleep(retry_delay);
            }
        }

        let video_reader = video_reader.ok_or_else(|| {
            StreamError::Runtime("Failed to subscribe to video track after retries".into())
        })?;
        let audio_reader = audio_reader.ok_or_else(|| {
            StreamError::Runtime("Failed to subscribe to audio track after retries".into())
        })?;

        let outputs = self.outputs.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_signal_sender = Some(shutdown_tx);

        let audio_sample_rate = self.config.audio_sample_rate.unwrap_or(48000);
        let audio_channels = self.config.audio_channels.unwrap_or(2) as usize;
        let gpu_context = ctx.gpu.clone();
        let ctx_for_task = ctx.clone();

        ctx.tokio_handle().spawn(async move {
            run_moq_decode_receive_loop(
                video_reader,
                audio_reader,
                outputs,
                ctx_for_task,
                gpu_context,
                audio_sample_rate,
                audio_channels,
                shutdown_rx,
            )
            .await;
        });

        tracing::info!("[MoqDecodeSubscribeProcessor] Started decode receive loop");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_signal_sender.take() {
            let _ = tx.send(());
        }
        tracing::info!("[MoqDecodeSubscribeProcessor] Stopped");
        Ok(())
    }
}

// ============================================================================
// ASYNC RECEIVE + DECODE LOOP
// ============================================================================

/// Frame received from a MoQ track, tagged with its track name.
struct MoqDecodableFrame {
    track_name: String,
    payload: Bytes,
}

#[allow(clippy::too_many_arguments)]
async fn run_moq_decode_receive_loop(
    video_reader: MoqTrackReader,
    audio_reader: MoqTrackReader,
    outputs: Arc<OutputWriter>,
    ctx: RuntimeContext,
    gpu_context: GpuContext,
    audio_sample_rate: u32,
    audio_channels: usize,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    // Spawn per-track receive tasks that funnel frames into a shared channel
    let (frame_sender, mut frame_receiver) =
        tokio::sync::mpsc::channel::<MoqDecodableFrame>(256);

    let video_sender = frame_sender.clone();
    tokio::spawn(run_moq_track_reader(
        VIDEO_TRACK_NAME.to_string(),
        video_reader,
        video_sender,
    ));

    let audio_sender = frame_sender.clone();
    tokio::spawn(run_moq_track_reader(
        AUDIO_TRACK_NAME.to_string(),
        audio_reader,
        audio_sender,
    ));
    drop(frame_sender);

    // Initialize decode states
    let mut video_state = VideoDecodeState::new(ctx, gpu_context);

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
    let mut audio_state = AudioDecodeState::new(audio_sample_rate, audio_channels);

    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    tracing::warn!("[MoqDecodeSubscribeProcessor] Opus decoding not supported on this platform");

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("[MoqDecodeSubscribeProcessor] Shutdown signal received");
                break;
            }
            received = frame_receiver.recv() => {
                match received {
                    Some(frame) if frame.track_name == VIDEO_TRACK_NAME => {
                        if let Some(video_frame) = video_state.process_moq_frame(&frame.payload) {
                            if let Err(e) = outputs.write("video_out", &video_frame) {
                                tracing::warn!(
                                    "[MoqDecodeSubscribeProcessor] Failed to write video frame: {}", e
                                );
                            }
                        }
                    }
                    Some(frame) if frame.track_name == AUDIO_TRACK_NAME => {
                        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
                        if let Some(audio_frame) = audio_state.process_moq_frame(&frame.payload) {
                            if let Err(e) = outputs.write("audio_out", &audio_frame) {
                                tracing::warn!(
                                    "[MoqDecodeSubscribeProcessor] Failed to write audio frame: {}", e
                                );
                            }
                        }
                    }
                    Some(_) => {} // Unknown track — ignore
                    None => {
                        tracing::info!("[MoqDecodeSubscribeProcessor] All track channels closed");
                        break;
                    }
                }
            }
        }
    }
}

/// Reads subgroups and frames from a single MoQ track, sending them to the channel.
async fn run_moq_track_reader(
    track_name: String,
    mut track_reader: MoqTrackReader,
    sender: tokio::sync::mpsc::Sender<MoqDecodableFrame>,
) {
    loop {
        let mut subgroup_reader = match track_reader.next_subgroup().await {
            Ok(Some(sg)) => sg,
            Ok(None) => {
                tracing::info!(
                    track = %track_name,
                    "[MoqDecodeSubscribeProcessor] Track ended"
                );
                break;
            }
            Err(e) => {
                tracing::warn!(
                    track = %track_name,
                    %e,
                    "[MoqDecodeSubscribeProcessor] Error reading subgroup"
                );
                break;
            }
        };

        loop {
            match subgroup_reader.read_frame().await {
                Ok(Some(frame_bytes)) => {
                    let frame = MoqDecodableFrame {
                        track_name: track_name.clone(),
                        payload: frame_bytes,
                    };
                    if sender.send(frame).await.is_err() {
                        return; // Receiver dropped
                    }
                }
                Ok(None) => break, // Subgroup finished
                Err(e) => {
                    tracing::warn!(
                        track = %track_name,
                        %e,
                        "[MoqDecodeSubscribeProcessor] Error reading frame"
                    );
                    break;
                }
            }
        }
    }
}

// ============================================================================
// VIDEO DECODE STATE
// ============================================================================

/// Decodes H.264 NAL units received from MoQ into Videoframe.
///
/// Expects each MoQ frame to contain one or more H.264 NAL units in
/// Annex B format (0x00 0x00 0x00 0x01 + NAL data). Collects SPS/PPS
/// to initialize the decoder, then decodes IDR and non-IDR frames.
///
/// Mirrors the decode path used by [`WebRtcWhepProcessor`] so that the
/// resulting [`Videoframe`] is identical to what DisplayProcessor expects.
struct VideoDecodeState {
    ctx: RuntimeContext,
    gpu_context: GpuContext,
    decoder: Option<VideoDecoder>,
    sps_nal: Option<Bytes>,
    pps_nal: Option<Bytes>,
    /// Skip P-frames until we've decoded at least one IDR (keyframe).
    /// Necessary when joining a live stream mid-GOP.
    received_first_idr: bool,
    frame_count: u64,
}

impl VideoDecodeState {
    fn new(ctx: RuntimeContext, gpu_context: GpuContext) -> Self {
        Self {
            ctx,
            gpu_context,
            decoder: None,
            sps_nal: None,
            pps_nal: None,
            received_first_idr: false,
            frame_count: 0,
        }
    }

    /// Process a raw MoQ video frame payload.
    ///
    /// The payload is expected to be Annex B H.264 data (start codes + NAL units).
    fn process_moq_frame(&mut self, payload: &[u8]) -> Option<Videoframe> {
        if payload.len() < 5 {
            return None;
        }

        // Split payload into individual NAL units by finding start codes
        let nals = split_annex_b_nal_units(payload);
        if nals.is_empty() {
            // Payload may be a single raw NAL unit without start codes
            return self.process_nal_unit(payload);
        }

        let mut result_frame = None;
        for nal in nals {
            if let Some(frame) = self.process_nal_unit(nal) {
                result_frame = Some(frame);
            }
        }
        result_frame
    }

    /// Process a single NAL unit (without start code prefix).
    ///
    /// Matches the WHEP processor's decode path exactly: SPS/PPS are stored
    /// and used to initialize the decoder via `update_format`, then IDR and
    /// non-IDR NALs are wrapped in Annex B format and passed to `decode()`.
    /// The decoder's extradata (set by `update_format`) provides SPS/PPS
    /// context -- no need to prepend them before IDR frames.
    fn process_nal_unit(&mut self, nal: &[u8]) -> Option<Videoframe> {
        if nal.is_empty() {
            return None;
        }

        let nal_type = nal[0] & 0x1F;

        // SPS (7) — Sequence Parameter Set
        if nal_type == 7 {
            tracing::info!(
                "[MoqDecodeSubscribeProcessor] Received SPS ({} bytes)",
                nal.len()
            );
            self.sps_nal = Some(Bytes::copy_from_slice(nal));

            // Try to initialize decoder if we have both SPS and PPS
            if let (Some(sps), Some(pps)) = (&self.sps_nal, &self.pps_nal) {
                self.initialize_decoder(sps.clone(), pps.clone());
            }
            return None;
        }

        // PPS (8) — Picture Parameter Set
        if nal_type == 8 {
            tracing::info!(
                "[MoqDecodeSubscribeProcessor] Received PPS ({} bytes)",
                nal.len()
            );
            self.pps_nal = Some(Bytes::copy_from_slice(nal));

            // Try to initialize decoder if we have both SPS and PPS
            if let (Some(sps), Some(pps)) = (&self.sps_nal, &self.pps_nal) {
                self.initialize_decoder(sps.clone(), pps.clone());
            }
            return None;
        }

        // IDR (5) — mark that we can start decoding
        if nal_type == 5 {
            self.received_first_idr = true;
        }

        // IDR (5) or Non-IDR (1) — decode frame
        // Skip P-frames until first IDR when joining mid-stream.
        if (nal_type == 1 || nal_type == 5) && self.received_first_idr {
            if let Some(decoder) = &mut self.decoder {
                let timestamp_ns = MediaClock::now().as_nanos() as i64;

                // For IDR frames, prepend SPS+PPS because FFmpeg's decoder
                // context was opened before extradata was set (avcodec_open2
                // is called in VideoDecoder::new, before update_format).
                // Sending SPS+PPS inline with each IDR ensures the parser
                // always has the parameter sets available.
                let nal_data = if nal_type == 5 {
                    if let (Some(sps), Some(pps)) = (&self.sps_nal, &self.pps_nal) {
                        let mut buf = Vec::with_capacity(
                            4 + sps.len() + 4 + pps.len() + 4 + nal.len(),
                        );
                        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        buf.extend_from_slice(sps);
                        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        buf.extend_from_slice(pps);
                        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        buf.extend_from_slice(nal);
                        Bytes::from(buf)
                    } else {
                        let mut buf = Vec::with_capacity(4 + nal.len());
                        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        buf.extend_from_slice(nal);
                        Bytes::from(buf)
                    }
                } else {
                    let mut buf = Vec::with_capacity(4 + nal.len());
                    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    buf.extend_from_slice(nal);
                    Bytes::from(buf)
                };

                // Decoder returns Videoframe directly (handles buffer pooling internally)
                match decoder.decode(&nal_data, timestamp_ns, &self.gpu_context) {
                    Ok(Some(ipc_frame)) => {
                        self.frame_count += 1;

                        if self.frame_count % 30 == 0 {
                            tracing::info!(
                                "[MoqDecodeSubscribeProcessor] Decoded video frame #{}",
                                self.frame_count
                            );
                        }
                        return Some(ipc_frame);
                    }
                    Ok(None) => {
                        // Decoder needs more data
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[MoqDecodeSubscribeProcessor] Video decode error: {}",
                            e
                        );
                    }
                }
            } else {
                tracing::debug!(
                    "[MoqDecodeSubscribeProcessor] NAL type {} received but decoder not ready",
                    nal_type
                );
            }
        }

        None
    }

    fn initialize_decoder(&mut self, sps: Bytes, pps: Bytes) {
        if self.decoder.is_some() {
            return; // Already initialized
        }

        tracing::info!(
            "[MoqDecodeSubscribeProcessor] Initializing video decoder with SPS ({} bytes) and PPS ({} bytes)",
            sps.len(),
            pps.len()
        );

        match VideoDecoder::new(Default::default(), &self.ctx) {
            Ok(mut decoder) => {
                if let Err(e) = decoder.update_format(&sps, &pps) {
                    tracing::error!(
                        "[MoqDecodeSubscribeProcessor] Failed to update decoder format: {}",
                        e
                    );
                    return;
                }
                self.decoder = Some(decoder);
                tracing::info!("[MoqDecodeSubscribeProcessor] Video decoder initialized");
            }
            Err(e) => {
                tracing::error!(
                    "[MoqDecodeSubscribeProcessor] Failed to create video decoder: {}",
                    e
                );
            }
        }
    }
}

// ============================================================================
// AUDIO DECODE STATE
// ============================================================================

/// Decodes Opus packets received from MoQ into Audioframe.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
struct AudioDecodeState {
    decoder: Option<OpusDecoder>,
    frame_count: u64,
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
impl AudioDecodeState {
    fn new(sample_rate: u32, channels: usize) -> Self {
        let decoder = match OpusDecoder::new(sample_rate, channels) {
            Ok(d) => {
                tracing::info!(
                    "[MoqDecodeSubscribeProcessor] Opus decoder created ({}Hz, {} ch)",
                    sample_rate,
                    channels
                );
                Some(d)
            }
            Err(e) => {
                tracing::error!(
                    "[MoqDecodeSubscribeProcessor] Failed to create Opus decoder: {}",
                    e
                );
                None
            }
        };

        Self {
            decoder,
            frame_count: 0,
        }
    }

    fn process_moq_frame(&mut self, payload: &[u8]) -> Option<Audioframe> {
        let decoder = self.decoder.as_mut()?;
        let timestamp_ns = MediaClock::now().as_nanos() as i64;

        match decoder.decode_to_audio_frame(payload, timestamp_ns) {
            Ok(audio_frame) => {
                self.frame_count += 1;
                if self.frame_count == 1 {
                    tracing::info!(
                        "[MoqDecodeSubscribeProcessor] First audio frame decoded"
                    );
                } else if self.frame_count % 50 == 0 {
                    tracing::info!(
                        "[MoqDecodeSubscribeProcessor] Decoded audio frame #{}",
                        self.frame_count
                    );
                }
                Some(audio_frame)
            }
            Err(e) => {
                tracing::warn!(
                    "[MoqDecodeSubscribeProcessor] Audio decode error: {}",
                    e
                );
                None
            }
        }
    }
}

// ============================================================================
// ANNEX B NAL UNIT SPLITTER
// ============================================================================

/// Splits an Annex B byte stream into individual NAL units (without start codes).
///
/// Recognizes both 3-byte (0x00 0x00 0x01) and 4-byte (0x00 0x00 0x00 0x01) start codes.
fn split_annex_b_nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let len = data.len();

    // Find first start code
    let mut nal_start = None;
    while i < len {
        if i + 2 < len && data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                nal_start = Some(i + 3);
                i += 3;
                break;
            } else if i + 3 < len && data[i + 2] == 0 && data[i + 3] == 1 {
                nal_start = Some(i + 4);
                i += 4;
                break;
            }
        }
        i += 1;
    }

    let Some(mut current_nal_start) = nal_start else {
        return nals;
    };

    // Find subsequent start codes to delimit NAL units
    while i < len {
        if i + 2 < len && data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                // 3-byte start code
                let nal_end = i;
                if nal_end > current_nal_start {
                    nals.push(&data[current_nal_start..nal_end]);
                }
                current_nal_start = i + 3;
                i += 3;
                continue;
            } else if i + 3 < len && data[i + 2] == 0 && data[i + 3] == 1 {
                // 4-byte start code
                let nal_end = i;
                if nal_end > current_nal_start {
                    nals.push(&data[current_nal_start..nal_end]);
                }
                current_nal_start = i + 4;
                i += 4;
                continue;
            }
        }
        i += 1;
    }

    // Last NAL unit
    if current_nal_start < len {
        nals.push(&data[current_nal_start..]);
    }

    nals
}
