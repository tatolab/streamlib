// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub(crate) mod address_chunk;
mod end_the_process_at_once;
mod engine_teardown_watchdog;
mod exposed_output_port_subscription;
mod graph_change_listener;
mod helper_process_group_registry;
mod loaded_stream;
mod loaded_stream_output_port_exposure;
mod local_processor_type_registration;
mod machine_name;
mod machine_state_directory;
mod open_descriptor_soft_limit;
mod operations;
mod operations_on_the_streams_loaded_in_this_runtime;
mod operations_runtime;
mod processor_interpreter_launch_record;
mod project_stream_action_queue;
mod project_venv_made_with_the_runtime_unit_uv;
pub(crate) use operations_runtime::mark_this_thread_as_a_processor_execution_thread;
mod reserved_slot_forwarder;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_crash_pinned_on_a_stream;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod status;
mod stream_actions_of_this_runtime;
mod stream_environment;
mod stream_location_and_id;

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
pub use exposed_output_port_subscription::{
    ExposedOutputPortOfALoadedStream, ExposedOutputPortSourceWiring, ExposedOutputPortSubscription,
    ExposureChangeReceiver, LoadedStreamAndTheOutputPortsItExposes,
};
pub use helper_process_group_registry::{
    deregister_a_helper_process_group, register_a_helper_process_group,
};
pub(crate) use helper_process_group_registry::{
    kill_every_registered_helper_process_group,
    kill_every_registered_helper_process_group_of_one_stream,
};
pub use loaded_stream::{HowALoadedStreamEnded, LoadedStreamInThisRuntime, LoadedStreamTag};
pub use machine_name::MachineName;
pub use machine_state_directory::{
    OwnerExposureRuling, PendingStreamActionKind, PendingStreamActionOnTheRecord,
    STREAM_RECORD_SCHEMA_VERSION, StreamRecord, StreamRecordAsRead, StreamRecordOfAnEarlierRuntime,
    StreamRecordReadFailure, StreamRecordsInTheStateDirectory,
};
pub(crate) use machine_state_directory::{
    OwnerExposureRulingsSplitAroundTheLoad, the_owners_exposure_rulings_split_around_the_load,
};
pub use open_descriptor_soft_limit::{
    OpenDescriptorSoftLimitRaise, raise_the_open_descriptor_soft_limit_to_its_ceiling,
};
pub(crate) use open_descriptor_soft_limit::{
    start_the_interpreter_at_the_open_descriptor_soft_limit_the_runtime_started_with,
    the_open_descriptor_limit,
};
pub use operations::{BoxFuture, NodeInTheGraph, RuntimeOperations};
pub use operations_on_the_streams_loaded_in_this_runtime::OperationsOnTheStreamsLoadedInThisRuntime;
pub use project_stream_action_queue::{
    FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT, ProjectStreamsListing, StreamAction,
    StreamActionKind, StreamActionQueued, StreamActionState, StreamActionStep,
    StreamActionWorkingOnAStream, StreamOfAStreamAction, StreamTheLastCompileOfTheProjectNamed,
    WhatTheLastCompileOfTheProjectGave,
};
pub(crate) use runtime::what_interrupted_a_load;
pub use runtime::{
    EveryStreamEndedDuringTheWait, OptionsForLoadingOneStream, Runner, RunnerConstructionOptions,
    StreamLoadObservingInterrupts, WhatALoadedStreamIsKeyedBy,
};
pub use runtime_crash_pinned_on_a_stream::{
    CrashOfThePreviousRuntimeRun, HowThePreviousRuntimeRunEnded, RuntimeRunInProgressRecord,
    pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread,
};
pub(crate) use runtime_crash_pinned_on_a_stream::{
    StreamThisThreadWorksFor, end_the_run_in_progress_record_as_the_owner_ends_the_process,
    mark_this_thread_as_working_for, pin_the_runtimes_crash_on_each_stream,
    pin_the_runtimes_crash_on_no_stream,
};
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
pub use stream_actions_of_this_runtime::{
    LoadedStreamHolding, OutputPortExposureOutcome, RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM,
    RunProjectRequest, RunStreamRequest, StreamListing, StreamListingState,
    StreamLoadedOrFailedByARun, StreamReloadAtTheStart, StreamRenameOutcome, StreamRunOutcome,
    StreamStateAfterItsRun,
};
pub use stream_environment::StreamEnvironment;
pub use stream_location_and_id::StreamLocation;
