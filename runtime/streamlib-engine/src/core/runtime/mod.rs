// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod capability_extensions;
mod end_the_process_at_once;
mod engine_teardown_watchdog;
mod graph_change_listener;
mod helper_process_group_registry;
mod local_processor_type_registration;
pub mod mesh;
pub(crate) mod mesh_address_chunk;
mod operations;
mod operations_runtime;
mod link_requests_applied_into_this_runtimes_graph;
mod output_ports_in_this_runtimes_graph;
pub(crate) use operations_runtime::mark_this_thread_as_a_processor_execution_thread;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_mesh_configuration;
pub(crate) mod runtime_name;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod stated_configuration_value;
mod status;
mod streamlib_runtime_directory;
mod surface_image_exchange;
mod tap;

pub use crate::core::compiler::{
    DescriptionOfTheAbandonedProcessorThreads, ProcessorDisplayNameAndId,
};
pub use crate::core::signals::ScopedShutdownSignalOwnership;
pub use capability_extensions::{LoadedCapabilityExtension, LoadedCapabilityExtensionRegistry};
pub(crate) use end_the_process_at_once::kill_every_helper_process_group_and_end_the_process_at_once;
pub use engine_teardown_watchdog::{
    ArmedEngineTeardownWatchdog, EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
    note_what_the_engine_teardown_is_waiting_on,
};
pub(crate) use helper_process_group_registry::kill_every_registered_helper_process_group;
pub use helper_process_group_registry::{
    deregister_a_helper_process_group, register_a_helper_process_group,
};
pub use mesh::{
    RuntimeMeshMembership, RuntimeMeshObservation, RuntimeMeshObservationRequest,
    observe_a_runtime_mesh,
};
pub use mesh_address_chunk::what_one_mesh_address_chunk_may_be;
pub use operations::{BoxFuture, RuntimeOperations};
pub(crate) use link_requests_applied_into_this_runtimes_graph::LinkRequestsAppliedIntoThisRuntimesGraph;
pub(crate) use output_ports_in_this_runtimes_graph::OutputPortsInThisRuntimesGraph;
pub use runtime::Runner;
pub use runtime_mesh_configuration::RuntimeMeshConfiguration;
pub use runtime_name::RuntimeName;
#[cfg(test)]
pub(crate) use runtime_shutdown_request::RuntimeShutdownEscalationClearedOnDrop;
pub(crate) use runtime_shutdown_request::escalate_runtime_shutdown_for_a_delivered_signal;
pub use runtime_shutdown_request::{
    RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, RuntimeShutdownEscalation,
    is_runtime_shutdown_forced, is_runtime_shutdown_requested, request_runtime_shutdown,
    runtime_shutdown_escalation, take_runtime_shutdown_escalation,
};
pub use runtime_unique_id::RuntimeUniqueId;
pub use status::RuntimeStatus;
pub use streamlib_runtime_directory::StreamlibRuntimeDirectory;
pub(crate) use streamlib_runtime_directory::current_process_uid;
pub use surface_image_exchange::ExchangedPublishedSurfaceFramePngImage;
pub use tap::TapSubscription;
