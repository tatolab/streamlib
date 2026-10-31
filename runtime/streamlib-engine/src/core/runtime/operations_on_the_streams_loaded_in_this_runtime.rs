// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;
use std::sync::Arc;

use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;

use super::operations::RuntimeOperations;
use super::{
    LoadedStreamAndTheOutputPortsItExposes, MachineName, OutputPortExposureOutcome,
    ProjectStreamsListing, RunProjectRequest, RunStreamRequest, Runner, StreamAction,
    StreamActionQueued, StreamListing, StreamRenameOutcome, StreamRunOutcome,
};
use crate::core::ProcessorDescriptor;
use crate::core::error::Result;
use crate::core::graph::OutputPortExposureLevel;

/// What the local API reaches a runtime through: the runtime's description, the
/// stream a call names, a stream's node catalog, and the stream
/// actions.
///
/// `run_project`, `start_stream`, `stop_stream` and `remove_project` answer at
/// once, their actions running on the project's stream action queue; every
/// other call may block — `run_stream` runs a compile and describes — so the
/// local API calls each from a blocking task.
///
/// Implemented by [`Runner`], and by the control plane's test stubs.
pub trait OperationsOnTheStreamsLoadedInThisRuntime: Send + Sync {
    /// This machine's name, the first part of every address.
    fn this_machines_name(&self) -> &MachineName;

    /// [`Runner::describe_this_runtime`].
    fn describe_this_runtime(&self) -> DescribeRuntimeToolResult;

    /// The operations on the loaded stream `stream_id` names. Refused with
    /// [`Error::NotFound`](crate::core::error::Error::NotFound) naming the loaded streams when it is not loaded.
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Arc<dyn RuntimeOperations>>;

    /// The node types the loaded stream `stream_id` names resolves — the
    /// native ones, then the ones described in its own interpreter — with the
    /// same [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The node types described in the own interpreter of the loaded stream
    /// `stream_id` names, with the same [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The ids of the loaded streams, in order.
    fn ids_of_the_loaded_streams(&self) -> Vec<String>;

    /// Every loaded stream, in id order, with the output ports it exposes
    /// now — one read of the stream table. Blocks while a stream compiles, so
    /// the local API calls it from a blocking task.
    fn every_loaded_stream_and_the_output_ports_it_exposes(
        &self,
    ) -> Vec<LoadedStreamAndTheOutputPortsItExposes>;

    /// [`Runner::run_stream`].
    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome>;

    /// [`Runner::run_project`].
    fn run_project(&self, request: RunProjectRequest) -> Result<StreamActionQueued>;

    /// [`Runner::describe_stream_action`].
    fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction>;

    /// [`Runner::cancel_stream_action`].
    fn cancel_stream_action(&self, action_id: &str) -> Result<StreamAction>;

    /// [`Runner::list_project_streams`].
    fn list_project_streams(&self, project_directory: &Path) -> Result<ProjectStreamsListing>;

    /// [`Runner::stop_stream`].
    fn stop_stream(&self, stream_id: &str) -> Result<StreamActionQueued>;

    /// [`Runner::start_stream`].
    fn start_stream(&self, stream_id: &str) -> Result<StreamActionQueued>;

    /// [`Runner::remove_project`].
    fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued>;

    /// [`Runner::rename_stream`].
    fn rename_stream(&self, stream_id: &str, display_name: &str) -> Result<StreamRenameOutcome>;

    /// [`Runner::list_streams`].
    fn list_streams(&self) -> Vec<StreamListing>;

    /// [`Runner::expose_port`].
    fn expose_port(
        &self,
        stream_id: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome>;
}

impl OperationsOnTheStreamsLoadedInThisRuntime for Runner {
    fn this_machines_name(&self) -> &MachineName {
        self.machine_name()
    }

    fn describe_this_runtime(&self) -> DescribeRuntimeToolResult {
        Runner::describe_this_runtime(self)
    }

    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Arc<dyn RuntimeOperations>> {
        self.loaded_stream_of_the_id(stream_id)
            .map(|stream| stream as Arc<dyn RuntimeOperations>)
    }

    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.loaded_stream_of_the_id(stream_id)
            .map(|stream| stream.node_types_this_stream_resolves().node_catalog())
    }

    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.loaded_stream_of_the_id(stream_id).map(|stream| {
            stream
                .node_types_this_stream_resolves()
                .descriptors_described_in_this_streams_processor_interpreter()
        })
    }

    fn ids_of_the_loaded_streams(&self) -> Vec<String> {
        Runner::ids_of_the_loaded_streams(self)
    }

    fn every_loaded_stream_and_the_output_ports_it_exposes(
        &self,
    ) -> Vec<LoadedStreamAndTheOutputPortsItExposes> {
        Runner::every_loaded_stream_and_the_output_ports_it_exposes(self)
    }

    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        Runner::run_stream(self, request)
    }

    fn run_project(&self, request: RunProjectRequest) -> Result<StreamActionQueued> {
        Runner::run_project(self, request)
    }

    fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        Runner::describe_stream_action(self, action_id)
    }

    fn cancel_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        Runner::cancel_stream_action(self, action_id)
    }

    fn list_project_streams(&self, project_directory: &Path) -> Result<ProjectStreamsListing> {
        Runner::list_project_streams(self, project_directory)
    }

    fn stop_stream(&self, stream_id: &str) -> Result<StreamActionQueued> {
        Runner::stop_stream(self, stream_id)
    }

    fn start_stream(&self, stream_id: &str) -> Result<StreamActionQueued> {
        Runner::start_stream(self, stream_id)
    }

    fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        Runner::remove_project(self, project_directory)
    }

    fn rename_stream(&self, stream_id: &str, display_name: &str) -> Result<StreamRenameOutcome> {
        Runner::rename_stream(self, stream_id, display_name)
    }

    fn list_streams(&self) -> Vec<StreamListing> {
        Runner::list_streams(self)
    }

    fn expose_port(
        &self,
        stream_id: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        Runner::expose_port(self, stream_id, node, port, level)
    }
}
