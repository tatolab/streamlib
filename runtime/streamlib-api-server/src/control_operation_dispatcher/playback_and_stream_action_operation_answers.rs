// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_control_contract::{OneProjectByItsDirectoryRequest, OneStreamActionRequest};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CancelStreamActionToolResult, PausePlaybackToolResult, ProjectStreamActionQueuedToolResult,
    ResumePlaybackToolResult,
};

use crate::mcp::{describe_stream_action_tool_result_of, stream_action_state_on_the_wire};

use super::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    refuse_a_relative_project_directory,
};

impl ControlOperationDispatcher {
    pub(super) async fn answer_start_playback(
        &self,
        start_playback_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = start_playback_request;
        self.a_project_stream_action_queued_by(
            project_directory,
            |operations_on_the_loaded_streams, project_directory| {
                operations_on_the_loaded_streams.start_playback(&project_directory)
            },
        )
        .await
        .map(|queued| {
            ControlOperationAnswer::ProjectStreamActionQueued(ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            })
        })
    }

    pub(super) async fn answer_stop_playback(
        &self,
        stop_playback_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = stop_playback_request;
        self.a_project_stream_action_queued_by(
            project_directory,
            |operations_on_the_loaded_streams, project_directory| {
                operations_on_the_loaded_streams.stop_playback(&project_directory)
            },
        )
        .await
        .map(|queued| {
            ControlOperationAnswer::ProjectStreamActionQueued(ProjectStreamActionQueuedToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            })
        })
    }

    pub(super) async fn answer_pause_playback(
        &self,
        pause_playback_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = pause_playback_request;
        {
            refuse_a_relative_project_directory(&project_directory)
                .map_err(ControlOperationRefusal::Failed)?;
            self.a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.pause_playback(&project_directory)
            })
            .await
            .map(|paused| {
                ControlOperationAnswer::PausePlayback(PausePlaybackToolResult {
                    project_directory: paused.project_directory,
                    streams: paused.stream_ids,
                })
            })
        }
    }

    pub(super) async fn answer_resume_playback(
        &self,
        resume_playback_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = resume_playback_request;
        {
            refuse_a_relative_project_directory(&project_directory)
                .map_err(ControlOperationRefusal::Failed)?;
            self.a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.resume_playback(&project_directory)
            })
            .await
            .map(|resumed| {
                ControlOperationAnswer::ResumePlayback(ResumePlaybackToolResult {
                    project_directory: resumed.project_directory,
                    streams: resumed.stream_ids,
                })
            })
        }
    }

    pub(super) async fn answer_describe_stream_action(
        &self,
        describe_stream_action_request: OneStreamActionRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneStreamActionRequest { action } = describe_stream_action_request;
        self.a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
            operations_on_the_loaded_streams.describe_stream_action(&action)
        })
        .await
        .map(|stream_action| {
            ControlOperationAnswer::DescribeStreamAction(describe_stream_action_tool_result_of(
                stream_action,
            ))
        })
    }

    pub(super) async fn answer_cancel_stream_action(
        &self,
        cancel_stream_action_request: OneStreamActionRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneStreamActionRequest { action } = cancel_stream_action_request;
        self.a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
            operations_on_the_loaded_streams.cancel_stream_action(&action)
        })
        .await
        .map(|stream_action| {
            ControlOperationAnswer::CancelStreamAction(CancelStreamActionToolResult {
                action: stream_action.action_id,
                state: stream_action_state_on_the_wire(stream_action.state),
            })
        })
    }
}
