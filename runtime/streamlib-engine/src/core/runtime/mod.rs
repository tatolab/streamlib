// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub(crate) mod address_chunk;
mod end_the_process_at_once;
mod engine_teardown_watchdog;
mod graph_change_listener;
mod helper_process_group_registry;
mod local_processor_type_registration;
mod operations;
mod operations_runtime;
mod processor_interpreter_launch_record;
pub(crate) use operations_runtime::mark_this_thread_as_a_processor_execution_thread;
#[allow(clippy::module_inception)]
mod runtime;
pub(crate) mod runtime_name;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod stated_configuration_value;
mod status;
mod stream_environment;
mod streamlib_runtime_directory;
mod surface_image_exchange;
mod tap;

pub use crate::core::compiler::{
    DescriptionOfTheAbandonedProcessorThreads, ProcessorDisplayNameAndId,
};
pub use crate::core::signals::ScopedShutdownSignalOwnership;
pub use address_chunk::what_one_address_chunk_may_be;
pub(crate) use end_the_process_at_once::{
    kill_every_helper_process_group_and_end_the_process_at_once, park_forever,
    park_forever_if_the_process_is_ending_at_once,
};
pub use engine_teardown_watchdog::{
    ArmedEngineTeardownWatchdog, EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
    note_what_the_engine_teardown_is_waiting_on,
};
pub(crate) use helper_process_group_registry::kill_every_registered_helper_process_group;
pub use helper_process_group_registry::{
    deregister_a_helper_process_group, register_a_helper_process_group,
};
pub use operations::{BoxFuture, NodeInTheGraph, RuntimeOperations};
pub use runtime::{Runner, RunnerConstructionOptions};
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
pub use stream_environment::StreamEnvironment;
pub use streamlib_runtime_directory::StreamlibRuntimeDirectory;
pub(crate) use streamlib_runtime_directory::current_process_uid;
pub use surface_image_exchange::ExchangedPublishedSurfaceFramePngImage;
pub use tap::TapSubscription;
