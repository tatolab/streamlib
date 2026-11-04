// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One tab: the page a node asked for, and everything CEF's handlers send that
//! node over its connection.

use std::collections::HashMap;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use tatolab_browser_tab_protocol::PaintedIOSurfaceSenderOfTheBrowserHost;
use tatolab_browser_tab_protocol::{
    BrowserHostTabFrameSlots, BrowserHostTabLogLevel, BrowserHostTabRequest, PageInputTrackKind,
    TAB_PROTOCOL_VERSION, TabProtocolMessageFromBrowserHost, TabProtocolMessageWriter,
    TabProtocolSocketFailure,
};

/// How many painted frames go by between two reports of what a tab painted.
const PAINTS_BETWEEN_REPORTS: u64 = 600;

/// The most `data` messages a tab queues for the CEF thread; past it, a
/// message is dropped and the node told.
const MOST_PAGE_DATA_REQUESTS_QUEUED: u32 = 256;
/// How many bytes of `data` one tab may queue for the CEF thread.
const MOST_PAGE_DATA_BYTES_QUEUED: u64 = 64 * 1024 * 1024;

/// The most frames of one video input queued for the page; past it, the
/// newest is dropped so the page draws the latest frames the CEF thread keeps
/// up with.
const MOST_PAGE_INPUT_VIDEO_FRAMES_QUEUED_PER_INPUT: u32 = 2;
/// The most blocks of one audio input queued for the page.
const MOST_PAGE_INPUT_AUDIO_BLOCKS_QUEUED_PER_INPUT: u32 = 32;

/// How long the CEF thread waits for the node to copy a painted texture
/// before the page may paint into it again; past it, the page goes on and the
/// wait is counted.
const PAINTED_TEXTURE_COPY_BUDGET: Duration = Duration::from_millis(100);

/// A tab id, unique for the life of the host.
pub type BrowserHostTabId = u64;

/// One tab and its connection to the node that asked for it.
pub struct BrowserHostTab {
    pub tab_id: BrowserHostTabId,
    pub tab_request: BrowserHostTabRequest,
    writer_to_the_node: Mutex<TabProtocolMessageWriter>,
    pub frame_slots: BrowserHostTabFrameSlots,
    /// Bit `n` set while the node holds slot `n`.
    frame_slots_held_by_the_node: AtomicU32,
    pub main_frame_document_loaded: AtomicBool,
    /// Set once CEF has let the page go.
    pub page_closed: AtomicBool,
    connection_ended: AtomicBool,
    frames_painted: AtomicU64,
    frames_dropped_with_every_slot_held: AtomicU64,
    audio_channels: AtomicU32,
    page_data_requests_queued: AtomicU32,
    page_data_bytes_queued: AtomicU64,
    page_data_requests_dropped: AtomicU64,
    page_input_chunks_queued_by_input_port_name: Mutex<HashMap<String, u32>>,
    page_input_chunks_dropped: AtomicU64,
    next_painted_texture_number: AtomicU64,
    last_painted_texture_number_the_node_copied: Mutex<u64>,
    painted_texture_copied_by_the_node: Condvar,
    painted_texture_copies_not_answered_in_time: AtomicU64,
    /// The node's Mach port painted IOSurfaces cross on, `None` when the
    /// request named none or it was not found.
    #[cfg(target_os = "macos")]
    painted_iosurface_sender: Option<PaintedIOSurfaceSenderOfTheBrowserHost>,
}

/// One texture CEF painted the page into, as `on_accelerated_paint` hands it:
/// its descriptors are CEF's, live only for the length of that call.
#[cfg(target_os = "linux")]
pub struct PaintedTextureOfThePage {
    pub width: u32,
    pub height: u32,
    pub pixel_format: &'static str,
    pub plane_descriptors: Vec<RawFd>,
    pub plane_offsets_bytes: Vec<u64>,
    pub plane_strides_bytes: Vec<u64>,
    pub allocation_byte_size: u64,
    pub drm_format_modifier: u64,
}

/// One IOSurface CEF painted the page into, as `on_accelerated_paint` hands
/// it: CEF's, live only for the length of that call.
#[cfg(target_os = "macos")]
pub struct PaintedTextureOfThePage {
    pub width: u32,
    pub height: u32,
    pub pixel_format: &'static str,
    pub iosurface: *const std::ffi::c_void,
}

impl BrowserHostTab {
    pub fn new(
        tab_id: BrowserHostTabId,
        tab_request: BrowserHostTabRequest,
        writer_to_the_node: TabProtocolMessageWriter,
        frame_slots: BrowserHostTabFrameSlots,
    ) -> Self {
        Self {
            tab_id,
            writer_to_the_node: Mutex::new(writer_to_the_node),
            frame_slots,
            frame_slots_held_by_the_node: AtomicU32::new(0),
            main_frame_document_loaded: AtomicBool::new(false),
            page_closed: AtomicBool::new(false),
            connection_ended: AtomicBool::new(false),
            frames_painted: AtomicU64::new(0),
            frames_dropped_with_every_slot_held: AtomicU64::new(0),
            audio_channels: AtomicU32::new(0),
            page_data_requests_queued: AtomicU32::new(0),
            page_data_bytes_queued: AtomicU64::new(0),
            page_data_requests_dropped: AtomicU64::new(0),
            page_input_chunks_queued_by_input_port_name: Mutex::new(HashMap::new()),
            page_input_chunks_dropped: AtomicU64::new(0),
            next_painted_texture_number: AtomicU64::new(1),
            last_painted_texture_number_the_node_copied: Mutex::new(0),
            painted_texture_copied_by_the_node: Condvar::new(),
            painted_texture_copies_not_answered_in_time: AtomicU64::new(0),
            #[cfg(target_os = "macos")]
            painted_iosurface_sender: painted_iosurface_sender_of(&tab_request),
            tab_request,
        }
    }

    /// Whether CEF paints this tab into textures handed to the node.
    pub fn paints_into_shared_textures(&self) -> bool {
        #[cfg(target_os = "macos")]
        let painted_textures_can_cross = self.painted_iosurface_sender.is_some();
        #[cfg(not(target_os = "macos"))]
        let painted_textures_can_cross = true;
        self.tab_request.paint_into_shared_textures && painted_textures_can_cross
    }

    pub fn connection_ended(&self) -> bool {
        self.connection_ended.load(Ordering::Acquire)
    }

    /// Send `message` to the node; a failed send ends the connection, after
    /// which nothing more is sent.
    pub fn send_to_the_node(&self, message: &TabProtocolMessageFromBrowserHost) {
        self.send_to_the_node_carrying(message, &[]);
    }

    /// Send `message` with `attached_descriptors` riding on it, as
    /// [`Self::send_to_the_node`] does; `false` once it was not sent.
    fn send_to_the_node_carrying(
        &self,
        message: &TabProtocolMessageFromBrowserHost,
        attached_descriptors: &[RawFd],
    ) -> bool {
        if self.connection_ended() {
            return false;
        }
        let sent = self
            .writer_to_the_node
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send(message, attached_descriptors);
        match sent {
            Ok(()) => true,
            Err(TabProtocolSocketFailure::Encoding(refusal)) => {
                tracing::warn!("a message to the node was refused before sending: {refusal}");
                if !matches!(message, TabProtocolMessageFromBrowserHost::TabLog { .. }) {
                    self.log_to_the_node(
                        BrowserHostTabLogLevel::Warn,
                        format!("a message from the page was dropped: {refusal}"),
                    );
                }
                false
            }
            Err(_) => {
                self.end_the_connection();
                false
            }
        }
    }

    /// Tell the node its tab opened, handing it the frame slots; sent from the
    /// CEF thread before it paints the page, so it is the node's first message.
    pub fn answer_that_the_tab_opened(&self) {
        let opened_answer = TabProtocolMessageFromBrowserHost::TabOpened {
            tab_protocol_version: TAB_PROTOCOL_VERSION,
            browser_host_process_id: std::process::id(),
        };
        let sent = self
            .writer_to_the_node
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send(
                &opened_answer,
                &[self.frame_slots.shared_memory_descriptor()],
            );
        if sent.is_err() {
            self.end_the_connection();
        }
    }

    pub fn end_the_connection(&self) {
        if !self.connection_ended.swap(true, Ordering::AcqRel) {
            self.writer_to_the_node
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .shut_down();
            self.painted_texture_copied_by_the_node.notify_all();
        }
    }

    /// Count one more `data` message of `byte_count` bytes queued for the
    /// CEF thread; `false`, and the message dropped, when the queue is
    /// already full by count or by bytes.
    pub fn page_data_request_queued(&self, byte_count: usize) -> bool {
        let byte_count = byte_count as u64;
        let queued = self
            .page_data_requests_queued
            .fetch_add(1, Ordering::AcqRel);
        let bytes_queued = self
            .page_data_bytes_queued
            .fetch_add(byte_count, Ordering::AcqRel);
        if queued < MOST_PAGE_DATA_REQUESTS_QUEUED
            && (bytes_queued == 0 || bytes_queued + byte_count <= MOST_PAGE_DATA_BYTES_QUEUED)
        {
            return true;
        }
        self.page_data_request_dequeued(byte_count as usize);
        let dropped = self
            .page_data_requests_dropped
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if dropped == 1 || dropped.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
            self.log_to_the_node(
                BrowserHostTabLogLevel::Warn,
                format!(
                    "{dropped} data messages dropped with {MOST_PAGE_DATA_REQUESTS_QUEUED} \
                     messages or {MOST_PAGE_DATA_BYTES_QUEUED} bytes already waiting for the page"
                ),
            );
        }
        false
    }

    /// The CEF thread took one queued `data` message of `byte_count` bytes.
    pub fn page_data_request_dequeued(&self, byte_count: usize) {
        self.page_data_requests_queued
            .fetch_sub(1, Ordering::AcqRel);
        self.page_data_bytes_queued
            .fetch_sub(byte_count as u64, Ordering::AcqRel);
    }

    /// Count one more chunk of `input_port_name` queued for the CEF thread;
    /// `false`, and the chunk dropped, when the input is not one the tab was
    /// opened with or its queue is full.
    pub fn page_input_chunk_queued(&self, input_port_name: &str) -> bool {
        let most_queued = match self
            .tab_request
            .page_input_tracks
            .iter()
            .find(|track| track.input_port_name == input_port_name)
            .map(|track| track.kind)
        {
            Some(PageInputTrackKind::Video) => MOST_PAGE_INPUT_VIDEO_FRAMES_QUEUED_PER_INPUT,
            Some(PageInputTrackKind::Audio) => MOST_PAGE_INPUT_AUDIO_BLOCKS_QUEUED_PER_INPUT,
            None => {
                self.page_input_chunk_dropped(input_port_name);
                return false;
            }
        };
        let mut queued_by_input_port_name = self
            .page_input_chunks_queued_by_input_port_name
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let queued = queued_by_input_port_name
            .entry(input_port_name.to_string())
            .or_insert(0);
        if *queued < most_queued {
            *queued += 1;
            return true;
        }
        drop(queued_by_input_port_name);
        self.page_input_chunk_dropped(input_port_name);
        false
    }

    /// The CEF thread took one queued chunk of `input_port_name`.
    pub fn page_input_chunk_dequeued(&self, input_port_name: &str) {
        if let Some(queued) = self
            .page_input_chunks_queued_by_input_port_name
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(input_port_name)
        {
            *queued = queued.saturating_sub(1);
        }
    }

    /// Count one chunk of `input_port_name` that never reached the page.
    pub fn page_input_chunk_dropped(&self, input_port_name: &str) {
        let dropped = self
            .page_input_chunks_dropped
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if dropped == 1 || dropped.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
            self.log_to_the_node(
                BrowserHostTabLogLevel::Info,
                format!(
                    "{dropped} input chunks dropped before reaching the page, the latest of \
                     {input_port_name}"
                ),
            );
        }
    }

    /// Log `message` under this tab in the node's log.
    pub fn log_to_the_node(&self, level: BrowserHostTabLogLevel, message: impl Into<String>) {
        self.send_to_the_node(&TabProtocolMessageFromBrowserHost::TabLog {
            level,
            message: message.into(),
        });
    }

    /// Copy one painted frame of packed BGRA into a slot the node does not
    /// hold and hand it over; with every slot held, the frame is dropped.
    pub fn hand_a_painted_frame_to_the_node(
        &self,
        painted_bgra_rows: &[u8],
        width: u32,
        height: u32,
    ) {
        if self.connection_ended() {
            return;
        }
        if width != self.tab_request.width || height != self.tab_request.height {
            return;
        }
        let Some(slot) = self.take_a_free_frame_slot() else {
            self.frames_dropped_with_every_slot_held
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        // SAFETY: the slot was free, so the node does not read it, and only
        // the CEF thread paints.
        let Some(slot_bytes) = (unsafe { self.frame_slots.slot_for_writing(slot) }) else {
            return;
        };
        let copied_byte_count = slot_bytes.len().min(painted_bgra_rows.len());
        slot_bytes[..copied_byte_count].copy_from_slice(&painted_bgra_rows[..copied_byte_count]);
        self.send_to_the_node(&TabProtocolMessageFromBrowserHost::FramePainted {
            slot,
            width,
            height,
            row_stride_bytes: width * 4,
        });
        self.count_one_painted_frame();
    }

    fn count_one_painted_frame(&self) {
        let painted = self.frames_painted.fetch_add(1, Ordering::Relaxed) + 1;
        if painted.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
            self.log_to_the_node(
                BrowserHostTabLogLevel::Debug,
                format!(
                    "{painted} frames painted, {} dropped while the node held every slot, {} \
                     painted textures not copied in time",
                    self.frames_dropped_with_every_slot_held
                        .load(Ordering::Relaxed),
                    self.painted_texture_copies_not_answered_in_time
                        .load(Ordering::Relaxed)
                ),
            );
        }
    }

    /// Hand the texture the page painted into over to the node, and wait,
    /// within [`PAINTED_TEXTURE_COPY_BUDGET`], for the node to copy it before
    /// CEF may paint into it again.
    pub fn hand_a_painted_texture_to_the_node(&self, painted: PaintedTextureOfThePage) {
        if painted.width != self.tab_request.width || painted.height != self.tab_request.height {
            return;
        }
        let painted_texture_number = self
            .next_painted_texture_number
            .fetch_add(1, Ordering::Relaxed);
        if !self.send_the_painted_texture_to_the_node(painted, painted_texture_number) {
            return;
        }
        if !self.wait_for_the_node_to_copy(painted_texture_number, PAINTED_TEXTURE_COPY_BUDGET) {
            let not_answered = self
                .painted_texture_copies_not_answered_in_time
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if not_answered == 1 || not_answered.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
                self.log_to_the_node(
                    BrowserHostTabLogLevel::Warn,
                    format!(
                        "{not_answered} painted textures were not copied within \
                         {PAINTED_TEXTURE_COPY_BUDGET:?}, so the page painted on over them"
                    ),
                );
            }
        }
        self.count_one_painted_frame();
    }

    /// Send `painted` as `painted_texture_number`, its DMA-BUF planes riding
    /// on the message; `false` once it was not sent.
    #[cfg(target_os = "linux")]
    fn send_the_painted_texture_to_the_node(
        &self,
        painted: PaintedTextureOfThePage,
        painted_texture_number: u64,
    ) -> bool {
        self.send_to_the_node_carrying(
            &TabProtocolMessageFromBrowserHost::TexturePainted {
                painted_texture_number,
                width: painted.width,
                height: painted.height,
                pixel_format: painted.pixel_format.to_string(),
                allocation_byte_size: painted.allocation_byte_size,
                plane_offsets_bytes: painted.plane_offsets_bytes,
                plane_strides_bytes: painted.plane_strides_bytes,
                drm_format_modifier: painted.drm_format_modifier,
            },
            &painted.plane_descriptors,
        )
    }

    /// Send `painted`'s IOSurface across the node's Mach port, then the
    /// message naming it as `painted_texture_number`; `false` once either
    /// was not sent.
    #[cfg(target_os = "macos")]
    fn send_the_painted_texture_to_the_node(
        &self,
        painted: PaintedTextureOfThePage,
        painted_texture_number: u64,
    ) -> bool {
        let Some(painted_iosurface_sender) = &self.painted_iosurface_sender else {
            return false;
        };
        if self.connection_ended() {
            return false;
        }
        // SAFETY: CEF's IOSurface is live for the `on_accelerated_paint` call
        // this runs in.
        let crossed = unsafe {
            painted_iosurface_sender.send_painted_iosurface(
                painted.iosurface,
                painted_texture_number,
                PAINTED_TEXTURE_COPY_BUDGET,
            )
        };
        if let Err(failure) = crossed {
            tracing::warn!("painted texture {painted_texture_number} did not cross: {failure}");
            return false;
        }
        self.send_to_the_node_carrying(
            &TabProtocolMessageFromBrowserHost::TexturePainted {
                painted_texture_number,
                width: painted.width,
                height: painted.height,
                pixel_format: painted.pixel_format.to_string(),
                allocation_byte_size: 0,
                plane_offsets_bytes: Vec::new(),
                plane_strides_bytes: Vec::new(),
                drm_format_modifier: 0,
            },
            &[],
        )
    }

    /// The node copied painted texture `painted_texture_number`.
    pub fn painted_texture_copied_by_the_node(&self, painted_texture_number: u64) {
        let mut last_copied = self
            .last_painted_texture_number_the_node_copied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if painted_texture_number > *last_copied {
            *last_copied = painted_texture_number;
            self.painted_texture_copied_by_the_node.notify_all();
        }
    }

    /// Whether the node copied `painted_texture_number` within `budget`.
    fn wait_for_the_node_to_copy(&self, painted_texture_number: u64, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        let mut last_copied = self
            .last_painted_texture_number_the_node_copied
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if *last_copied >= painted_texture_number {
                return true;
            }
            let now = Instant::now();
            if now >= deadline || self.connection_ended() {
                return false;
            }
            last_copied = self
                .painted_texture_copied_by_the_node
                .wait_timeout(last_copied, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    fn take_a_free_frame_slot(&self) -> Option<u32> {
        let slot_count = self.frame_slots.slot_count();
        let mut held = self.frame_slots_held_by_the_node.load(Ordering::Acquire);
        loop {
            let slot = (0..slot_count).find(|slot| held & (1 << slot) == 0)?;
            match self.frame_slots_held_by_the_node.compare_exchange_weak(
                held,
                held | (1 << slot),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(slot),
                Err(now_held) => held = now_held,
            }
        }
    }

    /// The node gave `slot` back.
    pub fn frame_slot_released_by_the_node(&self, slot: u32) {
        if slot < self.frame_slots.slot_count() {
            self.frame_slots_held_by_the_node
                .fetch_and(!(1 << slot), Ordering::AcqRel);
        }
    }

    pub fn audio_stream_started(&self, sample_rate: u32, channels: u32) {
        self.audio_channels.store(channels, Ordering::Release);
        self.send_to_the_node(&TabProtocolMessageFromBrowserHost::AudioStreamStarted {
            sample_rate,
            channels,
        });
    }

    pub fn audio_stream_channels(&self) -> u32 {
        self.audio_channels.load(Ordering::Acquire)
    }

    /// Hand one planar packet over, interleaved.
    pub fn hand_an_audio_packet_to_the_node(&self, channel_planes: &[&[f32]], sample_count: usize) {
        let mut interleaved_samples_le_f32 =
            Vec::with_capacity(channel_planes.len() * sample_count * 4);
        for sample_index in 0..sample_count {
            for plane in channel_planes {
                let sample = plane.get(sample_index).copied().unwrap_or(0.0);
                interleaved_samples_le_f32.extend_from_slice(&sample.to_le_bytes());
            }
        }
        self.send_to_the_node(&TabProtocolMessageFromBrowserHost::AudioPacket {
            sample_count: sample_count as u32,
            interleaved_samples_le_f32,
        });
    }

    pub fn audio_stream_stopped(&self) {
        self.audio_channels.store(0, Ordering::Release);
        self.send_to_the_node(&TabProtocolMessageFromBrowserHost::AudioStreamStopped);
    }
}

/// The node's Mach port `tab_request` names, looked up once for the tab.
#[cfg(target_os = "macos")]
fn painted_iosurface_sender_of(
    tab_request: &BrowserHostTabRequest,
) -> Option<PaintedIOSurfaceSenderOfTheBrowserHost> {
    let service_name = tab_request.painted_iosurface_mach_service_name.as_deref()?;
    PaintedIOSurfaceSenderOfTheBrowserHost::look_up(service_name)
        .inspect_err(|failure| {
            tracing::warn!(
                "the node's Mach port {service_name} was not found, so the page paints into \
                 frame slots: {failure}"
            );
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use tatolab_browser_tab_protocol::{PageInputTrack, TabProtocolMessageReader};

    fn tab_with_slots(slot_count: u32) -> (BrowserHostTab, TabProtocolMessageReader) {
        let (host_end, node_end) = UnixStream::pair().unwrap();
        let tab = BrowserHostTab::new(
            1,
            BrowserHostTabRequest {
                tenant_stream_id: "stream".to_string(),
                url: "about:blank".to_string(),
                width: 2,
                height: 1,
                frame_rate: 30,
                transparent_background: true,
                audio_sample_rate: 48_000,
                frame_slot_count: slot_count,
                paint_into_shared_textures: false,
                painted_iosurface_mach_service_name: None,
                page_input_tracks: vec![PageInputTrack {
                    input_port_name: "video_in_0".to_string(),
                    kind: PageInputTrackKind::Video,
                }],
            },
            TabProtocolMessageWriter::over(host_end),
            BrowserHostTabFrameSlots::create(slot_count, 2, 1).unwrap(),
        );
        (tab, TabProtocolMessageReader::over(node_end))
    }

    #[test]
    fn a_video_input_queues_two_frames_and_an_input_the_tab_was_not_opened_with_none() {
        let (tab, _reader) = tab_with_slots(1);

        assert!(tab.page_input_chunk_queued("video_in_0"));
        assert!(tab.page_input_chunk_queued("video_in_0"));
        assert!(!tab.page_input_chunk_queued("video_in_0"));
        tab.page_input_chunk_dequeued("video_in_0");
        assert!(tab.page_input_chunk_queued("video_in_0"));
        assert!(!tab.page_input_chunk_queued("audio_in_0"));
        assert_eq!(tab.page_input_chunks_dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_frame_goes_to_a_free_slot_and_is_dropped_once_every_slot_is_held() {
        let (tab, mut node) = tab_with_slots(2);
        let painted = [1u8; 8];
        tab.hand_a_painted_frame_to_the_node(&painted, 2, 1);
        tab.hand_a_painted_frame_to_the_node(&painted, 2, 1);
        tab.hand_a_painted_frame_to_the_node(&painted, 2, 1);
        let slots: Vec<u32> = (0..2)
            .map(|_| match node.receive().unwrap() {
                TabProtocolMessageFromBrowserHost::FramePainted { slot, .. } => slot,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(slots, [0, 1]);
        assert_eq!(
            tab.frames_dropped_with_every_slot_held
                .load(Ordering::Relaxed),
            1
        );
        tab.frame_slot_released_by_the_node(0);
        tab.hand_a_painted_frame_to_the_node(&painted, 2, 1);
        assert!(matches!(
            node.receive().unwrap(),
            TabProtocolMessageFromBrowserHost::FramePainted { slot: 0, .. }
        ));
    }

    #[cfg(target_os = "linux")]
    fn a_painted_texture_of(tab: &BrowserHostTab, plane_descriptor: RawFd) {
        tab.hand_a_painted_texture_to_the_node(PaintedTextureOfThePage {
            width: 2,
            height: 1,
            pixel_format: "bgra32",
            plane_descriptors: vec![plane_descriptor],
            plane_offsets_bytes: vec![0],
            plane_strides_bytes: vec![8],
            allocation_byte_size: 8,
            drm_format_modifier: 0,
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_painted_texture_is_handed_over_with_its_plane() {
        use std::os::fd::AsRawFd;
        let (tab, mut node) = tab_with_slots(1);
        let plane = std::fs::File::open("/dev/null").unwrap();
        tab.painted_texture_copied_by_the_node(1);

        a_painted_texture_of(&tab, plane.as_raw_fd());

        let painted = node.receive::<TabProtocolMessageFromBrowserHost>().unwrap();
        assert!(
            matches!(
                painted,
                TabProtocolMessageFromBrowserHost::TexturePainted {
                    painted_texture_number: 1,
                    ..
                }
            ),
            "{painted:?}"
        );
        node.take_attached_descriptor().unwrap();
        assert_eq!(
            tab.painted_texture_copies_not_answered_in_time
                .load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn the_wait_for_a_copy_ends_once_the_node_copied_it() {
        let tab = std::sync::Arc::new(tab_with_slots(1).0);
        let copying_node = std::thread::spawn({
            let tab = std::sync::Arc::clone(&tab);
            move || {
                std::thread::sleep(Duration::from_millis(30));
                tab.painted_texture_copied_by_the_node(1);
            }
        });

        let started = Instant::now();
        assert!(tab.wait_for_the_node_to_copy(1, Duration::from_secs(30)));

        assert!(started.elapsed() >= Duration::from_millis(30));
        copying_node.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_painted_texture_the_node_does_not_copy_in_time_is_counted() {
        use std::os::fd::AsRawFd;
        let (tab, mut node) = tab_with_slots(1);
        let plane = std::fs::File::open("/dev/null").unwrap();

        a_painted_texture_of(&tab, plane.as_raw_fd());

        assert!(matches!(
            node.receive().unwrap(),
            TabProtocolMessageFromBrowserHost::TexturePainted { .. }
        ));
        assert_eq!(
            tab.painted_texture_copies_not_answered_in_time
                .load(Ordering::Relaxed),
            1
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_tab_whose_mach_port_is_not_found_paints_into_frame_slots() {
        let (host_end, _node_end) = UnixStream::pair().unwrap();
        let tab = BrowserHostTab::new(
            1,
            BrowserHostTabRequest {
                tenant_stream_id: "stream".to_string(),
                url: "about:blank".to_string(),
                width: 2,
                height: 1,
                frame_rate: 30,
                transparent_background: true,
                audio_sample_rate: 48_000,
                frame_slot_count: 1,
                paint_into_shared_textures: true,
                painted_iosurface_mach_service_name: Some(format!(
                    "com.tatolab.browser.paint.nobody.{}",
                    std::process::id()
                )),
            },
            TabProtocolMessageWriter::over(host_end),
            BrowserHostTabFrameSlots::create(1, 2, 1).unwrap(),
        );
        assert!(!tab.paints_into_shared_textures());
    }

    #[test]
    fn a_frame_of_another_extent_is_not_handed_over() {
        let (tab, _node) = tab_with_slots(2);
        tab.hand_a_painted_frame_to_the_node(&[0u8; 16], 4, 1);
        assert_eq!(tab.frames_painted.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn audio_is_handed_over_interleaved() {
        let (tab, mut node) = tab_with_slots(1);
        tab.hand_an_audio_packet_to_the_node(&[&[1.0, 2.0], &[-1.0, -2.0]], 2);
        match node.receive().unwrap() {
            TabProtocolMessageFromBrowserHost::AudioPacket {
                sample_count,
                interleaved_samples_le_f32,
            } => {
                assert_eq!(sample_count, 2);
                let samples: Vec<f32> = interleaved_samples_le_f32
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(f32::from_le_bytes)
                    .collect();
                assert_eq!(samples, [1.0, -1.0, 2.0, -2.0]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn data_past_the_queue_bound_is_dropped_and_the_node_told_once() {
        let (tab, mut node) = tab_with_slots(1);
        for _ in 0..MOST_PAGE_DATA_REQUESTS_QUEUED {
            assert!(tab.page_data_request_queued(10));
        }
        assert!(!tab.page_data_request_queued(10));
        assert!(matches!(
            node.receive().unwrap(),
            TabProtocolMessageFromBrowserHost::TabLog {
                level: BrowserHostTabLogLevel::Warn,
                ..
            }
        ));
        tab.page_data_request_dequeued(10);
        assert!(tab.page_data_request_queued(10));
    }

    #[test]
    fn data_past_the_byte_bound_is_dropped_but_one_large_message_alone_is_queued() {
        let (tab, _node) = tab_with_slots(1);
        let most_bytes = MOST_PAGE_DATA_BYTES_QUEUED as usize;
        assert!(tab.page_data_request_queued(most_bytes + 1));
        assert!(!tab.page_data_request_queued(1));
        tab.page_data_request_dequeued(most_bytes + 1);
        assert!(tab.page_data_request_queued(most_bytes / 2));
        assert!(tab.page_data_request_queued(most_bytes / 2));
        assert!(!tab.page_data_request_queued(1));
    }

    #[test]
    fn a_node_that_went_away_ends_the_connection_and_is_sent_nothing_more() {
        let (tab, node) = tab_with_slots(1);
        drop(node);
        for _ in 0..4 {
            tab.log_to_the_node(BrowserHostTabLogLevel::Info, "x".repeat(64 * 1024));
        }
        assert!(tab.connection_ended());
    }
}
