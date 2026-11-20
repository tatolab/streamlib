// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(any(test, feature = "test-support"))]
pub mod a_project_on_disk_for_a_test;
pub(crate) mod address_chunk;
#[cfg(any(test, feature = "test-support"))]
pub mod graph_renders_and_commits_counted_for_a_test;
mod operations;
mod operations_runtime;
pub(crate) use operations_runtime::mark_this_thread_as_a_node_execution_thread;
pub(crate) use projects_and_their_venv::project_venv_made_with_the_runtime_unit_uv::the_projects_venv_interpreter;
mod machine_state_and_process_end;
mod node_interpreters;
mod projects_and_their_venv;
#[allow(
    clippy::module_inception,
    reason = "the module holds the `Runtime` type its parent is named for"
)]
mod runtime;
mod streams_loaded_in_this_runtime;

pub use crate::core::compiler::compiler_ops::project_node_types_listed_from_source::{
    ImportedProjectPythonSources, PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE,
    list_a_projects_node_types_and_graph_generators_from_source,
};
pub use crate::core::compiler::compiler_ops::extension_code_checked_against_the_kit::{
    EXTENSION_SDK_KIT_ENVIRONMENT_VARIABLE, ExtensionCodeCheck, ExtensionCodeCheckKind,
    ExtensionCodeCheckRun, ExtensionCodeCheckers, ExtensionCodeCheckersHeld, ExtensionCodeFinding,
    ExtensionCodeFindingSeverity, ExtensionCodeNotChecked, ExtensionCodeNotCheckedCode,
    ExtensionFileChecked, ExtensionSdkKitTemplate,
};
pub use crate::core::compiler::compiler_ops::python_node_declarations_checked_in_the_projects_interpreter::{
    PYTHON_NODE_DECLARATION_CHECK_ENTRY_MODULE, PythonModuleChecked, PythonNodeDeclarationCheck,
    PythonNodeDeclarationCheckNotFinished, PythonNodeDeclarationFinding,
    PythonNodeDeclarationFindingSeverity, check_a_projects_python_node_declarations,
};
pub use crate::core::compiler::node_thread_shutdown::{
    DescriptionOfTheAbandonedNodeThreads, NodeDisplayNameAndId,
};
pub use address_chunk::what_one_address_chunk_may_be;
pub(crate) use machine_state_and_process_end::end_the_process_at_once::{
    kill_every_helper_process_group_and_end_the_process_at_once, park_forever,
    park_forever_if_the_process_is_ending_at_once,
};
pub use machine_state_and_process_end::engine_teardown_watchdog::{
    ArmedEngineTeardownWatchdog, EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED,
    TeardownProgressNoteOfOneStream, note_what_the_engine_teardown_is_waiting_on,
};
pub(crate) use machine_state_and_process_end::engine_teardown_watchdog::{
    ArmedTeardownWatchdogOfOneStream, ENGINE_TEARDOWN_WATCHDOG_BUDGET,
    count_threads_abandoned_in_this_process,
};
pub use machine_state_and_process_end::machine_name::MachineName;
pub use machine_state_and_process_end::runtime_crash_pinned_on_a_stream::{
    CrashOfThePreviousRuntimeRun, HowThePreviousRuntimeRunEnded, RuntimeRunInProgressRecord,
    pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread,
};
pub(crate) use machine_state_and_process_end::runtime_crash_pinned_on_a_stream::{
    StreamThisThreadWorksFor, end_the_run_in_progress_record_as_the_owner_ends_the_process,
    mark_this_thread_as_working_for, pin_the_runtimes_crash_on_each_stream,
    pin_the_runtimes_crash_on_no_stream,
};
#[cfg(test)]
pub(crate) use machine_state_and_process_end::runtime_shutdown_request::TheMachinesShutdownEscalationClearedOnDrop;
pub(crate) use machine_state_and_process_end::runtime_shutdown_request::escalate_the_machines_shutdown_for_a_delivered_signal;
pub use machine_state_and_process_end::runtime_shutdown_request::{
    RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, RuntimeShutdownEscalation,
    ShutdownEscalationOfOneStream, is_the_machines_shutdown_requested,
    request_the_shutdown_of_every_loaded_stream, take_the_machines_shutdown_escalation,
    the_machines_shutdown_escalation,
};
pub use machine_state_and_process_end::runtime_unique_id::RuntimeUniqueId;
pub use node_interpreters::helper_process_group_registry::{
    deregister_a_helper_process_group, register_a_helper_process_group,
};
pub(crate) use node_interpreters::helper_process_group_registry::{
    kill_every_registered_helper_process_group,
    kill_every_registered_helper_process_group_of_one_stream,
};
pub use node_interpreters::open_descriptor_soft_limit::{
    OpenDescriptorSoftLimitRaise, raise_the_open_descriptor_soft_limit_to_its_ceiling,
};
pub(crate) use node_interpreters::open_descriptor_soft_limit::{
    start_the_interpreter_at_the_open_descriptor_soft_limit_the_runtime_started_with,
    the_open_descriptor_limit,
};
pub use operations::{
    ANodeNotBuilt, ANodeTypesPort, BoxFuture, GraphEditBatch, GraphEditBatchRefused,
    HowFarARefusedBatchWasTakenBack, NodeInTheGraph, PortOfANodeOfAType, RuntimeOperations,
    WhatALiveConnectMakesOfALink,
};
pub use projects_and_their_venv::project_first_party_packages_moved_to_the_runtime_version::ProjectHistoryRecordingFirstPartyPackageMoves;
pub use projects_and_their_venv::project_venv_sync_progress::{
    ProjectVenvSyncPackage, ProjectVenvSyncPackageKind, ProjectVenvSyncPackageState,
    ProjectVenvSyncPhase, ProjectVenvSyncProgress,
};
pub(crate) use runtime::what_interrupted_a_load;
pub use runtime::{
    CreatedProjectFirstCommit, EveryStreamEndedDuringTheWait, OptionsForLoadingOneStream, Runner,
    RunnerConstructionOptions, StreamLoadObservingInterrupts,
};
pub use streams_loaded_in_this_runtime::function_node_states_carried_across_loads::{
    FunctionNodeState, FunctionNodeStatesCarriedForEveryStream,
    FunctionNodeStatesCarriedForOneStream, TheStoreOfEachNodeOfAStream,
};
pub use streams_loaded_in_this_runtime::loaded_stream::{
    HowALoadedStreamEnded, LoadedStreamInThisRuntime, LoadedStreamTag,
};
pub use streams_loaded_in_this_runtime::machine_state_directory::{
    PendingStreamActionKind, PendingStreamActionOnTheRecord, STREAM_RECORD_SCHEMA_VERSION,
    StreamFailureKind, StreamRecord, StreamRecordAsRead, StreamRecordOfAnEarlierRuntime,
    StreamRecordReadFailure, StreamRecordsInTheStateDirectory,
};
pub use streams_loaded_in_this_runtime::operations_on_the_streams_loaded_in_this_runtime::OperationsOnTheStreamsLoadedInThisRuntime;
pub use streams_loaded_in_this_runtime::output_port_viewer::{
    OutputPortViewerChannelHandOff, OutputPortViewerHeldOpen,
};
pub use streams_loaded_in_this_runtime::project_stream_action_queue::{
    FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT, StreamAction, StreamActionKind, StreamActionQueued,
    StreamActionState, StreamActionStep, StreamActionWorkingOnAStream, StreamOfAStreamAction,
};
pub use streams_loaded_in_this_runtime::status::RuntimeStatus;
pub use streams_loaded_in_this_runtime::stream_actions_of_this_runtime::{
    CreateProjectRequest, LoadedStreamHolding, ProjectCreationQueued, ProjectOpenOrReloadRequest,
    ProjectPausedOrResumed, RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM, StreamListing,
    StreamListingState, StreamReloadAtTheStart, StreamsToReadOrEdit,
};
pub use streams_loaded_in_this_runtime::stream_environment::StreamEnvironment;
pub use streams_loaded_in_this_runtime::stream_input_declared_by_a_graph_input::TopLevelGraphInputDeclaringAStreamInput;
pub use streams_loaded_in_this_runtime::stream_location_and_id::StreamLocation;
