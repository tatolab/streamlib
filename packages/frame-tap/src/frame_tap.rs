// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame tap (Linux) — a sink processor that samples incoming video frames
//! to disk as JPEG stills, off the hot path.
//!
//! Design (the seed for a general tap behavior):
//!
//! - **Sink, not inline.** The tap has a `video_in` input and no outputs.
//!   You attach it as a fan-out branch off any video output port; the
//!   engine broadcasts a port's frames to every connected consumer, so the
//!   tap samples the same frames the real consumer sees without rerouting
//!   the pipeline.
//! - **Non-blocking GPU readback.** Each sampled frame's texture is copied
//!   GPU→CPU through the plugin SDK's cdylib-safe [`TextureReadback`]
//!   PluginAbiObject using the *non-blocking* `submit` / `try_read_copy`
//!   pair. The handle is created host-side via
//!   `GpuContextLimitedAccess::escalate(|full| full.create_texture_readback(..))`
//!   once per source extent/format — never off a raw host device. The
//!   readback is single-in-flight: we submit at most one copy and drain it
//!   on a later `process()` call. If a sample is due while a prior readback
//!   is still in flight we skip it (drop-if-busy) — the pipeline never
//!   stalls on the tap.
//! - **Off-thread JPEG + bounded queue.** Completed readbacks are handed to
//!   a background writer thread over a small bounded channel. When the
//!   channel is full the sample is dropped (drop-on-full) rather than
//!   blocking `process()`. JPEG encoding and disk writes happen entirely on
//!   the writer thread.
//! - **Filesystem-safe by default.** The writer enforces `max_file_count`
//!   and `max_total_mb` caps, evicting oldest-first, and writes atomically
//!   (`*.jpg.tmp` + rename) so a reader never sees a half-written file.
//!
//! A future optimization (tracked separately) routes the readback copy on a
//! dedicated transfer queue so it doesn't contend with the render/compute
//! queue at all.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder};

use streamlib_plugin_sdk::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{
    ReadbackTicket, TextureFormat, TextureReadback, TextureSourceLayout, VulkanLayout,
};

use crate::_generated_::VideoFrame;
use crate::_generated_::tatolab__frame_tap::frame_tap_config::Strategy;

/// Writer-thread queue depth. Small and bounded: when full, samples are
/// dropped (drop-on-full) so `process()` never blocks on disk I/O.
const WRITER_QUEUE_DEPTH: usize = 4;

/// Default sample cadence for [`Strategy::EveryNFrames`].
const DEFAULT_N_FRAMES: u32 = 30;
/// Default sample gap (ms) for [`Strategy::EveryDuration`].
const DEFAULT_INTERVAL_MS: u32 = 1000;
/// Default retained count for [`Strategy::KeepLastK`].
const DEFAULT_KEEP_LAST_K: u32 = 8;
/// Default JPEG quality (1..=100).
const DEFAULT_JPEG_QUALITY: u32 = 85;
/// Default filesystem caps.
const DEFAULT_MAX_FILE_COUNT: u32 = 200;
const DEFAULT_MAX_TOTAL_MB: u32 = 512;

/// Backoff between readback-handle creation retries after a failure. Each
/// `escalate` scope-end runs a device-wide drain (`wait_device_idle`, see the
/// escalate contract in the plugin SDK's `context.rs`), so a *persistent*
/// creation failure must not re-escalate on every sampled frame (that is a
/// full-GPU stall per frame under [`Strategy::KeepLastK`]). A short backoff
/// still recovers a transient failure quickly; an extent/format change bypasses
/// it entirely.
const READBACK_CREATION_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// One in-flight GPU→CPU readback awaiting completion.
struct PendingReadback {
    ticket: ReadbackTicket,
    width: u32,
    height: u32,
    color_type: ColorType,
}

/// Backoff bookkeeping for a *failed* readback-handle creation. Persisted so a
/// persistent failure re-escalates on a cadence rather than every sampled frame
/// (each `escalate` scope-end is a device-wide drain). Cleared on a successful
/// creation; bypassed immediately on a key change.
struct ReadbackCreationBackoff {
    /// The `(width, height, format)` key whose readback-handle creation failed.
    failed_key: (u32, u32, TextureFormat),
    /// Monotonic earliest instant to retry creation for `failed_key`.
    retry_at: Instant,
}

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/frame-tap/FrameTap",
    description = "Samples video frames to disk (JPEG) on a configurable strategy, off the hot path. A sink: attach it to any video output port (fan-out) to inspect that output without rerouting the pipeline.",
    execution = reactive,
    config = crate::_generated_::FrameTapConfig,
    input("video_in", "@tatolab/core/VideoFrame"),
)]
pub struct FrameTapProcessor {
    /// LimitedAccess context for resolving surfaces and escalating for
    /// privileged readback creation in `process()`.
    gpu_context: Option<GpuContextLimitedAccess>,
    /// Readback handle, created host-side (via `escalate` +
    /// `create_texture_readback`) once the source extent/format is known.
    /// `!Clone` — the primitive owns its single-in-flight staging resources.
    readback: Option<TextureReadback>,
    /// `(width, height, format)` the current readback handle is bound to;
    /// a change rebuilds the handle.
    readback_key: Option<(u32, u32, TextureFormat)>,
    /// Backoff state for a failed readback-handle creation, throttling the
    /// (device-draining) `escalate` retry so a persistent failure does not
    /// stall the GPU every sampled frame. `None` when no failure is pending.
    readback_creation_backoff: Option<ReadbackCreationBackoff>,
    /// In-flight readback (single). `None` when idle.
    pending: Option<PendingReadback>,
    /// Background JPEG writer (bounded, drop-on-full).
    writer: Option<SampleWriter>,
    /// Total frames observed.
    frame_count: u64,
    /// Monotonic index of the next sample file written.
    sample_index: u64,
    /// Samples dropped because the readback was busy or the writer queue full.
    dropped_samples: u64,
    /// Wall-clock instant of the last submitted sample (for `EveryDuration`).
    last_sample_at: Option<Instant>,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor for FrameTapProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The readback handle is created lazily in `process()` (its extent is
        // only known once frames flow) via `gpu_context.escalate(..)`; setup
        // just stashes the LimitedAccess view. No raw host device is held.
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        // KeepLastK uses keep_last_k as the file cap; the other strategies use
        // max_file_count. Both honor max_total_mb.
        let max_files = match &self.config.strategy {
            Strategy::KeepLastK => self.config.keep_last_k.unwrap_or(DEFAULT_KEEP_LAST_K),
            _ => self.config.max_file_count.unwrap_or(DEFAULT_MAX_FILE_COUNT),
        };
        let max_total_bytes =
            (self.config.max_total_mb.unwrap_or(DEFAULT_MAX_TOTAL_MB) as u64) * 1024 * 1024;
        let output_dir = PathBuf::from(&self.config.output_dir);

        tracing::info!(
            "FrameTap: setup (strategy={:?}, output_dir={:?}, max_files={}, max_total_mb={})",
            self.config.strategy,
            output_dir,
            max_files,
            self.config.max_total_mb.unwrap_or(DEFAULT_MAX_TOTAL_MB),
        );

        self.writer = Some(SampleWriter::spawn(output_dir, max_files, max_total_bytes));
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "FrameTap: teardown — {} frames seen, {} samples written, {} dropped (busy/full)",
            self.frame_count,
            self.sample_index,
            self.dropped_samples,
        );
        // Drops the writer, which closes the channel, drains, and joins the
        // writer thread.
        self.writer = None;
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.frame_count += 1;
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;
        let gpu = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Configuration("FrameTap: GPU context not initialized".into()))?
            .clone();

        // 1. Drain a completed prior readback (non-blocking) and enqueue the
        //    write. `try_read_copy` copies the ready bytes into an owned `Vec`,
        //    so the staging buffer is free to be reused by the next submit.
        if let Some(pending) = self.pending.take() {
            self.drain_pending(pending);
        }

        // 2. Decide whether to sample this frame and submit a fresh readback.
        //    drop-if-busy: only submit when no readback is in flight.
        if self.pending.is_none() && self.should_sample() {
            self.submit_sample(&gpu, &frame)?;
        }
        Ok(())
    }
}

impl FrameTapProcessor::Processor {
    /// Try to read a completed readback and hand its bytes to the writer.
    fn drain_pending(&mut self, pending: PendingReadback) {
        // `try_read_copy` (not `try_read`) COPIES the ready staging bytes into
        // an owned `Vec`: the readback is stored inline (`!Clone`), so a
        // borrowed `try_read` slice would pin `&self` and block the
        // `self.sample_index` bookkeeping below. The owned copy is also exactly
        // what the background writer needs — it outlives the borrow window.
        let read_result = match self.readback.as_ref() {
            Some(rb) => rb.try_read_copy(pending.ticket),
            None => return,
        };
        match read_result {
            Ok(Some(bytes)) => {
                let prefix = self
                    .config
                    .filename_prefix
                    .clone()
                    .unwrap_or_else(|| "frame".to_string());
                let quality =
                    self.config.jpeg_quality.unwrap_or(DEFAULT_JPEG_QUALITY).clamp(1, 100) as u8;
                let path = PathBuf::from(&self.config.output_dir)
                    .join(format!("{}_{:06}.jpg", prefix, self.sample_index));
                let job = SampleJob {
                    bytes,
                    width: pending.width as u16,
                    height: pending.height as u16,
                    color_type: pending.color_type,
                    quality,
                    path,
                };
                let enqueued = self.writer.as_ref().map(|w| w.try_enqueue(job)).unwrap_or(false);
                if enqueued {
                    self.sample_index += 1;
                } else {
                    self.dropped_samples += 1;
                }
            }
            Ok(None) => {
                // Still in flight — keep waiting.
                self.pending = Some(pending);
            }
            Err(e) => {
                tracing::warn!("FrameTap: readback try_read_copy failed: {}", e);
            }
        }
    }

    /// Strategy gate: should this frame be sampled?
    fn should_sample(&self) -> bool {
        match &self.config.strategy {
            // `EveryNFrames` in the schema; jtd-codegen renders the Rust
            // ident as `EveryNframes` (wire value preserved via serde rename).
            Strategy::EveryNframes => {
                let n = self.config.n_frames.unwrap_or(DEFAULT_N_FRAMES).max(1) as u64;
                self.frame_count % n == 0
            }
            Strategy::EveryDuration => {
                let interval = self.config.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS) as u128;
                match self.last_sample_at {
                    None => true,
                    Some(t) => t.elapsed().as_millis() >= interval,
                }
            }
            // KeepLastK samples every frame; drop-if-busy throttles it to
            // readback throughput and the writer retains only the last K.
            Strategy::KeepLastK => true,
        }
    }

    /// Resolve the frame's surface, (re)build the readback handle if needed,
    /// and submit a non-blocking GPU→CPU copy.
    fn submit_sample(&mut self, gpu: &GpuContextLimitedAccess, frame: &VideoFrame) -> Result<()> {
        let registration = gpu.resolve_texture_registration_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;
        let texture = registration.texture().clone();
        let layout = registration.current_layout();
        let format = texture.format();

        let color_type = match color_type_for(format) {
            Some(ct) => ct,
            None => {
                tracing::warn!(
                    "FrameTap: unsupported texture format {:?} — skipping sample",
                    format
                );
                return Ok(());
            }
        };

        let key = (texture.width(), texture.height(), format);
        if self.readback_key != Some(key) {
            // Throttle a persistent creation failure: each `escalate` scope-end
            // drains the whole device, so if creation for THIS exact key failed
            // recently and its backoff has not elapsed, skip the sample without
            // re-escalating. A key change (different extent/format) or an
            // elapsed backoff falls through and retries — see
            // `readback_creation_backoff_blocks`.
            if readback_creation_backoff_blocks(
                self.readback_creation_backoff.as_ref(),
                key,
                Instant::now(),
            ) {
                return Ok(());
            }
            let width = texture.width();
            let height = texture.height();
            // Privileged host-side creation: `escalate` opens a FullAccess
            // window just long enough to build the readback on the host's own
            // device, then drains + releases it. The returned `TextureReadback`
            // is cached and its per-frame `submit` / `try_read_copy` run
            // scope-free (no second escalate). `escalate` returns
            // `Result<Result<..>>` — the outer is the escalate machinery, the
            // inner is the creation. Best-effort on both: warn, arm a backoff so
            // a persistent failure does not re-drain the GPU every frame, and
            // skip this sample rather than stalling or failing the pipeline.
            match gpu.escalate(|full| {
                full.create_texture_readback("frame-tap", width, height, format)
            }) {
                Ok(Ok(readback)) => {
                    tracing::info!(
                        "FrameTap: created readback handle ({:?}, {}x{})",
                        format,
                        width,
                        height,
                    );
                    self.readback = Some(readback);
                    self.readback_key = Some(key);
                    self.readback_creation_backoff = None;
                }
                Ok(Err(e)) => {
                    tracing::warn!("FrameTap: readback handle creation failed: {}", e);
                    self.readback_creation_backoff = Some(ReadbackCreationBackoff {
                        failed_key: key,
                        retry_at: Instant::now() + READBACK_CREATION_RETRY_BACKOFF,
                    });
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("FrameTap: escalate for readback creation failed: {}", e);
                    self.readback_creation_backoff = Some(ReadbackCreationBackoff {
                        failed_key: key,
                        retry_at: Instant::now() + READBACK_CREATION_RETRY_BACKOFF,
                    });
                    return Ok(());
                }
            }
        }

        // Scope the immutable borrow of `self.readback` (`!Clone`, stored
        // inline) so the extracted ticket outlives it — the `self.pending`
        // write below is then borrow-conflict-free.
        let ticket = {
            let readback = match self.readback.as_ref() {
                Some(rb) => rb,
                None => return Ok(()),
            };
            match readback.submit(&texture, source_layout_for(layout)) {
                Ok(ticket) => ticket,
                Err(e) => {
                    tracing::warn!("FrameTap: readback submit failed: {}", e);
                    return Ok(());
                }
            }
        };
        self.pending = Some(PendingReadback {
            ticket,
            width: texture.width(),
            height: texture.height(),
            color_type,
        });
        self.last_sample_at = Some(Instant::now());
        Ok(())
    }
}

/// Map a texture format to the matching `jpeg-encoder` input color type.
/// The encoder consumes the raw 4-byte pixels directly — no swizzle pass.
fn color_type_for(format: TextureFormat) -> Option<ColorType> {
    match format {
        TextureFormat::Bgra8Unorm => Some(ColorType::Bgra),
        TextureFormat::Rgba8Unorm => Some(ColorType::Rgba),
        _ => None,
    }
}

/// Map the texture's current Vulkan layout to the readback's source-layout
/// hint. The readback transitions `source → TRANSFER_SRC → source`, so the
/// hint must match the real layout to stay validation-clean.
fn source_layout_for(layout: VulkanLayout) -> TextureSourceLayout {
    if layout == VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
        TextureSourceLayout::ShaderReadOnly
    } else {
        TextureSourceLayout::General
    }
}

/// Whether a pending readback-creation backoff currently blocks a (device-
/// draining) `escalate` retry for `key` at `now`. Pure + rig-free so the
/// throttle bookkeeping is unit-testable without a GPU: a backoff blocks only
/// its own `failed_key`, and only until `retry_at` — a key change or an elapsed
/// backoff never blocks.
fn readback_creation_backoff_blocks(
    backoff: Option<&ReadbackCreationBackoff>,
    key: (u32, u32, TextureFormat),
    now: Instant,
) -> bool {
    match backoff {
        Some(b) => b.failed_key == key && now < b.retry_at,
        None => false,
    }
}

/// A completed sample ready to encode + write, owned by the writer thread.
struct SampleJob {
    bytes: Vec<u8>,
    width: u16,
    height: u16,
    color_type: ColorType,
    quality: u8,
    path: PathBuf,
}

/// Background JPEG writer: a bounded channel + a thread that encodes and
/// writes samples and enforces the filesystem caps.
struct SampleWriter {
    tx: Option<SyncSender<SampleJob>>,
    handle: Option<JoinHandle<()>>,
}

impl SampleWriter {
    fn spawn(output_dir: PathBuf, max_files: u32, max_total_bytes: u64) -> Self {
        let (tx, rx) = sync_channel::<SampleJob>(WRITER_QUEUE_DEPTH);
        let handle = std::thread::Builder::new()
            .name("frame-tap-writer".to_string())
            .spawn(move || writer_loop(rx, output_dir, max_files, max_total_bytes))
            .ok();
        Self {
            tx: Some(tx),
            handle,
        }
    }

    /// Enqueue a sample; returns `false` if the queue is full (drop-on-full)
    /// or the writer thread is gone.
    fn try_enqueue(&self, job: SampleJob) -> bool {
        match self.tx.as_ref() {
            Some(tx) => tx.try_send(job).is_ok(),
            None => false,
        }
    }
}

impl Drop for SampleWriter {
    fn drop(&mut self) {
        // Close the channel so the loop drains and exits, then join.
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn writer_loop(rx: Receiver<SampleJob>, output_dir: PathBuf, max_files: u32, max_total_bytes: u64) {
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        tracing::warn!(
            "FrameTap: failed to create output dir {:?}: {}",
            output_dir,
            e
        );
    }
    while let Ok(job) = rx.recv() {
        if let Err(e) = write_jpeg_atomic(&job) {
            tracing::warn!("FrameTap: failed to write sample {:?}: {}", job.path, e);
            continue;
        }
        enforce_caps(&output_dir, max_files, max_total_bytes);
    }
    tracing::debug!("FrameTap: writer thread exiting");
}

/// Encode to a temp file then rename, so a reader never sees a partial JPEG.
fn write_jpeg_atomic(job: &SampleJob) -> std::io::Result<()> {
    let tmp = job.path.with_extension("jpg.tmp");
    let encoder = Encoder::new_file(&tmp, job.quality).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("jpeg encoder: {e}"))
    })?;
    encoder
        .encode(&job.bytes, job.width, job.height, job.color_type)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("jpeg encode: {e}")))?;
    std::fs::rename(&tmp, &job.path)
}

/// Evict oldest-first until the sample dir is under both caps.
fn enforce_caps(dir: &Path, max_files: u32, max_total_bytes: u64) {
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("jpg") {
                    return None;
                }
                let md = entry.metadata().ok()?;
                let mtime = md.modified().ok()?;
                Some((path, mtime, md.len()))
            })
            .collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|(_, mtime, _)| *mtime);

    let mut total: u64 = entries.iter().map(|(_, _, sz)| *sz).sum();
    let mut count = entries.len() as u64;
    for (path, _, size) in &entries {
        if count <= max_files as u64 && total <= max_total_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(*size);
            count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: (u32, u32, TextureFormat) = (1920, 1080, TextureFormat::Bgra8Unorm);
    const KEY_B: (u32, u32, TextureFormat) = (1280, 720, TextureFormat::Rgba8Unorm);

    #[test]
    fn no_backoff_never_blocks() {
        // First attempt (or after a success cleared the backoff): nothing to
        // throttle, so creation is always allowed.
        assert!(!readback_creation_backoff_blocks(None, KEY_A, Instant::now()));
    }

    #[test]
    fn active_backoff_for_same_key_blocks_until_retry_at() {
        let now = Instant::now();
        let backoff = ReadbackCreationBackoff {
            failed_key: KEY_A,
            retry_at: now + Duration::from_millis(500),
        };
        // Same key, before retry_at → blocked (no per-frame re-escalate).
        assert!(readback_creation_backoff_blocks(
            Some(&backoff),
            KEY_A,
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn elapsed_backoff_allows_retry() {
        let now = Instant::now();
        let backoff = ReadbackCreationBackoff {
            failed_key: KEY_A,
            retry_at: now + Duration::from_millis(500),
        };
        // Same key, at/after retry_at → allowed (transient-failure recovery).
        assert!(!readback_creation_backoff_blocks(
            Some(&backoff),
            KEY_A,
            now + Duration::from_millis(500),
        ));
        assert!(!readback_creation_backoff_blocks(
            Some(&backoff),
            KEY_A,
            now + Duration::from_millis(600),
        ));
    }

    #[test]
    fn key_change_bypasses_active_backoff() {
        let now = Instant::now();
        let backoff = ReadbackCreationBackoff {
            failed_key: KEY_A,
            retry_at: now + Duration::from_millis(500),
        };
        // A different extent/format must retry immediately even mid-backoff.
        assert!(!readback_creation_backoff_blocks(
            Some(&backoff),
            KEY_B,
            now + Duration::from_millis(100),
        ));
    }
}
