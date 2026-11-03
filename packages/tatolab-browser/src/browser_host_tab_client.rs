// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One node's page as a tab in the machine's browser host: leased through the
//! module host table, opened over the tab protocol, and published through the
//! table as the host paints, plays and publishes.
//!
//! The host runs CEF for every stream on the machine; this node's helper runs
//! none. When the host ends, the runtime starts it again and the tab is
//! opened again in the new one.

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
    ModuleHost, ModuleHostHostMemoryPixels, ModuleHostInputPort, ModuleHostLogLevel,
    ModuleHostNodeOutputTextureRequest, ModuleHostOutputPort, ModuleHostSharedNativeServiceLease,
    ModuleHostSharedNativeServiceState,
};

use crate::page_audio_blocks::PageAudioStreamTimeline;
use crate::page_data_messages::{
    events_msgpack_map_of_published_json, page_data_detail_json_of_msgpack_map,
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

const DATA_INPUT_WAIT: Duration = Duration::from_millis(50);
const PAINTS_BETWEEN_REPORTS: u64 = 600;

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
    closing: AtomicBool,
}

/// One open connection to the host: its reading half and the tab's slots.
struct OpenBrowserHostTabConnection {
    reader: TabProtocolMessageReader,
    frame_slots: BrowserHostTabFrameSlots,
}

impl BrowserHostTabOfOneNode {
    fn log(&self, level: ModuleHostLogLevel, message: &str) {
        self.module_host.log(level, BROWSER_LOG_TARGET, message);
    }

    fn writer(&self) -> MutexGuard<'_, Option<TabProtocolMessageWriter>> {
        self.writer_to_the_browser_host
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send_to_the_browser_host(&self, message: &TabProtocolMessageFromNode) -> bool {
        match self.writer().as_mut() {
            Some(writer) => writer.send(message, &[]).is_ok(),
            None => false,
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
        let mut writer = TabProtocolMessageWriter::over(writing_half);
        writer
            .send(
                &TabProtocolMessageFromNode::OpenTab {
                    tab_protocol_version: TAB_PROTOCOL_VERSION,
                    tab_request: self.tab_request.clone(),
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
    fn serve_the_connection(&self, connection: &mut OpenBrowserHostTabConnection) {
        loop {
            let message = match connection
                .reader
                .receive::<TabProtocolMessageFromBrowserHost>()
            {
                Ok(message) => message,
                Err(TabProtocolSocketFailure::Ended) => return,
                Err(failure) => {
                    if !self.closing.load(Ordering::Acquire) {
                        self.log(
                            ModuleHostLogLevel::Warn,
                            &format!("the browser host connection failed: {failure}"),
                        );
                    }
                    return;
                }
            };
            match message {
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
                    if !self.send_to_the_browser_host(
                        &TabProtocolMessageFromNode::ReleaseFrameSlot { slot },
                    ) {
                        return;
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
                TabProtocolMessageFromBrowserHost::TabOpened { .. }
                | TabProtocolMessageFromBrowserHost::TabRefused { .. } => {
                    self.log(
                        ModuleHostLogLevel::Warn,
                        "the browser host answered an open a second time; the tab is reopened",
                    );
                    return;
                }
            }
        }
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
            Ok(()) => {
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
                            "{published} frames published, {} dropped",
                            self.counters.frames_dropped.load(Ordering::Relaxed)
                        ),
                    );
                }
            }
            Err(refusal) => self.drop_a_frame(&refusal),
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
        let painted_frame_texture = self
            .module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: PAINTED_FRAME_POOL_KEY,
                rotation_depth: self.config.output_pool_rotation_depth,
                width,
                height,
                texture_format: PAINTED_FRAME_TEXTURE_FORMAT,
                usage: &PAINTED_FRAME_TEXTURE_USAGE,
            })
            .map_err(|refusal| refusal.to_string())?;
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
            self.serve_the_connection(&mut connection);
            self.end_the_connection();
            if self.closing.load(Ordering::Acquire) {
                return;
            }
            self.log(
                ModuleHostLogLevel::Warn,
                "the browser host ended; the page opens again once the runtime has started it again",
            );
            match self.wait_for_the_browser_host_and_open_the_tab_again() {
                Some(reopened) => {
                    self.counters
                        .browser_host_reconnects
                        .fetch_add(1, Ordering::Relaxed);
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
        while !self.closing.load(Ordering::Acquire) && !self.module_host.stop_requested() {
            match self.lease.state() {
                Ok(ModuleHostSharedNativeServiceState::Running) => {
                    match self.open_the_tab_in_the_browser_host() {
                        Ok(connection) => return Some(connection),
                        Err(refusal) => last_refusal = refusal,
                    }
                }
                Ok(ModuleHostSharedNativeServiceState::StartingAgain) => {}
                Ok(ModuleHostSharedNativeServiceState::Failed(why)) => {
                    self.log(
                        ModuleHostLogLevel::Error,
                        &format!(
                            "the browser host failed and the runtime does not start it again, so \
                             the page stays closed: {why}"
                        ),
                    );
                    return None;
                }
                Err(refusal) => {
                    self.log(
                        ModuleHostLogLevel::Error,
                        &format!("the browser host lease cannot be read, so the page stays closed: {refusal}"),
                    );
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
    let counters = &tab.counters;
    let delivery_stopped = || tab.closing.load(Ordering::Acquire) || module_host.stop_requested();
    while !delivery_stopped() {
        match module_host.wait_for_any_input_port(DATA_INPUT_WAIT) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(refusal) => {
                tab.log(
                    ModuleHostLogLevel::Error,
                    &format!("data stopped reaching the page: {refusal}"),
                );
                return;
            }
        }
        while !delivery_stopped() {
            let message = match data_input_port.read() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(refusal) => {
                    tab.log(
                        ModuleHostLogLevel::Error,
                        &format!("data stopped reaching the page: {refusal}"),
                    );
                    return;
                }
            };
            let refusal = match page_data_detail_json_of_msgpack_map(message.payload()) {
                Ok(detail_json) => {
                    if tab.send_to_the_browser_host(&TabProtocolMessageFromNode::DeliverPageData {
                        detail_json,
                    }) {
                        counters
                            .data_messages_delivered
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    "the browser host is not connected".to_string()
                }
                Err(refusal) => refusal,
            };
            if counters
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
