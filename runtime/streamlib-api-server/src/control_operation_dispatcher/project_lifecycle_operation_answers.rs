// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::runtime::ProjectOpenOrReloadRequest;
use streamlib_control_contract::OneProjectByItsDirectoryRequest;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ProjectStreamActionQueuedToolResult, RemoveProjectToolResult,
};

use super::{ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal};

impl ControlOperationDispatcher {
    pub(super) async fn answer_open_project(
        &self,
        open_project_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = open_project_request;
        self.a_project_stream_action_queued_by(
            project_directory,
            |operations_on_the_loaded_streams, project_directory| {
                operations_on_the_loaded_streams
                    .open_project(ProjectOpenOrReloadRequest { project_directory })
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

    pub(super) async fn answer_reload_project(
        &self,
        reload_project_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = reload_project_request;
        self.a_project_stream_action_queued_by(
            project_directory,
            |operations_on_the_loaded_streams, project_directory| {
                operations_on_the_loaded_streams
                    .reload_project(ProjectOpenOrReloadRequest { project_directory })
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

    pub(super) async fn answer_close_project(
        &self,
        close_project_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = close_project_request;
        self.a_project_stream_action_queued_by(
            project_directory,
            |operations_on_the_loaded_streams, project_directory| {
                operations_on_the_loaded_streams.close_project(&project_directory)
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

    pub(super) async fn answer_remove_project(
        &self,
        remove_project_request: OneProjectByItsDirectoryRequest,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneProjectByItsDirectoryRequest { project_directory } = remove_project_request;
        self.a_project_stream_action_queued_by(
            project_directory,
            |operations_on_the_loaded_streams, project_directory| {
                operations_on_the_loaded_streams.remove_project(&project_directory)
            },
        )
        .await
        .map(|queued| {
            ControlOperationAnswer::RemoveProject(RemoveProjectToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            })
        })
    }
}
