// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub(crate) mod address_chunk;
mod end_the_process_at_once;
mod engine_teardown_watchdog;
mod graph_change_listener;
mod helper_process_group_registry;
mod loaded_stream;
mod loaded_stream_output_port_exposure;
mod local_processor_type_registration;
mod machine_state_directory;
mod operations;
mod operations_on_the_streams_loaded_in_this_runtime;
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
mod stream_actions_of_this_runtime;
mod stream_environment;
mod surface_image_exchange;
mod tap;

pub use crate::core::compiler::{
    DescriptionOfTheAbandonedProcessorThreads, ProcessorDisplayNameAndId,
};
pub use address_chunk::what_one_address_chunk_may_be;
pub(crate) use end_the_process_at_once::{
    kill_every_helper_process_group_and_end_the_process_at_once, park_forever,
    park_forever_if_the_process_is_ending_at_once,
};
pub use engine_teardown_watchdog::{
    ArmedEngineTeardownWatchdog, EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
    TeardownProgressNoteOfOneStream, note_what_the_engine_teardown_is_waiting_on,
};
pub(crate) use engine_teardown_watchdog::{
    ArmedTeardownWatchdogOfOneStream, ENGINE_TEARDOWN_WATCHDOG_BUDGET,
    count_threads_abandoned_in_this_process,
};
pub use helper_process_group_registry::{
    deregister_a_helper_process_group, register_a_helper_process_group,
};
pub(crate) use helper_process_group_registry::{
    kill_every_registered_helper_process_group,
    kill_every_registered_helper_process_group_of_one_stream,
};
pub use loaded_stream::{HowALoadedStreamEnded, LoadedStreamInThisRuntime, LoadedStreamTag};
pub use loaded_stream_output_port_exposure::ExposedOutputPortReaderRegistration;
pub use machine_state_directory::{
    KEPT_STREAM_RECORD_FILE_MODE, KEPT_STREAM_RECORD_SCHEMA_VERSION, KeptStreamRecord,
    KeptStreamRecordReadFailure, KeptStreamRecordsInTheStateDirectory, OwnerExposureRuling,
    OwnerExposureRulingsSplitAroundTheLoad, the_owners_exposure_rulings_split_around_the_load,
};
pub use operations::{BoxFuture, NodeInTheGraph, RuntimeOperations};
pub use operations_on_the_streams_loaded_in_this_runtime::OperationsOnTheStreamsLoadedInThisRuntime;
pub use runtime::{
    OptionsForLoadingOneStream, Runner, RunnerConstructionOptions,
    StreamLoadObservingMachineShutdownRequests,
};
pub use runtime_name::RuntimeName;
#[cfg(test)]
pub(crate) use runtime_shutdown_request::TheMachinesShutdownEscalationClearedOnDrop;
pub(crate) use runtime_shutdown_request::escalate_the_machines_shutdown_for_a_delivered_signal;
pub use runtime_shutdown_request::{
    RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, RuntimeShutdownEscalation,
    ShutdownEscalationOfOneStream, is_the_machines_shutdown_requested,
    request_the_shutdown_of_every_loaded_stream, take_the_machines_shutdown_escalation,
    the_machines_shutdown_escalation,
};
pub use runtime_unique_id::RuntimeUniqueId;
pub use status::RuntimeStatus;
pub use crate::core::compiler::compiler_ops::stream_function_compile_in_the_projects_interpreter::{
    PROJECT_STREAM_COMPILE_ENTRY_MODULE, STREAM_FUNCTION_COMPILE_BOUND,
    StreamFunctionCompiledInTheProjectsInterpreter,
    compile_the_stream_function_in_the_projects_interpreter, the_projects_venv_interpreter,
};
pub use stream_actions_of_this_runtime::{
    KeptStreamReloadAtTheStart, LoadedStreamHolding, OutputPortExposureOutcome, RunStreamRequest, StreamListing,
    StreamListingState, StreamRemoveOutcome, StreamRunOutcome, StreamStartOutcome,
    StreamStopOutcome,
};
pub use stream_environment::StreamEnvironment;
pub use surface_image_exchange::ExchangedPublishedSurfaceFramePngImage;
pub use tap::TapSubscription;
