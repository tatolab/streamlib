// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(any(test, feature = "test-support"))]
pub mod a_project_on_disk_for_a_test;
pub(crate) mod address_chunk;
mod end_the_process_at_once;
mod engine_teardown_watchdog;
mod function_node_states_carried_across_loads;
mod graph_change_listener;
mod helper_process_group_registry;
mod live_graph_reconciled_to_a_graph_document;
mod loaded_stream;
mod local_processor_type_registration;
mod machine_name;
mod machine_state_directory;
mod new_project_name_drawing;
mod new_project_scaffold;
mod node_of_a_stream_not_running;
mod open_descriptor_soft_limit;
mod operations;
mod operations_on_the_streams_loaded_in_this_runtime;
mod operations_runtime;
mod output_port_viewer;
mod processor_interpreter_launch_record;
mod project_first_party_packages_moved_to_the_runtime_version;
mod project_folder_moved_to_the_trash;
mod project_node_code_a_load_describes;
mod project_stream_action_queue;
mod project_venv_made_with_the_runtime_unit_uv;
mod project_venv_sync_progress;
pub(crate) use operations_runtime::mark_this_thread_as_a_processor_execution_thread;
pub(crate) use project_venv_made_with_the_runtime_unit_uv::the_projects_venv_interpreter;
#[allow(clippy::module_inception)]
mod runtime;
mod runtime_crash_pinned_on_a_stream;
mod runtime_shutdown_request;
mod runtime_unique_id;
mod status;
mod stream_actions_of_this_runtime;
mod stream_environment;
mod stream_graph_operation_applied_live;
mod stream_location_and_id;
mod stream_step_coordinator;
mod view_node_output_written_by_a_panel;

pub use crate::core::compiler::compiler_ops::project_node_types_listed_from_source::{
    ImportedProjectPythonSources, PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE,
    list_a_projects_node_types_and_graph_generators_from_source,
};
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
pub use function_node_states_carried_across_loads::{
    FunctionNodeState, FunctionNodeStatesCarriedForEveryStream,
    FunctionNodeStatesCarriedForOneStream, TheStoreOfEachNodeOfAStream,
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
    PendingStreamActionKind, PendingStreamActionOnTheRecord, STREAM_RECORD_SCHEMA_VERSION,
    StreamRecord, StreamRecordAsRead, StreamRecordOfAnEarlierRuntime, StreamRecordReadFailure,
    StreamRecordsInTheStateDirectory,
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
pub use output_port_viewer::{OutputPortViewerChannelHandOff, OutputPortViewerHeldOpen};
pub use project_first_party_packages_moved_to_the_runtime_version::ProjectHistoryRecordingFirstPartyPackageMoves;
pub use project_stream_action_queue::{
    FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT, StreamAction, StreamActionKind, StreamActionQueued,
    StreamActionState, StreamActionStep, StreamActionWorkingOnAStream, StreamOfAStreamAction,
};
pub use project_venv_sync_progress::{
    ProjectVenvSyncPackage, ProjectVenvSyncPackageKind, ProjectVenvSyncPackageState,
    ProjectVenvSyncPhase, ProjectVenvSyncProgress,
};
pub(crate) use runtime::what_interrupted_a_load;
pub use runtime::{
    CreatedProjectFirstCommit, EveryStreamEndedDuringTheWait, OptionsForLoadingOneStream, Runner,
    RunnerConstructionOptions, StreamLoadObservingInterrupts,
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
    CreateProjectRequest, LoadedStreamHolding, ProjectCreationQueued, ProjectOpenOrReloadRequest,
    ProjectPausedOrResumed, RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM, StreamListing,
    StreamListingState, StreamReloadAtTheStart,
};
pub use stream_environment::StreamEnvironment;
pub use stream_location_and_id::StreamLocation;
