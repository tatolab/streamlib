// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One tab: the page a node asked for, and everything CEF's handlers send that
//! node over its connection.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use tatolab_browser_tab_protocol::{
    BrowserHostTabFrameSlots, BrowserHostTabLogLevel, BrowserHostTabRequest, TAB_PROTOCOL_VERSION,
    TabProtocolMessageFromBrowserHost, TabProtocolMessageWriter,
};

/// How many painted frames go by between two reports of what a tab painted.
const PAINTS_BETWEEN_REPORTS: u64 = 600;

/// The most `data` messages a tab queues for the CEF thread; past it, a
/// message is dropped and the node told.
const MOST_PAGE_DATA_REQUESTS_QUEUED: u32 = 256;
/// How many bytes of `data` one tab may queue for the CEF thread.
const MOST_PAGE_DATA_BYTES_QUEUED: u64 = 64 * 1024 * 1024;

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
            tab_request,
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
        }
    }

    pub fn connection_ended(&self) -> bool {
        self.connection_ended.load(Ordering::Acquire)
    }

    /// Send `message` to the node; a failed send ends the connection, after
    /// which nothing more is sent.
    pub fn send_to_the_node(&self, message: &TabProtocolMessageFromBrowserHost) {
        if self.connection_ended() {
            return;
        }
        let sent = self
            .writer_to_the_node
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send(message, &[]);
        if sent.is_err() {
            self.end_the_connection();
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
        let painted = self.frames_painted.fetch_add(1, Ordering::Relaxed) + 1;
        if painted.is_multiple_of(PAINTS_BETWEEN_REPORTS) {
            self.log_to_the_node(
                BrowserHostTabLogLevel::Debug,
                format!(
                    "{painted} frames painted, {} dropped while the node held every slot",
                    self.frames_dropped_with_every_slot_held
                        .load(Ordering::Relaxed)
                ),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use tatolab_browser_tab_protocol::TabProtocolMessageReader;

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
            },
            TabProtocolMessageWriter::over(host_end),
            BrowserHostTabFrameSlots::create(slot_count, 2, 1).unwrap(),
        );
        (tab, TabProtocolMessageReader::over(node_end))
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
