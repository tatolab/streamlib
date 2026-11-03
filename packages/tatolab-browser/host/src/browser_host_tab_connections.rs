// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The host's listening socket and one reader thread per tab connection: the
//! node's `OpenTab`, then its `data`, released frame slots and close.

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tatolab_browser_tab_protocol::{
    BrowserHostTabFrameSlots, TAB_PROTOCOL_VERSION, TabProtocolMessageFromBrowserHost,
    TabProtocolMessageFromNode, TabProtocolMessageReader, TabProtocolMessageWriter,
    TabProtocolSocketFailure, refusal_of_another_tab_protocol_version,
};

use crate::browser_host_cef_thread::{CefThreadRequest, CefThreadRequestSender};
use crate::browser_host_tab::{BrowserHostTab, BrowserHostTabId};

/// Accept tab connections on `listener` for the life of the host, each served
/// on a thread of its own.
pub fn accept_tab_connections(listener: UnixListener, cef_thread_requests: CefThreadRequestSender) {
    static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);
    for connection in listener.incoming() {
        let connection = match connection {
            Ok(connection) => connection,
            Err(failure) => {
                tracing::warn!("a tab connection was not accepted: {failure}");
                continue;
            }
        };
        let tab_id = NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed);
        let cef_thread_requests = cef_thread_requests.clone();
        let served = std::thread::Builder::new()
            .name(format!("tab-{tab_id}"))
            .spawn(move || serve_one_tab_connection(tab_id, connection, &cef_thread_requests));
        if let Err(failure) = served {
            tracing::warn!("tab {tab_id}'s connection could not be served: {failure}");
        }
    }
}

fn serve_one_tab_connection(
    tab_id: BrowserHostTabId,
    connection: UnixStream,
    cef_thread_requests: &CefThreadRequestSender,
) {
    let (reader, writer) = match connection.try_clone() {
        Ok(writing_half) => (
            TabProtocolMessageReader::over(connection),
            TabProtocolMessageWriter::over(writing_half),
        ),
        Err(failure) => {
            tracing::warn!("tab {tab_id}'s connection could not be split: {failure}");
            return;
        }
    };
    let Some((tab, reader)) = open_the_requested_tab(tab_id, reader, writer, cef_thread_requests)
    else {
        return;
    };
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
    let tab_request = match reader.receive::<TabProtocolMessageFromNode>() {
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
    let frame_slots_descriptor = frame_slots.shared_memory_descriptor();
    let opened_answer = TabProtocolMessageFromBrowserHost::TabOpened {
        tab_protocol_version: TAB_PROTOCOL_VERSION,
        browser_host_process_id: std::process::id(),
    };
    if writer
        .send(&opened_answer, &[frame_slots_descriptor])
        .is_err()
    {
        return None;
    }
    let tab = Arc::new(BrowserHostTab::new(
        tab_id,
        tab_request,
        writer,
        frame_slots,
    ));
    let (opened, open_outcome) = std::sync::mpsc::sync_channel(1);
    if !cef_thread_requests.send(CefThreadRequest::OpenTab {
        tab: Arc::clone(&tab),
        opened,
    }) {
        tab.end_the_connection();
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
            tab.log_to_the_node(
                tatolab_browser_tab_protocol::BrowserHostTabLogLevel::Error,
                reason,
            );
            tab.end_the_connection();
            None
        }
        Err(_) => {
            tab.end_the_connection();
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
        match reader.receive::<TabProtocolMessageFromNode>() {
            Ok(TabProtocolMessageFromNode::DeliverPageData { detail_json }) => {
                if !cef_thread_requests.send(CefThreadRequest::DeliverPageData {
                    tab_id: tab.tab_id,
                    detail_json,
                }) {
                    return;
                }
            }
            Ok(TabProtocolMessageFromNode::ReleaseFrameSlot { slot }) => {
                tab.frame_slot_released_by_the_node(slot);
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
