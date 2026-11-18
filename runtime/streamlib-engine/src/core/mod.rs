// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

// Engine-internal modules. Module-path is `pub(crate)` so consumers
// cannot reach `streamlib_engine::core::<name>` (or
// `streamlib::engine_internal::core::<name>`) — the boundary is
// type-system-enforced at the engine source-of-truth. Items that
// genuinely need to cross the boundary are re-exported below as
// narrow `pub use` selections; items not re-exported stay
// engine-internal.
pub(crate) mod compiler;
#[cfg(test)]
mod engine_build_id_composition;
#[cfg(test)]
mod engine_build_id_composition_tests;
pub(crate) mod logging;
pub(crate) mod signals;
#[cfg(test)]
pub(crate) mod test_support;

// Customer-facing modules. Module-path stays `pub` so consumers
// can reach `streamlib::sdk::<name>` via the SDK's per-module
// re-exports.
pub mod child_process_lifetime;
pub mod child_process_restart_backoff;
pub mod color;
pub mod context;
pub mod descriptors;
pub mod error;
pub mod execution;
pub mod graph;
pub mod json_schema;
pub mod machine_global_unique_name;
pub mod media_clock;
pub mod nodes;
pub(crate) mod output_port_stream_facts;
pub mod prelude;
pub mod pubsub;
pub mod rhi;
pub mod runtime;
pub mod shared_native_services;
pub mod stable_short_id;
pub mod supervised_child_process;
pub mod texture;
pub mod unix_socket_path_cleared_for_bind;
pub mod utils;

// Customer-facing modules.
pub use context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream, AudioClock,
    AudioClockConfig, AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream,
    AudioSampleFormat, AudioStreamFormat, AudioTickCallback, AudioTickContext,
    CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE, CaptureDeviceAccessPrompting,
    CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff, CapturedVideoFrameFromDevice,
    CapturedVideoFrameHandOff, DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
    DeviceBackendArmUnavailableReason, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport, GpuContext, GpuContextFullAccess, GpuContextLimitedAccess,
    GpuPoolClaimTable, GpuPoolClaimTableRow, GpuPoolClaimTableRowLease, GpuPoolClaimTables,
    GpuPoolClaimTablesOfOnePool, GpuPoolClaimantColumn, GpuPoolClaimantProcessId,
    GpuPoolFrameRecycled, GpuPoolSlotFrameDescriptorBody, GpuPoolSlotRehandTest, IsolationTier,
    NativeNodeOutputPixelBufferFrame, NodeOutputPixelBufferPoolOfOneNativeNode,
    OneOffAcquisitionCountedForItsNode, OutputTextureRegisteredUntilDropped, PoolSlotId,
    PooledTextureHandle, ProjectStoresOpenInThisRuntime, RequestedAudioStreamFormat,
    RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess, SharedAudioClock,
    SharedAudioDeviceBackend, SharedVideoDeviceBackend, SilentNullAudioDeviceBackend,
    SoftwareAudioClock, StoreScopedToThisNode, StreamClock, StreamClockReading, StreamClockSpec,
    SurfaceCheckOutLeaseHandOff, SurfaceCheckOutLeaseHolderId, SurfaceCheckOutLeaseRegistry,
    SurfaceClaimedAgainstProducerReuse, SurfaceStore, TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
    TextureCrossProcessImportability, TexturePool, TexturePoolDescriptor, TexturePoolKey,
    TexturePoolStats, TextureRegistration, TextureRing, TextureRingInner, TextureRingSlot,
    VideoCaptureDevice, VideoCaptureStream, VideoCaptureStreamFormat, VideoDeviceBackend,
    VideoDeviceStreamRequest, machine_time_ns_a_published_clock_reaches,
    open_capture_stream_in_requested_format, open_playback_stream_in_requested_format,
    platform_audio_clock_for_one_stream, probe_audio_device_backend, probe_video_device_backend,
    stream_clock_reading_of_a_published_clock, stream_time_ns_on_a_published_clock, texture_pool,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuCapabilitiesSnapshot,
    GpuPoolClaimTableSharedRows, GraphicsKernelHeldForOneDraw, RayTracingKernelHeldForOneTrace,
    SurfaceBoundKernelBindingResource,
};
#[cfg(target_os = "linux")]
pub use context::{SurfaceExportStaging, SurfaceExportStagingResidency};
pub use descriptors::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    ArrayOutputPortResidency, AudioWindowContract, AudioWindowContractDeclaredValues,
    ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType, CodeExamples, DescribedDeclaration,
    DescriptionLintFinding, DescriptionLintProblem, FunctionNodeLanguage,
    IMAGE_TEXTURE_FORMAT_WIRE_NAMES, ImageOutputPortFrameRate, ImageStreamContentLightLevel,
    ImageStreamFacts, ImageStreamFrameRate, ImageStreamMasteringDisplayColourVolume,
    NODE_REFERENCE_SCHEMA_KEYWORD, NodeClassImportPath, NodeClassShortName,
    NodeConfigFilledFromItsDefault, NodeConfigJsonSchema, NodeConfigKeyDescribedBy,
    NodeConfigKeyDescription, NodeDescriptor, NodeKind, NodeReferenceInConfig,
    NodeReferenceToNodeType, NodeRuntime, NodeScheduling, NodeTypeANodeReferenceNames,
    NodeValueLifetime, OutputPortDeclaredNames, PacketsStreamFacts, PortDescriptor, PortFamily,
    PortFamilyDeclaration, PortFamilyDeclarationDirection, PortFamilyLinkEndpoint, PortStreamFacts,
    lint_the_descriptions_of_a_node_type, output_port_codec_resolved_against_config,
    output_port_family_resolved_against_config, output_port_frame_rate_resolved_against_config,
    output_port_names_resolved_against_config, refusal_of_a_link_between_port_families,
    refusal_of_required_names, refuse_audio_window_beside_a_skipping_delivery_profile,
    refuse_stream_facts_of_another_family, schema_carrying_the_declared_description,
};
pub use error::{
    ChannelTrustTierLabel, DeviceMemoryHeapBudgetSource, DeviceMemoryHeapLocality, Error,
    OutputPortNotExposedToTheReader, PortDirection, PrivacyGatedCaptureDevice, Result,
    WhyANodeOfAStreamIsNotRunning,
};
pub use execution::{
    ExecutionConfig, ProcessExecution, ThreadPriority, node_scheduling_loop, run_node_loop,
    thread_runner,
};
pub use graph::graph_snapshot::{
    GraphFragment, GraphOnlyNodesAsDrawn, GraphSnapshot, GraphSnapshotLink, GraphSnapshotNode,
    GraphSnapshotWithGraphOnlyNodesResolved, LinkSetAsideByTheGraphsValidation,
    NodeChangeADrawnLinkBlocks,
};
pub use graph::{
    ComponentMap, ComponentSerializer, DeviceMatchedAudioWindowContractsComponent,
    EXPOSED_NAME_MAXIMUM_LENGTH, ExposedNameCastsToNothingError, FunctionNodeHostComponent,
    FunctionNodePromotedToItsOwnProcessComponent, FunctionNodePromotionOutOfItsReadersIslands,
    FunctionNodeRanCompiledComponent, Graph, GraphEdgeWithComponents, GraphNode, GraphNodePorts,
    GraphNodeWithComponents, GraphState, GraphWeight, HelperProcessRestartRecord,
    HelperProcessRestartsComponent, HowItsHelpersRanAFunctionNode,
    Iceoryx2ServicesHeldOpenForLinkComponent, InputLinkPortRef, InputPortMarker, Link,
    LinkCapacity, LinkCarriedThroughTheFunctionNodePoolComponent, LinkComponent,
    LinkComponentGraphRenders, LinkDirection,
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkState, LinkStateComponent,
    LinkTraversal, LinkTraversalMut, LinkUniqueId, LossCountsOfPortsInThisProcess, NodeComponent,
    NodeComponentGraphRenders, NodeFailureReasonComponent, NodeInstanceComponent, NodeLossCounts,
    NodeMetrics, NodeNotReadyBecauseComponent, NodePauseGateComponent, NodeReadyBarrierComponent,
    NodeReadyBarrierHandle, NodeRunId, NodeState, NodeStateComponent, NodeTraversal,
    NodeTraversalMut, ObservableGraphReadiness, ObservableNodeState, OutOfProcessLinkWireProgress,
    OutOfProcessLinkWireRepliesComponent, OutOfProcessLinkWiringComponent, OutputLinkPortRef,
    OutputPortMarker, PendingDeletionComponent, PortInfo, PortMarker,
    ProcessRaisingBecauseComponent, ReadOfALinkByTheReaderOfAFunctionNodeIsland,
    ShaderNodeFusedGroupComponent, ShaderNodeFusedGroupOfAnIsland, ShutdownChannelComponent,
    StateComponent, StepProgressCallbackInFlight, StepProgressOfOneNode,
    StepProgressOfOneNodeComponent, StepProgressReadingOfOneNode, StorableComponent,
    ThreadHandleComponent, TraversalSource, TraversalSourceMut, cast_exposed_name_to_url_safe,
    default_component_serializers, default_components, graph_fragment_flattening,
    graph_interface_resolution, graph_snapshot, graph_variable_resolution, input, output,
    validate_graph,
};
pub use nodes::{
    __generated_private, Config, ConfigValidationError, ContinuousNode, DynGeneratedNode,
    DynamicNodeConstructorFn, EmptyConfig, GeneratedNode, InstalledModuleOfANodeType, ManualNode,
    NODE_REGISTRY, NodeInstance, NodeInstanceFactory, NodeSpec, NodeTypesOneStreamResolves,
    OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply,
    OutOfProcessLinkWiringEnvelope, ReactiveNode, VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME, traits,
};
pub use rhi::NativeTextureHandle;
#[cfg(test)]
pub(crate) use runtime::TheMachinesShutdownEscalationClearedOnDrop;
#[cfg(any(test, feature = "test-support"))]
pub use runtime::a_project_on_disk_for_a_test;
pub use runtime::{
    ArmedEngineTeardownWatchdog, BoxFuture, CrashOfThePreviousRuntimeRun, CreateProjectRequest,
    CreatedProjectFirstCommit, DescriptionOfTheAbandonedNodeThreads,
    EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED, EveryStreamEndedDuringTheWait,
    FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT, FunctionNodeState,
    FunctionNodeStatesCarriedForEveryStream, FunctionNodeStatesCarriedForOneStream,
    HowALoadedStreamEnded, HowThePreviousRuntimeRunEnded, ImportedProjectPythonSources,
    LoadedStreamHolding, LoadedStreamInThisRuntime, LoadedStreamTag, MachineName,
    NodeDisplayNameAndId, NodeInTheGraph, OpenDescriptorSoftLimitRaise,
    OperationsOnTheStreamsLoadedInThisRuntime, OptionsForLoadingOneStream,
    OutputPortViewerChannelHandOff, OutputPortViewerHeldOpen,
    PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE, PendingStreamActionKind,
    PendingStreamActionOnTheRecord, ProjectCreationQueued,
    ProjectHistoryRecordingFirstPartyPackageMoves, ProjectOpenOrReloadRequest,
    ProjectPausedOrResumed, ProjectVenvSyncPackage, ProjectVenvSyncPackageKind,
    ProjectVenvSyncPackageState, ProjectVenvSyncPhase, ProjectVenvSyncProgress,
    RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM,
    RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL, Runner, RunnerConstructionOptions,
    RuntimeOperations, RuntimeRunInProgressRecord, RuntimeShutdownEscalation, RuntimeStatus,
    RuntimeUniqueId, STREAM_RECORD_SCHEMA_VERSION, ShutdownEscalationOfOneStream, StreamAction,
    StreamActionKind, StreamActionQueued, StreamActionState, StreamActionStep,
    StreamActionWorkingOnAStream, StreamEnvironment, StreamListing, StreamListingState,
    StreamLoadObservingInterrupts, StreamLocation, StreamOfAStreamAction, StreamRecord,
    StreamRecordAsRead, StreamRecordOfAnEarlierRuntime, StreamRecordReadFailure,
    StreamRecordsInTheStateDirectory, StreamReloadAtTheStart, TeardownProgressNoteOfOneStream,
    TheStoreOfEachNodeOfAStream, deregister_a_helper_process_group,
    is_the_machines_shutdown_requested,
    list_a_projects_node_types_and_graph_generators_from_source,
    note_what_the_engine_teardown_is_waiting_on,
    pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread,
    raise_the_open_descriptor_soft_limit_to_its_ceiling, register_a_helper_process_group,
    request_the_shutdown_of_every_loaded_stream, take_the_machines_shutdown_escalation,
    the_machines_shutdown_escalation, what_one_address_chunk_may_be,
};
pub use texture::{Texture, TextureDescriptor, TextureFormat, TextureUsages};
pub use utils::{checksum, compute_json_checksum};

// Narrow re-exports of engine-internal items that have sanctioned
// external consumers. Each line below is a deliberate boundary
// crossing — items not listed here stay engine-internal.

pub mod helper_process_transport {
    pub use super::compiler::compiler_ops::subprocess_bridge::{
        ENGINE_BUILD_ID, SubprocessBridge, SubprocessBridgeLinkDelivery,
    };
}

pub mod node_interpreter {
    pub use super::compiler::compiler_ops::node_interpreter_shutdown_ladder::{
        CALLBACK_RETURN_BUDGET, CHILD_SELF_EXIT_GRACE, TEARDOWN_BUDGET,
    };
    pub use super::compiler::compiler_ops::node_interpreter_spawn_host::{
        NODE_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY,
        NODE_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
        NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE,
        NODE_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
        NODE_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
        SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE, node_interpreter_bootstrap_path,
    };
    pub use super::compiler::compiler_ops::python_node_declaration::{
        AudioWindowFieldRefusal, HowAPythonNodeOfTheTypeRuns, PythonNodeDeclaration,
        read_a_channel_count_or_the_source_spelling,
    };
    pub use streamlib_consumer_rhi::runtime_unit_layout::{
        APP_BUNDLE_CONTENTS_DIRECTORY_NAME, APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME,
        BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, BROWSER_HOST_DIRECTORY_NAME,
        BROWSER_HOST_EXECUTABLE_FILE_NAME, BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME,
        BUNDLED_ICD_MANIFEST_FILE_NAME, FIRST_PARTY_MODULES_DIRECTORY_NAME,
        LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS,
        LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT, RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME,
        app_bundle_contents_directory_holding_the_executable_directory,
        browser_host_executable_beside_the_lend, browser_host_executable_for_the_lend,
        bundled_vulkan_driver_directory_in_the_lend, first_party_modules_directory_beside_the_lend,
        lend_directory_in_the_app_bundle_contents, lend_directory_in_the_runtime_unit,
    };
}
