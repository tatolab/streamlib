// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;

use super::operations::RuntimeOperations;
use super::{
    LoadedStreamAndTheOutputPortsItExposes, LoadedStreamTag, MachineName,
    OutputPortExposureOutcome, RunStreamRequest, Runner, StreamListing, StreamRemoveOutcome,
    StreamRunOutcome, StreamStartOutcome, StreamStopOutcome,
};
use crate::core::ProcessorDescriptor;
use crate::core::error::Result;
use crate::core::graph::OutputPortExposureLevel;
use crate::core::logging::LoadedStreamLogRecordsPage;

/// One page of a loaded stream's log records, and the tag of the load that
/// numbered them: a re-load under the same name numbers its records from 1
/// again under another tag.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecordsPageOfOneLoadedStream {
    /// The tag of the load whose records the page holds.
    pub stream_tag: LoadedStreamTag,
    /// The records, numbered by that load.
    pub records_page: LoadedStreamLogRecordsPage,
}

/// What the local API reaches a runtime through: the runtime's description, the
/// stream a call names, a stream's node catalog and log records, and the stream
/// actions.
///
/// Every stream action blocks — a load runs a compile and describes — so the
/// local API calls each from a blocking task.
///
/// Implemented by [`Runner`], and by the control plane's test stubs.
pub trait OperationsOnTheStreamsLoadedInThisRuntime: Send + Sync {
    /// This machine's name, the first part of every address.
    fn this_machines_name(&self) -> &MachineName;

    /// [`Runner::describe_this_runtime`].
    fn describe_this_runtime(&self) -> DescribeRuntimeToolResult;

    /// The operations on the loaded stream `stream_name` names. Refused with
    /// [`Error::NotFound`](crate::core::error::Error::NotFound) naming the loaded streams when it is not loaded.
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Arc<dyn RuntimeOperations>>;

    /// The node types the loaded stream `stream_name` names resolves — the
    /// native ones, then the ones described in its own interpreter — with the
    /// same [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The node types described in the own interpreter of the loaded stream
    /// `stream_name` names, with the same [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The cast names of the loaded streams, in order.
    fn names_of_the_loaded_streams(&self) -> Vec<String>;

    /// Every loaded stream, in name order, with the output ports it exposes
    /// now — one read of the stream table. Blocks while a stream compiles, so
    /// the local API calls it from a blocking task.
    fn every_loaded_stream_and_the_output_ports_it_exposes(
        &self,
    ) -> Vec<LoadedStreamAndTheOutputPortsItExposes>;

    /// The log records of the loaded stream `stream_name` names, numbered
    /// after `after`, at most `max_count` of them, with the tag of the load
    /// that numbered them; refused naming the loaded streams when it is not
    /// loaded.
    fn log_records_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
        after: u64,
        max_count: usize,
    ) -> Result<LogRecordsPageOfOneLoadedStream>;

    /// [`Runner::run_stream`].
    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome>;

    /// [`Runner::stop_stream`].
    fn stop_stream(&self, stream_name: &str) -> Result<StreamStopOutcome>;

    /// [`Runner::start_stream`].
    fn start_stream(&self, stream_name: &str) -> Result<StreamStartOutcome>;

    /// [`Runner::remove_stream`].
    fn remove_stream(&self, stream_name: &str) -> Result<StreamRemoveOutcome>;

    /// [`Runner::list_streams`].
    fn list_streams(&self) -> Vec<StreamListing>;

    /// [`Runner::expose_port`].
    fn expose_port(
        &self,
        stream_name: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome>;

    /// [`Runner::unload_the_attached_stream_if_still_the_same`].
    fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_name: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool;
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
        stream_name: &str,
    ) -> Result<Arc<dyn RuntimeOperations>> {
        self.loaded_stream_named(stream_name)
            .map(|stream| stream as Arc<dyn RuntimeOperations>)
    }

    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.loaded_stream_named(stream_name)
            .map(|stream| stream.node_types_this_stream_resolves().node_catalog())
    }

    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.loaded_stream_named(stream_name).map(|stream| {
            stream
                .node_types_this_stream_resolves()
                .descriptors_described_in_this_streams_processor_interpreter()
        })
    }

    fn names_of_the_loaded_streams(&self) -> Vec<String> {
        Runner::names_of_the_loaded_streams(self)
    }

    fn every_loaded_stream_and_the_output_ports_it_exposes(
        &self,
    ) -> Vec<LoadedStreamAndTheOutputPortsItExposes> {
        Runner::every_loaded_stream_and_the_output_ports_it_exposes(self)
    }

    fn log_records_of_the_stream_a_call_names(
        &self,
        stream_name: &str,
        after: u64,
        max_count: usize,
    ) -> Result<LogRecordsPageOfOneLoadedStream> {
        self.loaded_stream_named(stream_name)
            .map(|stream| LogRecordsPageOfOneLoadedStream {
                stream_tag: stream.stream_tag(),
                records_page: stream.log_records_after(after, max_count),
            })
    }

    fn run_stream(&self, request: RunStreamRequest) -> Result<StreamRunOutcome> {
        Runner::run_stream(self, request)
    }

    fn stop_stream(&self, stream_name: &str) -> Result<StreamStopOutcome> {
        Runner::stop_stream(self, stream_name)
    }

    fn start_stream(&self, stream_name: &str) -> Result<StreamStartOutcome> {
        Runner::start_stream(self, stream_name)
    }

    fn remove_stream(&self, stream_name: &str) -> Result<StreamRemoveOutcome> {
        Runner::remove_stream(self, stream_name)
    }

    fn list_streams(&self) -> Vec<StreamListing> {
        Runner::list_streams(self)
    }

    fn expose_port(
        &self,
        stream_name: &str,
        node: &str,
        port: &str,
        level: OutputPortExposureLevel,
    ) -> Result<OutputPortExposureOutcome> {
        Runner::expose_port(self, stream_name, node, port, level)
    }

    fn unload_the_attached_stream_if_still_the_same(
        &self,
        stream_name: &str,
        stream_tag: LoadedStreamTag,
    ) -> bool {
        Runner::unload_the_attached_stream_if_still_the_same(self, stream_name, stream_tag)
    }
}
