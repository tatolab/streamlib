// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The operations on the streams a runtime loads and on what they log: a project's log records
//! read, a viewer of an output port opened and closed, and a stream's clock rescaled.

use std::sync::Arc;

use streamlib::sdk::runtime::OutputPortViewerHeldOpen;
use streamlib_control_contract::{
    CloseViewerRequest, LogsRequest, OpenViewerRequest, SetStreamClockScaleRequest,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CloseViewerToolResult, LogsToolResult, OpenViewerToolResult, SetStreamClockScaleToolResult,
};
use streamlib_runtime_client_contract::project_log_records::{
    ProjectLogRecordsPage, project_log_records_between,
};

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    refuse_a_relative_project_directory, the_refusal_of_a_reach_for_a_node,
};

/// How many log records one `logs` call returns when the caller names no
/// `count`.
const DEFAULT_LOGS_RECORD_COUNT: u32 = 256;

/// The most log records one `logs` call returns: as many as a stream's log
/// route holds in memory.
const MAX_LOGS_RECORD_COUNT: u32 = 4096;

/// The viewers one connection that stays open opened, by id, each holding its port's channel
/// open until it is closed or the connection ends.
#[derive(Default)]
pub(crate) struct ViewersOneConnectionHoldsOpen {
    viewers_by_id: parking_lot::Mutex<std::collections::BTreeMap<String, OutputPortViewerHeldOpen>>,
}

impl std::fmt::Debug for ViewersOneConnectionHoldsOpen {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_set()
            .entries(self.viewers_by_id.lock().keys())
            .finish()
    }
}

impl Drop for ViewersOneConnectionHoldsOpen {
    /// Releasing a port's channel may wait on its helper process, so the
    /// viewers a connection left open are released off the async workers.
    fn drop(&mut self) {
        let viewers_left_open = std::mem::take(self.viewers_by_id.get_mut());
        if viewers_left_open.is_empty() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(async_runtime) => {
                drop(async_runtime.spawn_blocking(move || drop(viewers_left_open)))
            }
            Err(_) => drop(viewers_left_open),
        }
    }
}

impl ControlRequestOrigin {
    fn the_viewers_its_connection_holds_open(
        &self,
        method_name: &str,
    ) -> Result<&Arc<ViewersOneConnectionHoldsOpen>, ControlOperationRefusal> {
        self.viewers_held_open.as_ref().ok_or_else(|| {
            ControlOperationRefusal::Failed(format!(
                "{method_name} failed: a viewer stays open for the connection that opened it, \
                 and this request's connection ends with its answer; open it over a connection \
                 that stays open, `/mcp/stdio` or `/control/ws`"
            ))
        })
    }
}

impl ControlOperationDispatcher {
    pub(crate) async fn logs(
        &self,
        LogsRequest {
            project_directory,
            stream,
            action,
            after,
            until,
            count,
        }: LogsRequest,
    ) -> Result<LogsToolResult, ControlOperationRefusal> {
        refuse_a_relative_project_directory(&project_directory)?;
        let max_count = count
            .unwrap_or(DEFAULT_LOGS_RECORD_COUNT)
            .clamp(1, MAX_LOGS_RECORD_COUNT) as usize;
        let read_project_directory = project_directory.clone();
        let ProjectLogRecordsPage {
            records,
            next_after,
            records_no_longer_held,
        } = tokio::task::spawn_blocking(move || {
            project_log_records_between(
                &read_project_directory,
                stream.as_deref(),
                action.as_deref(),
                after.as_deref(),
                until.as_deref(),
                max_count,
            )
        })
        .await
        .map_err(|join_failure| format!("logs failed: the read did not finish: {join_failure}"))?
        .map_err(|not_read| format!("logs failed: {not_read}"))?;
        Ok(LogsToolResult {
            project_directory,
            records,
            next_after,
            records_no_longer_held,
        })
    }

    pub(crate) async fn open_viewer(
        &self,
        OpenViewerRequest {
            stream,
            node_id,
            port,
            viewer_process_id,
        }: OpenViewerRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<OpenViewerToolResult, ControlOperationRefusal> {
        let viewers_held_open =
            request_origin.the_viewers_its_connection_holds_open("open_viewer")?;
        let viewer = format!("{stream}/{node_id}/{port}");
        if viewers_held_open.viewers_by_id.lock().contains_key(&viewer) {
            return Err(format!(
                "open_viewer failed: this connection already holds viewer `{viewer}` open"
            )
            .into());
        }
        let stream_operations = self.the_stream_a_call_names("open_viewer", &stream)?;
        let viewer_held_open = stream_operations
            .open_a_viewer_of_an_output_port(node_id, port, viewer_process_id)
            .await;
        let viewer_held_open = match viewer_held_open {
            Ok(viewer_held_open) => viewer_held_open,
            Err(refusal) => {
                return Err(the_refusal_of_a_reach_for_a_node("open_viewer", refusal));
            }
        };
        let hand_off = viewer_held_open.hand_off().clone();
        let viewer = format!("{}/{}/{}", hand_off.stream, hand_off.node_id, hand_off.port);
        let viewer_already_open = {
            let mut viewers_by_id = viewers_held_open.viewers_by_id.lock();
            match viewers_by_id.entry(viewer.clone()) {
                std::collections::btree_map::Entry::Occupied(_) => Some(viewer_held_open),
                std::collections::btree_map::Entry::Vacant(vacant) => {
                    vacant.insert(viewer_held_open);
                    None
                }
            }
        };
        if let Some(viewer_opened_twice) = viewer_already_open {
            let _ = tokio::task::spawn_blocking(move || drop(viewer_opened_twice)).await;
            return Err(format!(
                "open_viewer failed: this connection already holds viewer `{viewer}` open"
            )
            .into());
        }
        Ok(OpenViewerToolResult {
            viewer,
            stream: hand_off.stream,
            node_id: hand_off.node_id,
            node_name: hand_off.node_name,
            port: hand_off.port,
            iceoryx2_domain_root: hand_off.iceoryx2_domain_root.display().to_string(),
            channel: hand_off.channel_service_name,
            wake_service: hand_off.wake_service_name,
            surface_share_address: hand_off.surface_share_address,
        })
    }

    pub(crate) async fn close_viewer(
        &self,
        CloseViewerRequest { viewer }: CloseViewerRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<CloseViewerToolResult, ControlOperationRefusal> {
        let closed = request_origin
            .the_viewers_its_connection_holds_open("close_viewer")?
            .viewers_by_id
            .lock()
            .remove(&viewer);
        let Some(closed) = closed else {
            return Err(format!(
                "close_viewer failed: this connection holds no viewer `{viewer}` open"
            )
            .into());
        };
        tokio::task::spawn_blocking(move || drop(closed))
            .await
            .map_err(|join_failure| format!("close_viewer failed: {join_failure}"))?;
        Ok(CloseViewerToolResult { viewer })
    }

    pub(crate) async fn set_stream_clock_scale(
        &self,
        SetStreamClockScaleRequest { stream, scale }: SetStreamClockScaleRequest,
    ) -> Result<SetStreamClockScaleToolResult, ControlOperationRefusal> {
        let stream_for_the_result = stream.clone();
        let rescaled = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.set_stream_clock_scale(&stream, scale)
            })
            .await?;
        Ok(SetStreamClockScaleToolResult {
            stream: stream_for_the_result,
            scale: rescaled.scale,
        })
    }
}
