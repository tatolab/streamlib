// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The operations that start a project: one created from the starter or a sample, and the samples
//! listed.

use streamlib::sdk::runtime::CreateProjectRequest as CreateProjectInTheRuntimeRequest;
use streamlib_control_contract::{CreateProjectRequest, ListSamplesRequest};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    CreateProjectToolResult, ListSamplesToolResult,
};

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, refuse_a_relative_project_directory,
};

impl ControlOperationDispatcher {
    pub(crate) async fn create_project(
        &self,
        CreateProjectRequest {
            project_directory,
            sample,
        }: CreateProjectRequest,
    ) -> Result<CreateProjectToolResult, ControlOperationRefusal> {
        if let Some(project_directory) = &project_directory {
            refuse_a_relative_project_directory(project_directory)?;
        }
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                operations_on_the_loaded_streams.create_project(CreateProjectInTheRuntimeRequest {
                    project_directory,
                    sample,
                })
            })
            .await?;
        Ok(CreateProjectToolResult {
            action: queued.action_id,
            stream: queued.stream_id,
            project_directory: queued.project_directory,
        })
    }

    pub(crate) async fn list_samples(
        &self,
        ListSamplesRequest {}: ListSamplesRequest,
    ) -> Result<ListSamplesToolResult, ControlOperationRefusal> {
        Ok(self.operations_on_the_loaded_streams.list_samples())
    }
}
