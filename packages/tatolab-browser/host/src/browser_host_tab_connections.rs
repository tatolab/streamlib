// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The host's listening socket and one reader thread per tab connection: the
//! node's `OpenTab`, then its `data`, released frame slots and close.

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tatolab_browser_tab_protocol::{
    BrowserHostTabFrameSlots, TAB_PROTOCOL_VERSION, TabProtocolMessageFromBrowserHost,
    TabProtocolMessageFromNode, TabProtocolMessageReader, TabProtocolMessageWriter,
    TabProtocolSocketFailure, refusal_of_another_tab_protocol_version,
};

use tatolab_browser_page_bridge::PageInputChunkKind;

use crate::browser_host_cef_thread::{
    CefThreadRequest, CefThreadRequestSender, PageInputChunkForTheCefThread,
};
use crate::browser_host_tab::{BrowserHostTab, BrowserHostTabId};

/// How long a new connection has to send its `OpenTab`.
const OPEN_TAB_REQUEST_BUDGET: Duration = Duration::from_secs(10);
/// The most connections waiting to open a tab at once; one past it is
/// closed unserved, so connections that never send `OpenTab` cannot hold
/// every thread and descriptor the host has.
const MOST_CONNECTIONS_OPENING_A_TAB: usize = 64;
/// How long a send to a node may block before its connection is ended, so a
/// node that stops reading never stalls the CEF thread every tab shares.
const SEND_TO_THE_NODE_BUDGET: Duration = Duration::from_secs(1);

/// Accept tab connections on `listener` for the life of the host, each served
/// on a thread of its own.
pub fn accept_tab_connections(listener: UnixListener, cef_thread_requests: CefThreadRequestSender) {
    static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);
    let connections_opening_a_tab = Arc::new(AtomicUsize::new(0));
    for connection in listener.incoming() {
        let connection = match connection {
            Ok(connection) => connection,
            Err(failure) => {
                tracing::warn!("a tab connection was not accepted: {failure}");
                continue;
            }
        };
        let Some(opening_a_tab) = ConnectionOpeningATab::counted_in(&connections_opening_a_tab)
        else {
            tracing::warn!(
                "a tab connection was closed unserved: {MOST_CONNECTIONS_OPENING_A_TAB} \
                 connections are already opening a tab"
            );
            continue;
        };
        let tab_id = NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed);
        let cef_thread_requests = cef_thread_requests.clone();
        let served = std::thread::Builder::new()
            .name(format!("tab-{tab_id}"))
            .spawn(move || {
                serve_one_tab_connection(tab_id, connection, opening_a_tab, &cef_thread_requests)
            });
        if let Err(failure) = served {
            tracing::warn!("tab {tab_id}'s connection could not be served: {failure}");
        }
    }
}

/// One connection counted among those opening a tab until it is dropped.
struct ConnectionOpeningATab {
    connections_opening_a_tab: Arc<AtomicUsize>,
}

impl ConnectionOpeningATab {
    fn counted_in(connections_opening_a_tab: &Arc<AtomicUsize>) -> Option<Self> {
        let mut opening = connections_opening_a_tab.load(Ordering::Acquire);
        loop {
            if opening >= MOST_CONNECTIONS_OPENING_A_TAB {
                return None;
            }
            match connections_opening_a_tab.compare_exchange_weak(
                opening,
                opening + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => opening = current,
            }
        }
        Some(Self {
            connections_opening_a_tab: Arc::clone(connections_opening_a_tab),
        })
    }
}

impl Drop for ConnectionOpeningATab {
    fn drop(&mut self) {
        self.connections_opening_a_tab
            .fetch_sub(1, Ordering::AcqRel);
    }
}

fn serve_one_tab_connection(
    tab_id: BrowserHostTabId,
    connection: UnixStream,
    opening_a_tab: ConnectionOpeningATab,
    cef_thread_requests: &CefThreadRequestSender,
) {
    let split = connection
        .try_clone()
        .and_then(|reading_half| Ok((reading_half, connection.try_clone()?)));
    let (reader, writer) = match split {
        Ok((reading_half, writing_half)) => (
            TabProtocolMessageReader::over(reading_half),
            TabProtocolMessageWriter::over(writing_half).sending_within(SEND_TO_THE_NODE_BUDGET),
        ),
        Err(failure) => {
            tracing::warn!("tab {tab_id}'s connection could not be split: {failure}");
            return;
        }
    };
    let opened = open_the_requested_tab(tab_id, reader, writer, cef_thread_requests);
    drop(opening_a_tab);
    let Some((tab, reader)) = opened else {
        return;
    };
    if let Err(failure) = connection.set_read_timeout(None) {
        tab.log_to_the_node(
            tatolab_browser_tab_protocol::BrowserHostTabLogLevel::Error,
            format!("the tab's connection keeps its open timeout: {failure}"),
        );
        tab.end_the_connection();
        cef_thread_requests.send(CefThreadRequest::CloseTab { tab_id });
        return;
    }
    serve_the_open_tab(&tab, reader, cef_thread_requests);
    tab.end_the_connection();
    cef_thread_requests.send(CefThreadRequest::CloseTab { tab_id });
}

/// Read the node's `OpenTab`, open its page, and answer; `None` once the tab
/// was refused or the node went away.
fn open_the_requested_tab(
    tab_id: BrowserHostTabId,
    mut reader: TabProtocolMessageReader,
    mut writer: TabProtocolMessageWriter,
    cef_thread_requests: &CefThreadRequestSender,
) -> Option<(Arc<BrowserHostTab>, TabProtocolMessageReader)> {
    let refuse = |writer: &mut TabProtocolMessageWriter, reason: String| {
        tracing::warn!("tab {tab_id} was refused: {reason}");
        let _ = writer.send(
            &TabProtocolMessageFromBrowserHost::TabRefused { reason },
            &[],
        );
    };
    let open_tab_request =
        reader.receive_within::<TabProtocolMessageFromNode>(OPEN_TAB_REQUEST_BUDGET);
    reader.discard_attached_descriptors();
    let tab_request = match open_tab_request {
        Ok(TabProtocolMessageFromNode::OpenTab {
            tab_protocol_version,
            tab_request,
        }) => {
            if tab_protocol_version != TAB_PROTOCOL_VERSION {
                refuse(
                    &mut writer,
                    refusal_of_another_tab_protocol_version(tab_protocol_version),
                );
                return None;
            }
            tab_request
        }
        Ok(other) => {
            refuse(
                &mut writer,
                format!("a tab connection opens with open_tab, not {other:?}"),
            );
            return None;
        }
        Err(TabProtocolSocketFailure::Ended) => return None,
        Err(failure) => {
            refuse(
                &mut writer,
                format!("the open_tab did not arrive whole: {failure}"),
            );
            return None;
        }
    };
    if let Err(reason) = tab_request.refuse_what_cef_cannot_render() {
        refuse(&mut writer, reason);
        return None;
    }
    let frame_slots = match BrowserHostTabFrameSlots::create(
        tab_request.frame_slot_count,
        tab_request.width,
        tab_request.height,
    ) {
        Ok(frame_slots) => frame_slots,
        Err(failure) => {
            refuse(
                &mut writer,
                format!("the tab's frame slots were not made: {failure}"),
            );
            return None;
        }
    };
    let tab = Arc::new(BrowserHostTab::new(
        tab_id,
        tab_request,
        writer,
        frame_slots,
    ));
    let (opened, open_outcome) = std::sync::mpsc::sync_channel(1);
    let refuse_the_open_tab = |reason: String| {
        tracing::warn!("tab {tab_id} was refused: {reason}");
        tab.send_to_the_node(&TabProtocolMessageFromBrowserHost::TabRefused { reason });
        tab.end_the_connection();
    };
    if !cef_thread_requests.send(CefThreadRequest::OpenTab {
        tab: Arc::clone(&tab),
        opened,
    }) {
        refuse_the_open_tab("the browser host is stopping".to_string());
        return None;
    }
    match open_outcome.recv() {
        Ok(Ok(())) => {
            tab.log_to_the_node(
                tatolab_browser_tab_protocol::BrowserHostTabLogLevel::Info,
                format!(
                    "tab {tab_id} of the browser host {} opened {} at {}x{} in stream {}'s profile",
                    std::process::id(),
                    tab.tab_request.url,
                    tab.tab_request.width,
                    tab.tab_request.height,
                    tab.tab_request.tenant_stream_id,
                ),
            );
            Some((tab, reader))
        }
        Ok(Err(reason)) => {
            refuse_the_open_tab(reason);
            None
        }
        Err(_) => {
            refuse_the_open_tab("the browser host is stopping".to_string());
            None
        }
    }
}

fn serve_the_open_tab(
    tab: &Arc<BrowserHostTab>,
    mut reader: TabProtocolMessageReader,
    cef_thread_requests: &CefThreadRequestSender,
) {
    loop {
        let received = reader.receive::<TabProtocolMessageFromNode>();
        reader.discard_attached_descriptors();
        match received {
            Ok(TabProtocolMessageFromNode::DeliverPageData { detail_json }) => {
                if !tab.page_data_request_queued(detail_json.len()) {
                    continue;
                }
                if !cef_thread_requests.send(CefThreadRequest::DeliverPageData {
                    tab_id: tab.tab_id,
                    detail_json,
                }) {
                    return;
                }
            }
            Ok(TabProtocolMessageFromNode::DeliverPageInputVideoFrame {
                input_port_name,
                width,
                height,
                timestamp_ns,
                rgba_pixels,
            }) => {
                if !tab.page_input_chunk_queued(&input_port_name) {
                    continue;
                }
                if !cef_thread_requests.send(CefThreadRequest::DeliverPageInputChunk {
                    tab_id: tab.tab_id,
                    chunk: PageInputChunkForTheCefThread {
                        kind: PageInputChunkKind::VideoFrameRgba,
                        input_port_name,
                        width_or_sample_rate: width,
                        height_or_channel_count: height,
                        timestamp_ns,
                        payload: rgba_pixels,
                    },
                }) {
                    return;
                }
            }
            Ok(TabProtocolMessageFromNode::DeliverPageInputAudioBlock {
                input_port_name,
                sample_rate,
                channel_count,
                first_sample_timestamp_ns,
                interleaved_samples_le_f32,
            }) => {
                if !tab.page_input_chunk_queued(&input_port_name) {
                    continue;
                }
                if !cef_thread_requests.send(CefThreadRequest::DeliverPageInputChunk {
                    tab_id: tab.tab_id,
                    chunk: PageInputChunkForTheCefThread {
                        kind: PageInputChunkKind::AudioBlockF32,
                        input_port_name,
                        width_or_sample_rate: sample_rate,
                        height_or_channel_count: channel_count,
                        timestamp_ns: first_sample_timestamp_ns,
                        payload: interleaved_samples_le_f32,
                    },
                }) {
                    return;
                }
            }
            Ok(TabProtocolMessageFromNode::ReleaseFrameSlot { slot }) => {
                tab.frame_slot_released_by_the_node(slot);
            }
            Ok(TabProtocolMessageFromNode::PaintedTextureCopied {
                painted_texture_number,
            }) => {
                tab.painted_texture_copied_by_the_node(painted_texture_number);
            }
            Ok(TabProtocolMessageFromNode::CloseTab) | Err(TabProtocolSocketFailure::Ended) => {
                return;
            }
            Ok(TabProtocolMessageFromNode::OpenTab { .. }) => {
                tracing::warn!(
                    "tab {} asked to open a second page on one connection; it is closed",
                    tab.tab_id
                );
                return;
            }
            Err(failure) => {
                tracing::warn!("tab {}'s connection failed: {failure}", tab.tab_id);
                return;
            }
        }
    }
}
