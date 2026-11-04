// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One node's page as a tab in the machine's browser host: leased through the
//! module host table, opened over the tab protocol, and published through the
//! table as the host paints, plays and publishes.
//!
//! The host runs CEF for every stream on the machine; this node's helper runs
//! none. When the host ends, the runtime starts it again and the tab is
//! opened again in the new one.

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tatolab_browser_tab_protocol::{
    BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME, BrowserHostTabFrameSlots, BrowserHostTabLogLevel,
    BrowserHostTabRequest, TAB_PROTOCOL_VERSION, TabProtocolMessageFromBrowserHost,
    TabProtocolMessageFromNode, TabProtocolMessageReader, TabProtocolMessageWriter,
    TabProtocolSocketFailure,
};
use tatolab_module_sdk::{
    ModuleHost, ModuleHostColorInfo, ModuleHostForeignDmaBuf, ModuleHostHostMemoryPixels,
    ModuleHostInputPort, ModuleHostLogLevel, ModuleHostNodeOutputTextureRequest,
    ModuleHostOutputPort, ModuleHostSharedNativeServiceLease, ModuleHostSharedNativeServiceState,
    ModuleHostSurface,
};

use crate::page_audio_blocks::PageAudioStreamTimeline;
use crate::page_data_messages::{
    events_msgpack_map_of_published_json, page_data_detail_json_of_msgpack_map,
};
use crate::painted_textures_adopted_by_the_node::{
    PaintedTextureDmaBufIdentity, PaintedTextureLayout, PaintedTexturesAdoptedByTheNode,
};
use crate::video_frame_record::PaintedPageVideoFrame;

/// The `tracing` target this module's records carry.
const BROWSER_LOG_TARGET: &str = "tatolab_browser";

const PAINTED_FRAME_POOL_KEY: &str = "painted_page";
const PAINTED_FRAME_TEXTURE_FORMAT: &str = "rgba8_unorm";
const PAINTED_FRAME_TEXTURE_USAGE: [&str; 2] = ["texture_binding", "storage_binding"];
/// The host paints BGRA rows, which the write converts into the RGBA texture.
const PAINTED_PAGE_PIXEL_FORMAT: &str = "bgra32";

/// How many painted frames the host may hand this node before one comes back:
/// one being published, one waiting, and one the host paints into.
const BROWSER_HOST_TAB_FRAME_SLOT_COUNT: u32 = 3;

/// How long the host has to answer an `OpenTab`.
const BROWSER_HOST_TAB_OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a node whose host ended asks whether the new one is up.
const BROWSER_HOST_RECONNECT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a node waits for the host to come back before it says so again.
const BROWSER_HOST_RECONNECT_REPORT_INTERVAL: Duration = Duration::from_secs(30);
/// How long a running browser host may keep refusing to open the page again
/// before the node fails.
const LONGEST_REOPEN_REFUSAL_BY_A_RUNNING_BROWSER_HOST: Duration = Duration::from_secs(60);

const DATA_INPUT_WAIT: Duration = Duration::from_millis(50);
/// How many `data` messages wait for a restarting browser host before the oldest is dropped.
const MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY: usize = 256;
/// How many bytes of `data` JSON a node holds while the host is away.
const MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY: usize = 64 * 1024 * 1024;
/// How long one send to the host may take before the connection ends.
const SEND_TO_THE_BROWSER_HOST_BUDGET: Duration = Duration::from_secs(5);
const PAINTS_BETWEEN_REPORTS: u64 = 600;
/// How many painted textures in a row may fail to copy, once one has copied,
/// before the tab opens again painting into frame slots.
const MOST_PAINTED_TEXTURE_COPIES_FAILED_IN_A_ROW: u32 = 30;

/// Why a connection to the host stopped being served.
enum BrowserHostConnectionEnd {
    /// The host or the node ended it.
    Ended,
    /// A texture the page painted into could not be adopted, and why; the tab
    /// opens again painting into frame slots.
    PaintedTextureRefused(String),
}

/// One texture the host handed over with `TexturePainted`.
struct PaintedTextureHandedOver {
    width: u32,
    height: u32,
    pixel_format: String,
    allocation_byte_size: u64,
    plane_offsets_bytes: Vec<u64>,
    plane_strides_bytes: Vec<u64>,
    drm_format_modifier: u64,
}

/// How one send to the host went.
enum SendToTheBrowserHostOutcome {
    Sent,
    /// The message cannot be sent at all, and why; nothing was written.
    RefusedBeforeWriting(String),
    /// No connection, or the one there was has ended.
    BrowserHostAway,
}

/// What a page is opened with.
#[derive(Debug, Clone)]
pub struct BrowserPageConfig {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub transparent_background: bool,
    pub audio_sample_rate: u32,
    pub output_pool_rotation_depth: u32,
}

/// The ports one page publishes on and reads from, opened from the lent table.
pub struct BrowserPagePorts {
    pub module_host: ModuleHost,
    pub video_output_port: ModuleHostOutputPort,
    pub events_output_port: ModuleHostOutputPort,
    pub audio_output_port: ModuleHostOutputPort,
    pub data_input_port: ModuleHostInputPort,
}

/// How much a page has published and dropped since it opened.
#[derive(Debug, Default)]
pub struct BrowserPageCounters {
    pub frames_published: AtomicU64,
    /// Of `frames_published`, those copied from a texture the page painted
    /// into rather than uploaded from a frame slot.
    pub frames_published_from_painted_textures: AtomicU64,
    pub frames_dropped: AtomicU64,
    pub audio_blocks_published: AtomicU64,
    pub events_published: AtomicU64,
    pub data_messages_delivered: AtomicU64,
    pub data_messages_refused: AtomicU64,
    pub browser_host_reconnects: AtomicU64,
}

/// What the tab's two threads share.
struct BrowserHostTabOfOneNode {
    config: BrowserPageConfig,
    tab_request: BrowserHostTabRequest,
    module_host: ModuleHost,
    lease: ModuleHostSharedNativeServiceLease,
    video_output_port: ModuleHostOutputPort,
    events_output_port: ModuleHostOutputPort,
    audio_output_port: ModuleHostOutputPort,
    counters: Arc<BrowserPageCounters>,
    /// The connection's writing half, `None` between hosts.
    writer_to_the_browser_host: Mutex<Option<TabProtocolMessageWriter>>,
    audio_stream: Mutex<Option<PageAudioStreamTimeline>>,
    first_frame_refusal_logged: AtomicBool,
    /// Whether the tab is asked to paint into textures it hands over; cleared
    /// for good once one cannot be adopted.
    paints_into_painted_textures: AtomicBool,
    closing: AtomicBool,
    node_failed: AtomicBool,
}

/// One open connection to the host: its reading half, the tab's slots, and
/// the painted textures adopted over it.
struct OpenBrowserHostTabConnection {
    reader: TabProtocolMessageReader,
    frame_slots: BrowserHostTabFrameSlots,
    painted_textures_adopted: PaintedTexturesAdoptedByTheNode<ModuleHostSurface>,
    painted_texture_copies_failed_in_a_row: u32,
}

impl BrowserHostTabOfOneNode {
    fn log(&self, level: ModuleHostLogLevel, message: &str) {
        self.module_host.log(level, BROWSER_LOG_TARGET, message);
    }

    /// Fail this node for good with `reason`; the page stays closed.
    fn fail_the_node(&self, reason: &str) {
        self.node_failed.store(true, Ordering::Release);
        self.log(ModuleHostLogLevel::Error, reason);
        if let Err(refusal) = self.module_host.fail_the_node_unrecoverably(reason) {
            self.log(
                ModuleHostLogLevel::Error,
                &format!("the runtime was not told the node failed: {refusal}"),
            );
        }
    }

    fn writer(&self) -> MutexGuard<'_, Option<TabProtocolMessageWriter>> {
        self.writer_to_the_browser_host
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Send `message`. One refused before a byte was written leaves the
    /// connection as it was; a send that failed or ran past its budget leaves
    /// the stream partly written, so it ends the connection.
    fn send_to_the_browser_host(
        &self,
        message: &TabProtocolMessageFromNode,
    ) -> SendToTheBrowserHostOutcome {
        let mut writer = self.writer();
        let Some(open_writer) = writer.as_mut() else {
            return SendToTheBrowserHostOutcome::BrowserHostAway;
        };
        match open_writer.send(message, &[]) {
            Ok(()) => SendToTheBrowserHostOutcome::Sent,
            Err(TabProtocolSocketFailure::Encoding(refusal)) => {
                SendToTheBrowserHostOutcome::RefusedBeforeWriting(refusal)
            }
            Err(_) => {
                if let Some(failed_writer) = writer.take() {
                    failed_writer.shut_down();
                }
                SendToTheBrowserHostOutcome::BrowserHostAway
            }
        }
    }

    /// Connect to the host, open the tab, and map its frame slots.
    fn open_the_tab_in_the_browser_host(&self) -> Result<OpenBrowserHostTabConnection, String> {
        let endpoint = self.lease.endpoint_socket_path();
        let socket = UnixStream::connect(endpoint).map_err(|failure| {
            format!(
                "the browser host at {} did not accept: {failure}",
                endpoint.display()
            )
        })?;
        socket
            .set_read_timeout(Some(BROWSER_HOST_TAB_OPEN_TIMEOUT))
            .map_err(|failure| failure.to_string())?;
        let socket_whose_read_timeout_ends_once_open =
            socket.try_clone().map_err(|failure| failure.to_string())?;
        let writing_half = socket.try_clone().map_err(|failure| failure.to_string())?;
        let mut reader = TabProtocolMessageReader::over(socket);
        let mut writer = TabProtocolMessageWriter::over(writing_half)
            .sending_within(SEND_TO_THE_BROWSER_HOST_BUDGET);
        let tab_request = BrowserHostTabRequest {
            paint_into_shared_textures: self.paints_into_painted_textures.load(Ordering::Acquire),
            ..self.tab_request.clone()
        };
        writer
            .send(
                &TabProtocolMessageFromNode::OpenTab {
                    tab_protocol_version: TAB_PROTOCOL_VERSION,
                    tab_request,
                },
                &[],
            )
            .map_err(|failure| format!("the tab was not asked for: {failure}"))?;
        let frame_slots = match reader.receive::<TabProtocolMessageFromBrowserHost>() {
            Ok(TabProtocolMessageFromBrowserHost::TabOpened {
                tab_protocol_version,
                browser_host_process_id,
            }) => {
                let shared_memory = reader.take_attached_descriptor().map_err(|failure| {
                    format!("the tab's frame slots did not arrive: {failure}")
                })?;
                let frame_slots = BrowserHostTabFrameSlots::map_handed_over(
                    shared_memory,
                    self.tab_request.frame_slot_count,
                    self.tab_request.width,
                    self.tab_request.height,
                )
                .map_err(|failure| format!("the tab's frame slots were not mapped: {failure}"))?;
                self.log(
                    ModuleHostLogLevel::Info,
                    &format!(
                        "{} opened as a tab in the browser host {browser_host_process_id} \
                         (tab protocol {tab_protocol_version})",
                        self.config.url
                    ),
                );
                frame_slots
            }
            Ok(TabProtocolMessageFromBrowserHost::TabRefused { reason }) => {
                return Err(format!("the browser host refused the page: {reason}"));
            }
            Ok(other) => {
                return Err(format!(
                    "the browser host answered the open with {other:?}, not tab_opened"
                ));
            }
            Err(failure) => {
                return Err(format!(
                    "the browser host did not answer the open: {failure}"
                ));
            }
        };
        socket_whose_read_timeout_ends_once_open
            .set_read_timeout(None)
            .map_err(|failure| failure.to_string())?;
        *self.writer() = Some(writer);
        Ok(OpenBrowserHostTabConnection {
            reader,
            frame_slots,
            painted_textures_adopted: PaintedTexturesAdoptedByTheNode::default(),
            painted_texture_copies_failed_in_a_row: 0,
        })
    }

    fn end_the_connection(&self) {
        if let Some(writer) = self.writer().take() {
            writer.shut_down();
        }
        self.audio_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Serve what the host sends until the connection ends.
    fn serve_the_connection(
        &self,
        connection: &mut OpenBrowserHostTabConnection,
    ) -> BrowserHostConnectionEnd {
        loop {
            let message = match connection
                .reader
                .receive::<TabProtocolMessageFromBrowserHost>()
            {
                Ok(message) => message,
                Err(TabProtocolSocketFailure::Ended) => return BrowserHostConnectionEnd::Ended,
                Err(failure) => {
                    if !self.closing.load(Ordering::Acquire) {
                        self.log(
                            ModuleHostLogLevel::Warn,
                            &format!("the browser host connection failed: {failure}"),
                        );
                    }
                    return BrowserHostConnectionEnd::Ended;
                }
            };
            match message {
                TabProtocolMessageFromBrowserHost::TexturePainted {
                    painted_texture_number,
                    width,
                    height,
                    pixel_format,
                    allocation_byte_size,
                    plane_offsets_bytes,
                    plane_strides_bytes,
                    drm_format_modifier,
                } => {
                    let published = self.publish_the_painted_texture(
                        connection,
                        PaintedTextureHandedOver {
                            width,
                            height,
                            pixel_format,
                            allocation_byte_size,
                            plane_offsets_bytes,
                            plane_strides_bytes,
                            drm_format_modifier,
                        },
                    );
                    connection.reader.discard_attached_descriptors();
                    let answered = self.send_to_the_browser_host(
                        &TabProtocolMessageFromNode::PaintedTextureCopied {
                            painted_texture_number,
                        },
                    );
                    if let Err(refusal) = published {
                        return BrowserHostConnectionEnd::PaintedTextureRefused(refusal);
                    }
                    if !matches!(answered, SendToTheBrowserHostOutcome::Sent) {
                        return BrowserHostConnectionEnd::Ended;
                    }
                }
                TabProtocolMessageFromBrowserHost::FramePainted {
                    slot,
                    width,
                    height,
                    row_stride_bytes,
                } => {
                    self.publish_the_frame_in_slot(
                        &connection.frame_slots,
                        slot,
                        width,
                        height,
                        row_stride_bytes,
                    );
                    if !matches!(
                        self.send_to_the_browser_host(
                            &TabProtocolMessageFromNode::ReleaseFrameSlot { slot },
                        ),
                        SendToTheBrowserHostOutcome::Sent
                    ) {
                        return BrowserHostConnectionEnd::Ended;
                    }
                }
                TabProtocolMessageFromBrowserHost::AudioStreamStarted {
                    sample_rate,
                    channels,
                } => {
                    *self
                        .audio_stream
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(PageAudioStreamTimeline::new(sample_rate, channels));
                    self.log(
                        ModuleHostLogLevel::Info,
                        &format!(
                            "the page opened an audio stream: {sample_rate} Hz × {channels} ch"
                        ),
                    );
                }
                TabProtocolMessageFromBrowserHost::AudioPacket {
                    sample_count,
                    interleaved_samples_le_f32,
                } => self.publish_audio_packet(sample_count, &interleaved_samples_le_f32),
                TabProtocolMessageFromBrowserHost::AudioStreamStopped => {
                    self.audio_stream
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take();
                }
                TabProtocolMessageFromBrowserHost::PagePublished { published_json } => {
                    self.publish_page_object(&published_json);
                }
                TabProtocolMessageFromBrowserHost::TabLog { level, message } => {
                    self.log(module_log_level_of(level), &message);
                }
                TabProtocolMessageFromBrowserHost::PageRenderingGaveUp { reason } => {
                    self.fail_the_node(&reason);
                    return BrowserHostConnectionEnd::Ended;
                }
                TabProtocolMessageFromBrowserHost::TabOpened { .. }
                | TabProtocolMessageFromBrowserHost::TabRefused { .. } => {
                    self.log(
                        ModuleHostLogLevel::Warn,
                        "the browser host answered an open a second time; the tab is reopened",
                    );
                    return BrowserHostConnectionEnd::Ended;
                }
            }
        }
    }

    /// Publish the texture the page painted into, adopting it the first time
    /// it is handed over; `Err` when it cannot be adopted, or when the first
    /// copy or [`MOST_PAINTED_TEXTURE_COPIES_FAILED_IN_A_ROW`] copies in a row
    /// fail, since the frame slots still work then.
    fn publish_the_painted_texture(
        &self,
        connection: &mut OpenBrowserHostTabConnection,
        painted: PaintedTextureHandedOver,
    ) -> Result<(), String> {
        let mut plane_dma_bufs = Vec::with_capacity(painted.plane_offsets_bytes.len());
        for _ in 0..painted.plane_offsets_bytes.len() {
            let plane_dma_buf =
                connection
                    .reader
                    .take_attached_descriptor()
                    .map_err(|failure| {
                        format!("a painted texture's plane did not arrive: {failure}")
                    })?;
            plane_dma_bufs.push(File::from(plane_dma_buf));
        }
        if self.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        let dma_buf_identity = the_one_dma_buf_every_plane_lives_in(&plane_dma_bufs)?;
        let Some(dma_buf) = plane_dma_bufs.into_iter().next() else {
            return Err("a painted texture was handed over with no plane".to_string());
        };
        let layout = PaintedTextureLayout {
            dma_buf_identity,
            width: painted.width,
            height: painted.height,
            pixel_format: painted.pixel_format,
            allocation_byte_size: painted.allocation_byte_size,
            plane_offsets_bytes: painted.plane_offsets_bytes,
            plane_strides_bytes: painted.plane_strides_bytes,
            drm_format_modifier: painted.drm_format_modifier,
        };
        let (width, height) = (layout.width, layout.height);
        let adopted_layout = layout.clone();
        let adopted_surface =
            connection
                .painted_textures_adopted
                .adopted_or_adopt(layout, dma_buf, |dma_buf| {
                    self.module_host
                        .import_dma_buf(&ModuleHostForeignDmaBuf {
                            fd: dma_buf.as_raw_fd(),
                            width: adopted_layout.width,
                            height: adopted_layout.height,
                            pixel_format: &adopted_layout.pixel_format,
                            allocation_byte_size: adopted_layout.allocation_byte_size,
                            plane_offsets_bytes: &adopted_layout.plane_offsets_bytes,
                            plane_strides_bytes: &adopted_layout.plane_strides_bytes,
                            drm_format_modifier: adopted_layout.drm_format_modifier,
                        })
                        .map_err(|refusal| {
                            format!(
                                "the runtime did not adopt the {} texture the page painted into \
                             (DRM format modifier {:#x}): {refusal}",
                                adopted_layout.pixel_format, adopted_layout.drm_format_modifier
                            )
                        })
                })?;
        let painted_frame_texture = match self.acquire_a_painted_frame_texture(width, height) {
            Ok(painted_frame_texture) => painted_frame_texture,
            Err(refusal) => {
                self.drop_a_frame(&refusal);
                return Ok(());
            }
        };
        if let Err(refusal) = self.module_host.convert_surface_into(
            adopted_surface,
            &painted_frame_texture,
            &ModuleHostColorInfo::default(),
        ) {
            let refusal = format!("the texture the page painted into was not copied: {refusal}");
            connection.painted_texture_copies_failed_in_a_row += 1;
            let a_painted_texture_was_ever_copied = self
                .counters
                .frames_published_from_painted_textures
                .load(Ordering::Relaxed)
                > 0;
            if !a_painted_texture_was_ever_copied
                || connection.painted_texture_copies_failed_in_a_row
                    >= MOST_PAINTED_TEXTURE_COPIES_FAILED_IN_A_ROW
            {
                return Err(refusal);
            }
            self.drop_a_frame(&refusal);
            return Ok(());
        }
        connection.painted_texture_copies_failed_in_a_row = 0;
        match self.publish_the_painted_frame_texture(&painted_frame_texture, width, height) {
            Ok(()) => {
                self.counters
                    .frames_published_from_painted_textures
                    .fetch_add(1, Ordering::Relaxed);
                self.count_one_published_frame();
            }
            Err(refusal) => self.drop_a_frame(&refusal),
        }
        Ok(())
    }

    fn publish_the_frame_in_slot(
        &self,
        frame_slots: &BrowserHostTabFrameSlots,
        slot: u32,
        width: u32,
        height: u32,
        row_stride_bytes: u32,
    ) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: the host handed this slot over with `FramePainted` and does
        // not paint into it again until it is released, after this returns.
        let Some(painted_bgra_rows) = (unsafe { frame_slots.slot(slot) }) else {
            self.drop_a_frame(&format!(
                "the browser host named slot {slot} of {}",
                frame_slots.slot_count()
            ));
            return;
        };
        match self.write_painted_frame(painted_bgra_rows, width, height, row_stride_bytes) {
            Ok(()) => self.count_one_published_frame(),
            Err(refusal) => self.drop_a_frame(&refusal),
        }
    }

    fn count_one_published_frame(&self) {
        let published = self
            .counters
            .frames_published
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if published == 1 {
            self.log(ModuleHostLogLevel::Info, "first frame published");
        }
        if published.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
            self.log(
                ModuleHostLogLevel::Info,
                &format!(
                    "{published} frames published, {} of them from painted textures, {} dropped",
                    self.counters
                        .frames_published_from_painted_textures
                        .load(Ordering::Relaxed),
                    self.counters.frames_dropped.load(Ordering::Relaxed)
                ),
            );
        }
    }

    fn drop_a_frame(&self, refusal: &str) {
        self.counters.frames_dropped.fetch_add(1, Ordering::Relaxed);
        if !self
            .first_frame_refusal_logged
            .swap(true, Ordering::Relaxed)
        {
            self.log(
                ModuleHostLogLevel::Warn,
                &format!("dropping a painted frame: {refusal}"),
            );
        }
    }

    fn write_painted_frame(
        &self,
        painted_bgra_rows: &[u8],
        width: u32,
        height: u32,
        row_stride_bytes: u32,
    ) -> Result<(), String> {
        let painted_frame_texture = self.acquire_a_painted_frame_texture(width, height)?;
        self.module_host
            .write_surface_from_host_memory(
                &painted_frame_texture,
                &ModuleHostHostMemoryPixels {
                    pixels: painted_bgra_rows,
                    pixel_format: PAINTED_PAGE_PIXEL_FORMAT,
                    width,
                    height,
                    row_stride_bytes: u64::from(row_stride_bytes),
                },
            )
            .map_err(|refusal| refusal.to_string())?;
        self.publish_the_painted_frame_texture(&painted_frame_texture, width, height)
    }

    fn acquire_a_painted_frame_texture(
        &self,
        width: u32,
        height: u32,
    ) -> Result<ModuleHostSurface, String> {
        self.module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: PAINTED_FRAME_POOL_KEY,
                rotation_depth: self.config.output_pool_rotation_depth,
                width,
                height,
                texture_format: PAINTED_FRAME_TEXTURE_FORMAT,
                usage: &PAINTED_FRAME_TEXTURE_USAGE,
            })
            .map_err(|refusal| refusal.to_string())
    }

    fn publish_the_painted_frame_texture(
        &self,
        painted_frame_texture: &ModuleHostSurface,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let timestamp_ns = self.module_host.monotonic_now_ns();
        let record = PaintedPageVideoFrame {
            surface_id: painted_frame_texture.surface_id(),
            width,
            height,
            timestamp_ns,
            frames_per_second: self.config.frame_rate,
        }
        .to_msgpack();
        self.video_output_port
            .write(&record, Some(timestamp_ns), true)
            .map_err(|refusal| refusal.to_string())
    }

    fn publish_page_object(&self, published_json: &str) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let written = events_msgpack_map_of_published_json(published_json).and_then(|payload| {
            self.events_output_port
                .write(&payload, None, true)
                .map_err(|refusal| refusal.to_string())
        });
        match written {
            Ok(()) => {
                self.counters
                    .events_published
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(refusal) => self.log(
                ModuleHostLogLevel::Warn,
                &format!("a published object was not written to events: {refusal}"),
            ),
        }
    }

    fn publish_audio_packet(&self, sample_count: u32, interleaved_samples_le_f32: &[u8]) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let arrived_at_ns = self.module_host.monotonic_now_ns();
        let mut audio_stream = self
            .audio_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(timeline) = audio_stream.as_mut() else {
            return;
        };
        let interleaved_samples = samples_of_little_endian_f32_bytes(interleaved_samples_le_f32);
        if interleaved_samples.len() != sample_count as usize * timeline.channels() as usize {
            self.log(
                ModuleHostLogLevel::Warn,
                &format!(
                    "an audio packet of {sample_count} frames × {} ch carried {} samples; it \
                     is dropped",
                    timeline.channels(),
                    interleaved_samples.len()
                ),
            );
            return;
        }
        let first_sample_timestamp_ns = timeline.stamp_the_next_packet(sample_count, arrived_at_ns);
        let payload = timeline.audio_block_message_payload(
            &interleaved_samples,
            sample_count,
            first_sample_timestamp_ns,
        );
        match self
            .audio_output_port
            .write(&payload, Some(first_sample_timestamp_ns), true)
        {
            Ok(()) => {
                self.counters
                    .audio_blocks_published
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(refusal) => self.log(
                ModuleHostLogLevel::Warn,
                &format!("an audio block was not written: {refusal}"),
            ),
        }
    }

    /// Keep the tab open in the host until the node closes it, opening it
    /// again in each host the runtime starts after one ends.
    fn keep_the_tab_open(&self, mut connection: OpenBrowserHostTabConnection) {
        loop {
            let connection_end = self.serve_the_connection(&mut connection);
            self.end_the_connection();
            if self.closing.load(Ordering::Acquire) || self.node_failed.load(Ordering::Acquire) {
                return;
            }
            let browser_host_ended = match connection_end {
                BrowserHostConnectionEnd::Ended => {
                    self.log(
                        ModuleHostLogLevel::Warn,
                        "the browser host ended; the page opens again once the runtime has \
                         started it again",
                    );
                    true
                }
                BrowserHostConnectionEnd::PaintedTextureRefused(refusal) => {
                    self.paints_into_painted_textures
                        .store(false, Ordering::Release);
                    self.log(
                        ModuleHostLogLevel::Info,
                        &format!(
                            "{refusal}; the page opens again painting into frame slots, each \
                             frame uploaded from host memory"
                        ),
                    );
                    false
                }
            };
            match self.wait_for_the_browser_host_and_open_the_tab_again() {
                Some(reopened) => {
                    if browser_host_ended {
                        self.counters
                            .browser_host_reconnects
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    connection = reopened;
                }
                None => return,
            }
        }
    }

    fn wait_for_the_browser_host_and_open_the_tab_again(
        &self,
    ) -> Option<OpenBrowserHostTabConnection> {
        let mut next_report = Instant::now() + BROWSER_HOST_RECONNECT_REPORT_INTERVAL;
        let mut last_refusal = String::new();
        let mut refused_by_a_running_browser_host_since: Option<Instant> = None;
        while !self.closing.load(Ordering::Acquire) && !self.module_host.stop_requested() {
            match self.lease.state() {
                Ok(ModuleHostSharedNativeServiceState::Running) => {
                    match self.open_the_tab_in_the_browser_host() {
                        Ok(connection) => return Some(connection),
                        Err(refusal) => last_refusal = refusal,
                    }
                    let refused_since =
                        *refused_by_a_running_browser_host_since.get_or_insert_with(Instant::now);
                    if refused_since.elapsed() >= LONGEST_REOPEN_REFUSAL_BY_A_RUNNING_BROWSER_HOST {
                        self.fail_the_node(&format!(
                            "the browser host is running but kept refusing to open the page \
                             again for {LONGEST_REOPEN_REFUSAL_BY_A_RUNNING_BROWSER_HOST:?}: \
                             {last_refusal}"
                        ));
                        return None;
                    }
                }
                Ok(ModuleHostSharedNativeServiceState::StartingAgain) => {
                    refused_by_a_running_browser_host_since = None;
                }
                Ok(ModuleHostSharedNativeServiceState::Failed(why)) => {
                    self.fail_the_node(&format!(
                        "the browser host failed and the runtime does not start it again, so \
                         the page stays closed: {why}"
                    ));
                    return None;
                }
                Err(refusal) => {
                    self.fail_the_node(&format!(
                        "the browser host lease cannot be read, so the page stays closed: \
                         {refusal}"
                    ));
                    return None;
                }
            }
            if Instant::now() >= next_report {
                next_report = Instant::now() + BROWSER_HOST_RECONNECT_REPORT_INTERVAL;
                self.log(
                    ModuleHostLogLevel::Warn,
                    &format!(
                        "the page is still waiting for the browser host{}",
                        if last_refusal.is_empty() {
                            String::new()
                        } else {
                            format!(": {last_refusal}")
                        }
                    ),
                );
            }
            std::thread::park_timeout(BROWSER_HOST_RECONNECT_POLL_INTERVAL);
        }
        None
    }
}

/// The one DMA-BUF every plane of a painted texture lives in; the runtime
/// adopts a texture from one descriptor.
fn the_one_dma_buf_every_plane_lives_in(
    plane_dma_bufs: &[File],
) -> Result<PaintedTextureDmaBufIdentity, String> {
    let mut identities = plane_dma_bufs.iter().map(|plane_dma_buf| {
        PaintedTextureDmaBufIdentity::of(plane_dma_buf)
            .map_err(|failure| format!("a painted texture's plane cannot be read: {failure}"))
    });
    let Some(first_identity) = identities.next().transpose()? else {
        return Err("a painted texture was handed over with no plane".to_string());
    };
    for identity in identities {
        if identity? != first_identity {
            return Err(
                "the page painted a texture whose planes live in separate DMA-BUFs".to_string(),
            );
        }
    }
    Ok(first_identity)
}

fn module_log_level_of(level: BrowserHostTabLogLevel) -> ModuleHostLogLevel {
    match level {
        BrowserHostTabLogLevel::Error => ModuleHostLogLevel::Error,
        BrowserHostTabLogLevel::Warn => ModuleHostLogLevel::Warn,
        BrowserHostTabLogLevel::Info => ModuleHostLogLevel::Info,
        BrowserHostTabLogLevel::Debug => ModuleHostLogLevel::Debug,
    }
}

/// The `f32` samples little-endian `bytes` hold; a trailing partial sample is
/// left out.
fn samples_of_little_endian_f32_bytes(bytes: &[u8]) -> Vec<f32> {
    let (samples, _partial_sample) = bytes.as_chunks::<4>();
    samples.iter().copied().map(f32::from_le_bytes).collect()
}

/// A page rendering for one node: open from `open` until `close`.
pub struct BrowserPageRendering {
    tab: Arc<BrowserHostTabOfOneNode>,
    connection_thread: Option<JoinHandle<()>>,
    data_input_thread: Option<JoinHandle<()>>,
}

impl BrowserPageRendering {
    /// Lease the machine's browser host, open `config.url` as a tab in it, and
    /// start publishing what it paints and delivering `data` to it.
    pub fn open(config: BrowserPageConfig, ports: BrowserPagePorts) -> Result<Self, String> {
        refuse_a_config_cef_cannot_render(&config)?;
        let lease = ports
            .module_host
            .lease_shared_native_service(BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME)
            .map_err(|refusal| format!("the browser host was not leased: {refusal}"))?;
        let tab_request = BrowserHostTabRequest {
            tenant_stream_id: lease.tenant_stream_id().to_string(),
            url: config.url.clone(),
            width: config.width,
            height: config.height,
            frame_rate: config.frame_rate,
            transparent_background: config.transparent_background,
            audio_sample_rate: config.audio_sample_rate,
            frame_slot_count: BROWSER_HOST_TAB_FRAME_SLOT_COUNT,
            paint_into_shared_textures: true,
        };
        tab_request.refuse_what_cef_cannot_render()?;
        let tab = Arc::new(BrowserHostTabOfOneNode {
            config,
            tab_request,
            module_host: ports.module_host.clone(),
            lease,
            video_output_port: ports.video_output_port,
            events_output_port: ports.events_output_port,
            audio_output_port: ports.audio_output_port,
            counters: Arc::new(BrowserPageCounters::default()),
            writer_to_the_browser_host: Mutex::new(None),
            audio_stream: Mutex::new(None),
            first_frame_refusal_logged: AtomicBool::new(false),
            paints_into_painted_textures: AtomicBool::new(true),
            node_failed: AtomicBool::new(false),
            closing: AtomicBool::new(false),
        });
        let connection = tab.open_the_tab_in_the_browser_host()?;
        let connection_thread = std::thread::Builder::new()
            .name("tatolab-browser-tab".to_string())
            .spawn({
                let tab = Arc::clone(&tab);
                move || tab.keep_the_tab_open(connection)
            })
            .map_err(|failure| {
                tab.end_the_connection();
                format!("the tab's connection thread did not start: {failure}")
            })?;
        let data_input_thread = std::thread::Builder::new()
            .name("tatolab-browser-data".to_string())
            .spawn({
                let tab = Arc::clone(&tab);
                let data_input_port = ports.data_input_port;
                move || deliver_data_messages_to_the_page(&tab, &data_input_port)
            });
        let mut rendering = Self {
            tab,
            connection_thread: Some(connection_thread),
            data_input_thread: None,
        };
        match data_input_thread {
            Ok(data_input_thread) => rendering.data_input_thread = Some(data_input_thread),
            Err(failure) => {
                rendering.close();
                return Err(format!("the data thread did not start: {failure}"));
            }
        }
        Ok(rendering)
    }

    pub fn counters(&self) -> &BrowserPageCounters {
        &self.tab.counters
    }

    /// Stop publishing and delivering data and close the tab. Idempotent.
    pub fn close(&mut self) {
        self.tab.closing.store(true, Ordering::Release);
        if let Some(data_input_thread) = self.data_input_thread.take() {
            let _ = data_input_thread.join();
        }
        self.tab
            .send_to_the_browser_host(&TabProtocolMessageFromNode::CloseTab);
        self.tab.end_the_connection();
        if let Some(connection_thread) = self.connection_thread.take() {
            connection_thread.thread().unpark();
            let _ = connection_thread.join();
        }
    }
}

impl Drop for BrowserPageRendering {
    fn drop(&mut self) {
        self.close();
    }
}

/// Refuse what CEF's windowless renderer cannot paint before the host is asked.
pub fn refuse_a_config_cef_cannot_render(config: &BrowserPageConfig) -> Result<(), String> {
    if config.url.trim().is_empty() {
        return Err("a browser source needs a url".to_string());
    }
    if config.width == 0 || config.height == 0 || config.width > 8192 || config.height > 8192 {
        return Err(format!(
            "a page renders at 1x1 to 8192x8192, not {}x{}",
            config.width, config.height
        ));
    }
    if !(1..=60).contains(&config.frame_rate) {
        return Err(format!(
            "CEF paints a windowless page at 1 to 60 frames a second, not {}",
            config.frame_rate
        ));
    }
    if !(8_000..=192_000).contains(&config.audio_sample_rate) {
        return Err(format!(
            "audio_sample_rate is 8000 to 192000 Hz, not {}",
            config.audio_sample_rate
        ));
    }
    if config.output_pool_rotation_depth == 0 {
        return Err("output_pool_rotation_depth is at least 1".to_string());
    }
    Ok(())
}

fn deliver_data_messages_to_the_page(
    tab: &BrowserHostTabOfOneNode,
    data_input_port: &ModuleHostInputPort,
) {
    let module_host = &tab.module_host;
    let delivery_stopped = || tab.closing.load(Ordering::Acquire) || module_host.stop_requested();
    let mut held_while_the_browser_host_is_away = DataMessagesHeldForTheBrowserHost::default();
    while !delivery_stopped() {
        send_the_data_messages_held_for_the_browser_host(
            tab,
            &mut held_while_the_browser_host_is_away,
        );
        match module_host.wait_for_any_input_port(DATA_INPUT_WAIT) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(refusal) => {
                tab.fail_the_node(&format!("data stopped reaching the page: {refusal}"));
                return;
            }
        }
        while !delivery_stopped() {
            let message = match data_input_port.read() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(refusal) => {
                    tab.fail_the_node(&format!("data stopped reaching the page: {refusal}"));
                    return;
                }
            };
            match page_data_detail_json_of_msgpack_map(message.payload()) {
                Ok(detail_json) => {
                    for _ in 0..held_while_the_browser_host_is_away.hold_newest(detail_json) {
                        count_a_data_message_that_did_not_reach_the_page(
                            tab,
                            "the browser host is away, and the oldest data message held for it was dropped",
                        );
                    }
                    send_the_data_messages_held_for_the_browser_host(
                        tab,
                        &mut held_while_the_browser_host_is_away,
                    );
                }
                Err(refusal) => count_a_data_message_that_did_not_reach_the_page(tab, &refusal),
            }
        }
    }
}

/// The newest `data` messages, as page JSON, waiting for the host: at most
/// [`MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY`] of them and
/// [`MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY`] bytes, the newest
/// one always kept.
#[derive(Default)]
struct DataMessagesHeldForTheBrowserHost {
    detail_jsons: VecDeque<String>,
    bytes_held: usize,
}

impl DataMessagesHeldForTheBrowserHost {
    /// Hold `detail_json`, dropping the oldest past either bound; returns how
    /// many were dropped.
    fn hold_newest(&mut self, detail_json: String) -> usize {
        self.bytes_held += detail_json.len();
        self.detail_jsons.push_back(detail_json);
        let mut dropped = 0;
        while self.detail_jsons.len() > 1
            && (self.detail_jsons.len() > MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY
                || self.bytes_held > MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY)
        {
            self.take_oldest();
            dropped += 1;
        }
        dropped
    }

    fn take_oldest(&mut self) -> Option<String> {
        let oldest = self.detail_jsons.pop_front()?;
        self.bytes_held -= oldest.len();
        Some(oldest)
    }

    fn put_back_as_oldest(&mut self, detail_json: String) {
        self.bytes_held += detail_json.len();
        self.detail_jsons.push_front(detail_json);
    }
}

/// Send the held `data` messages oldest first, keeping the rest held once a
/// send fails because the browser host is away.
fn send_the_data_messages_held_for_the_browser_host(
    tab: &BrowserHostTabOfOneNode,
    held_while_the_browser_host_is_away: &mut DataMessagesHeldForTheBrowserHost,
) {
    while let Some(detail_json) = held_while_the_browser_host_is_away.take_oldest() {
        let message = TabProtocolMessageFromNode::DeliverPageData { detail_json };
        match tab.send_to_the_browser_host(&message) {
            SendToTheBrowserHostOutcome::Sent => {}
            SendToTheBrowserHostOutcome::RefusedBeforeWriting(refusal) => {
                count_a_data_message_that_did_not_reach_the_page(tab, &refusal);
                continue;
            }
            SendToTheBrowserHostOutcome::BrowserHostAway => {
                if let TabProtocolMessageFromNode::DeliverPageData { detail_json } = message {
                    held_while_the_browser_host_is_away.put_back_as_oldest(detail_json);
                }
                return;
            }
        }
        tab.counters
            .data_messages_delivered
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn count_a_data_message_that_did_not_reach_the_page(tab: &BrowserHostTabOfOneNode, refusal: &str) {
    if tab
        .counters
        .data_messages_refused
        .fetch_add(1, Ordering::Relaxed)
        == 0
    {
        tab.log(
            ModuleHostLogLevel::Warn,
            &format!("a data message did not reach the page: {refusal}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BrowserPageConfig {
        BrowserPageConfig {
            url: "https://example.com".to_string(),
            width: 1920,
            height: 1080,
            frame_rate: 60,
            transparent_background: true,
            audio_sample_rate: 48_000,
            output_pool_rotation_depth: 4,
        }
    }

    #[test]
    fn held_data_drops_the_oldest_past_either_bound_and_keeps_the_newest() {
        let mut held = DataMessagesHeldForTheBrowserHost::default();
        for index in 0..MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY {
            assert_eq!(held.hold_newest(index.to_string()), 0);
        }
        assert_eq!(held.hold_newest("newest".to_string()), 1);
        assert_eq!(held.take_oldest().as_deref(), Some("1"));

        let mut held = DataMessagesHeldForTheBrowserHost::default();
        let half_the_bytes = "x".repeat(MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY / 2);
        assert_eq!(held.hold_newest(half_the_bytes.clone()), 0);
        assert_eq!(held.hold_newest(half_the_bytes), 0);
        assert_eq!(held.hold_newest("past the bound".to_string()), 1);
        let larger_than_the_bound =
            "y".repeat(MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY + 1);
        assert_eq!(held.hold_newest(larger_than_the_bound.clone()), 2);
        assert_eq!(held.take_oldest(), Some(larger_than_the_bound));
        assert_eq!(held.bytes_held, 0);
    }

    #[test]
    fn a_config_cef_can_render_passes() {
        refuse_a_config_cef_cannot_render(&config()).expect("1080p60 renders");
    }

    #[test]
    fn what_cef_cannot_render_is_refused_by_name() {
        for (broken, named) in [
            (
                BrowserPageConfig {
                    url: " ".into(),
                    ..config()
                },
                "needs a url",
            ),
            (
                BrowserPageConfig {
                    width: 0,
                    ..config()
                },
                "0x1080",
            ),
            (
                BrowserPageConfig {
                    frame_rate: 120,
                    ..config()
                },
                "not 120",
            ),
            (
                BrowserPageConfig {
                    audio_sample_rate: 4_000,
                    ..config()
                },
                "not 4000",
            ),
            (
                BrowserPageConfig {
                    output_pool_rotation_depth: 0,
                    ..config()
                },
                "at least 1",
            ),
        ] {
            let refusal = refuse_a_config_cef_cannot_render(&broken).expect_err(named);
            assert!(refusal.contains(named), "{refusal}");
        }
    }

    #[test]
    fn interleaved_little_endian_samples_are_read_back_and_a_partial_one_is_left_out() {
        let mut bytes: Vec<u8> = [0.5f32, -1.0, 0.25]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        bytes.push(7);
        assert_eq!(
            samples_of_little_endian_f32_bytes(&bytes),
            [0.5, -1.0, 0.25]
        );
    }

    #[test]
    fn every_host_log_level_keeps_its_loudness() {
        assert_eq!(
            [
                BrowserHostTabLogLevel::Error,
                BrowserHostTabLogLevel::Warn,
                BrowserHostTabLogLevel::Info,
                BrowserHostTabLogLevel::Debug,
            ]
            .map(module_log_level_of),
            [
                ModuleHostLogLevel::Error,
                ModuleHostLogLevel::Warn,
                ModuleHostLogLevel::Info,
                ModuleHostLogLevel::Debug,
            ]
        );
    }
}
