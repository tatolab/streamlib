// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod capability_extensions;
mod engine_teardown_watchdog;
mod graph_change_listener;
mod helper_process_group_registry;
mod local_processor_type_registration;
mod operations;
mod operations_runtime;
pub(crate) use operations_runtime::mark_this_thread_as_a_processor_execution_thread;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod status;
mod streamlib_runtime_directory;
mod surface_image_exchange;
mod tap;

pub use crate::core::compiler::{
    AbandonedProcessorThread, refusal_naming_the_abandoned_processor_threads,
};
pub use capability_extensions::{LoadedCapabilityExtension, LoadedCapabilityExtensionRegistry};
pub use engine_teardown_watchdog::{
    ArmedEngineTeardownWatchdog, EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
    note_what_the_engine_teardown_is_waiting_on,
};
#[cfg(unix)]
pub(crate) use helper_process_group_registry::kill_every_registered_helper_process_group;
pub use helper_process_group_registry::{
    deregister_a_helper_process_group, register_a_helper_process_group,
};
pub use operations::{BoxFuture, RuntimeOperations};
pub use runtime::Runner;
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
