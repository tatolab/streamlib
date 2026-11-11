// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::graph_change_listener::GraphChangeListener;
use super::live_graph_reconciled_to_a_graph_document::{
    LiveNodeAsTheGraphHoldsIt, RunningLinkBetweenTwoNodesByUuid, the_live_links_by_node_uuid_of,
    the_live_nodes_of,
};
use super::processor_interpreter_launch_record::ProcessorInterpreterLaunchRecordOfOneStream;
use super::runtime::EngineResourcesSharedByEveryStream;
use super::stream_actions_of_this_runtime::LoadedStreamHolding;
use super::stream_step_coordinator::{StepLedgerReading, StreamStepCoordinatorOfOneStream};
use super::{
    ArmedTeardownWatchdogOfOneStream, RuntimeOperations, RuntimeStatus,
    ShutdownEscalationOfOneStream, StreamEnvironment, StreamLocation,
    TeardownProgressNoteOfOneStream,
};
use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::context::{
    CaptureDeviceAccessPrompting, GpuContext, LoadedStreamARuntimeContextBelongsTo, RuntimeContext,
    StreamClock, StreamClockReading, platform_audio_clock_for_one_stream,
};
use crate::core::error::PrivacyGatedCaptureDevice;
use crate::core::graph::{
    GraphNodeWithComponents, GraphState, LinkUniqueId, ObservableGraphReadiness,
    ProcessorPauseGateComponent, ProcessorUniqueId, StateComponent, StepProgressOfOneNodeComponent,
    cast_exposed_name_to_url_safe,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::graph_snapshot::{GraphOnlyNodesAsDrawn, NodeChangeADrawnLinkBlocks};
use crate::core::json_schema::ConfigNodeReferenceEdgeOutput;
use crate::core::logging::{ProjectLogRouteEntry, the_project_log_route_entry_of_this_thread};
use crate::core::media_clock::MediaClock;
use crate::core::processors::{NodeTypesOneStreamResolves, ProcessorSpec, ProcessorState};
use crate::core::pubsub::{
    Event, EventListener, LoadedStreamIdentity, PUBSUB, ProcessorEvent, RuntimeEvent, topics,
};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

/// A loaded stream's process-unique tag: non-zero, never reused in one
/// process, so it tells two streams apart where several runtimes each load a
/// stream of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoadedStreamTag(NonZeroU32);

impl LoadedStreamTag {
    /// A tag no stream of this process has carried; refused once every tag
    /// has been handed out, rather than reusing one.
    pub fn next_in_this_process() -> Result<Self> {
        static NEXT_LOADED_STREAM_TAG: AtomicU32 = AtomicU32::new(1);
        NEXT_LOADED_STREAM_TAG
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |tag| {
                tag.checked_add(1)
            })
            .ok()
            .and_then(NonZeroU32::new)
            .map(Self)
            .ok_or_else(|| {
                Error::Runtime(
                    "no stream can be loaded: every loaded-stream tag of this process has been \
                     handed out"
                        .to_string(),
                )
            })
    }

    /// The tag numbered `tag`, for a test that names its stream's tag.
    #[cfg(test)]
    pub(crate) fn numbered_for_a_test(tag: u32) -> Self {
        Self(NonZeroU32::new(tag).expect("a test's stream tag is non-zero"))
    }

    /// The tag's value.
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

impl std::fmt::Display for LoadedStreamTag {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One stream loaded in a [`Runner`](super::Runner): its own graph and
/// compiler, status, graph-change listener, runtime context, node types,
/// shutdown and log, under the engine's shared resources.
pub struct LoadedStreamInThisRuntime {
    /// This stream's identity, project directory, shutdown escalation,
    /// teardown progress note and log route — the one value every runtime
    /// context of this stream is handed.
    this_streams_identity_and_handles: Arc<LoadedStreamARuntimeContextBelongsTo>,
    engine_resources_shared_by_every_stream: Arc<EngineResourcesSharedByEveryStream>,
    /// Compiles this stream's graph changes into running processors.
    pub(crate) compiler: Arc<Compiler>,
    /// This stream's runtime context, made at its start and cleared at its stop.
    pub(crate) runtime_context: Arc<Mutex<Option<Arc<RuntimeContext>>>>,
    pub(crate) status: Arc<Mutex<RuntimeStatus>>,
    /// Subscribed to this stream's topic only; held so the subscription lives
    /// as long as the stream.
    _graph_change_listener: Arc<Mutex<dyn EventListener>>,
    pub(crate) processor_interpreter_launch_record:
        Arc<ProcessorInterpreterLaunchRecordOfOneStream>,
    node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
    /// The fragment, variable and interface nodes, and what they resolved to, of the
    /// graph this stream runs, so `graph` renders them as drawn.
    pub(super) graph_only_nodes_as_drawn: Mutex<GraphOnlyNodesAsDrawn>,
    /// Whether its playback is stopped and its graph held to read and edit, until its next
    /// start.
    is_held_stopped: AtomicBool,
    /// How long this stream's teardown has before its watchdog abandons it.
    teardown_watchdog_budget: Duration,
    /// Held for the whole of a start or a stop, so a stop that lands while
    /// the stream starts waits for the start rather than missing its context.
    one_start_or_stop_at_a_time: Mutex<()>,
    /// Set by the one request that starts this stream's shutdown thread.
    shutdown_thread_started: AtomicBool,
    /// The step barrier while a stepped stream runs.
    step_coordinator: Mutex<Option<StreamStepCoordinatorOfOneStream>>,
    /// Set by whichever of the shutdown thread and the watchdog ends this
    /// stream first.
    end_claimed: AtomicBool,
    the_end_of_this_stream: Arc<TheEndOfOneLoadedStream>,
    /// This stream, for the shutdown thread a `&self` request starts.
    this_stream: Weak<Self>,
    /// Whether the runtime records this stream or it lives with its host
    /// process.
    holding: LoadedStreamHolding,
    /// The label the stream is shown under, which a rename replaces.
    display_name: RwLock<String>,
    /// The project and stream asset the stream is.
    stream_location: StreamLocation,
}

/// What a loaded stream is keyed by and shown as: its id, its display name,
/// and the project and stream asset it is.
pub(crate) struct LoadedStreamNaming {
    pub(crate) stream_id: String,
    pub(crate) display_name: String,
    pub(crate) stream_location: StreamLocation,
}

/// How a loaded stream ended, and the wait for it. Held apart from the stream
/// so the thread that ends it signals the end after letting go of the stream:
/// a waiter that wakes then holds the last reference, and the engine's final
/// drop never runs on a detached thread.
#[derive(Default)]
struct TheEndOfOneLoadedStream {
    how_it_ended: Mutex<Option<HowALoadedStreamEnded>>,
    has_ended: parking_lot::Condvar,
}

impl TheEndOfOneLoadedStream {
    fn mark(&self, how_it_ended: HowALoadedStreamEnded) {
        *self.how_it_ended.lock() = Some(how_it_ended);
        self.has_ended.notify_all();
    }

    fn is_recorded(&self) -> bool {
        self.how_it_ended.lock().is_some()
    }
}

/// A stream taken out of the stream table, kept by the engine until its end is
/// recorded so a wait for every stream's end sees the stream between the two.
/// The stream is held weakly: the thread ending it still lets go of it before
/// it records the end.
pub(crate) struct LoadedStreamOutOfTheTableUntilItsEndIsRecorded {
    stream: Weak<LoadedStreamInThisRuntime>,
    the_end_of_this_stream: Arc<TheEndOfOneLoadedStream>,
}

impl LoadedStreamOutOfTheTableUntilItsEndIsRecorded {
    /// `stream`, as it leaves the stream table.
    pub(crate) fn leaving_the_table(stream: &Arc<LoadedStreamInThisRuntime>) -> Self {
        Self {
            stream: Arc::downgrade(stream),
            the_end_of_this_stream: Arc::clone(&stream.the_end_of_this_stream),
        }
    }

    /// Whether the stream's end is recorded.
    pub(crate) fn has_its_end_recorded(&self) -> bool {
        self.the_end_of_this_stream.is_recorded()
    }

    /// The stream, while anything still holds it.
    pub(crate) fn stream_while_anything_still_holds_it(
        &self,
    ) -> Option<Arc<LoadedStreamInThisRuntime>> {
        self.stream.upgrade()
    }
}

/// How a loaded stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HowALoadedStreamEnded {
    /// Its teardown finished and every processor was removed.
    Stopped,
    /// Its teardown finished, reporting the failure it met.
    StoppedReportingAFailure(String),
    /// Its teardown outlived its watchdog: its helper process groups were
    /// killed, its teardown thread abandoned, and it was unloaded.
    AbandonedByItsTeardownWatchdog {
        /// What the teardown was still waiting on when the watchdog fired.
        what_its_teardown_was_waiting_on: String,
    },
}

impl LoadedStreamInThisRuntime {
    pub(crate) fn new(
        engine_resources_shared_by_every_stream: Arc<EngineResourcesSharedByEveryStream>,
        naming: LoadedStreamNaming,
        project_directory: PathBuf,
        stream_environment: Option<StreamEnvironment>,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
        teardown_watchdog_budget: Duration,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<Self>> {
        let LoadedStreamNaming {
            stream_id,
            display_name,
            stream_location,
        } = naming;
        let stream_tag = LoadedStreamTag::next_in_this_process()?;
        let resolved_project_directory =
            std::fs::canonicalize(&project_directory).unwrap_or_else(|_| project_directory.clone());
        let action_that_loads_it = the_project_log_route_entry_of_this_thread().filter(|entry| {
            entry.route().project_directory() == Some(resolved_project_directory.as_path())
                && entry.runtime_id() == engine_resources_shared_by_every_stream.runtime_id.as_str()
                && entry
                    .stream_id()
                    .is_none_or(|entered_for| entered_for == stream_id.as_str())
        });
        let log_route = ProjectLogRouteEntry::for_a_stream_of_the_project_in(
            &project_directory,
            engine_resources_shared_by_every_stream.runtime_id.as_str(),
            &stream_id,
            action_that_loads_it
                .as_ref()
                .and_then(|entry| entry.action_id()),
        );
        let _entered_this_streams_log_route = log_route.enter_on_this_thread();
        let loaded_stream_identity = LoadedStreamIdentity {
            runtime_id: engine_resources_shared_by_every_stream
                .runtime_id
                .to_string(),
            stream_id: stream_id.clone(),
            stream_tag,
        };
        let node_types_this_stream_resolves = Arc::new(NodeTypesOneStreamResolves::new());
        let compiler = Arc::new(Compiler::new_resolving_node_types_through(Arc::clone(
            &node_types_this_stream_resolves,
        )));
        compiler.scope(|graph, _tx| graph.set_loaded_stream_id(stream_id.clone()));
        let runtime_context = Arc::new(Mutex::new(None));
        let status = Arc::new(Mutex::new(RuntimeStatus::Initial));
        let graph_change_listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(GraphChangeListener::new(
                Arc::clone(&status),
                Arc::clone(&runtime_context),
                Arc::clone(&compiler),
            )));
        PUBSUB.subscribe(
            &topics::loaded_stream(&loaded_stream_identity),
            Arc::clone(&graph_change_listener),
        )?;
        tracing::info!(
            "Loading the stream `{stream_id}` from {}",
            project_directory.display()
        );

        let shutdown_escalation = ShutdownEscalationOfOneStream::default();
        Ok(Arc::new_cyclic(|this_stream| Self {
            this_streams_identity_and_handles: Arc::new(LoadedStreamARuntimeContextBelongsTo {
                identity: loaded_stream_identity.clone(),
                project_directory: Arc::from(project_directory.as_path()),
                shutdown_escalation: shutdown_escalation.clone(),
                teardown_progress_note: TeardownProgressNoteOfOneStream::default(),
                log_route: Arc::clone(&log_route),
                capture_device_access_prompting,
                function_node_states: engine_resources_shared_by_every_stream
                    .function_node_states_carried_for_every_stream
                    .of_the_stream(&stream_id),
            }),
            processor_interpreter_launch_record: Arc::new(
                ProcessorInterpreterLaunchRecordOfOneStream::new(
                    loaded_stream_identity.clone(),
                    Arc::from(project_directory.as_path()),
                    stream_environment,
                    Arc::clone(&node_types_this_stream_resolves),
                    LoadedStreamAHelperProcessBelongsTo {
                        stream_tag,
                        shutdown_escalation: shutdown_escalation.clone(),
                    },
                ),
            ),
            node_types_this_stream_resolves,
            graph_only_nodes_as_drawn: Mutex::new(GraphOnlyNodesAsDrawn::default()),
            is_held_stopped: AtomicBool::new(false),
            engine_resources_shared_by_every_stream,
            compiler,
            runtime_context,
            status,
            _graph_change_listener: graph_change_listener,
            teardown_watchdog_budget,
            one_start_or_stop_at_a_time: Mutex::new(()),
            shutdown_thread_started: AtomicBool::new(false),
            step_coordinator: Mutex::new(None),
            end_claimed: AtomicBool::new(false),
            the_end_of_this_stream: Arc::default(),
            this_stream: this_stream.clone(),
            holding,
            display_name: RwLock::new(display_name),
            stream_location,
        }))
    }

    // =========================================================================
    // Identity
    // =========================================================================

    /// The stream's id, its key in the stream table and its address.
    pub fn stream_id(&self) -> &str {
        &self.this_streams_identity_and_handles.identity.stream_id
    }

    /// The label the stream is shown under.
    pub fn display_name(&self) -> String {
        self.display_name.read().clone()
    }

    /// Show the stream under `display_name` from now on.
    pub(crate) fn rename(&self, display_name: String) {
        self.compiler
            .scope(|graph, _tx| graph.set_stream_display_name(Some(display_name.clone())));
        *self.display_name.write() = display_name;
    }

    /// The project and stream asset the stream is.
    pub fn stream_location(&self) -> &StreamLocation {
        &self.stream_location
    }

    /// The directory the stream's project lives in.
    pub fn project_directory(&self) -> &Path {
        &self.this_streams_identity_and_handles.project_directory
    }

    /// Where the stream's processor interpreters start, if it was given one.
    pub fn stream_environment(&self) -> Option<&StreamEnvironment> {
        self.processor_interpreter_launch_record
            .stream_environment()
    }

    /// The stream's process-unique tag.
    pub fn stream_tag(&self) -> LoadedStreamTag {
        self.this_streams_identity_and_handles.identity.stream_tag
    }

    /// Whether the runtime records this stream or it lives with its host
    /// process.
    pub fn holding(&self) -> LoadedStreamHolding {
        self.holding
    }

    /// The active JSONL segment of the stream's project log, `None` when it
    /// writes none.
    pub fn jsonl_log_path(&self) -> Option<&Path> {
        self.log_route().jsonl_log_path()
    }

    /// The entry the records this stream's threads emit go through into its
    /// project's log.
    pub fn log_route(&self) -> &Arc<ProjectLogRouteEntry> {
        &self.this_streams_identity_and_handles.log_route
    }

    /// The runtime id, stream name and tag this stream publishes its events
    /// and registers its surfaces under.
    pub fn loaded_stream_identity(&self) -> &LoadedStreamIdentity {
        &self.this_streams_identity_and_handles.identity
    }

    /// How far this stream's own shutdown has gone.
    pub fn this_streams_shutdown_escalation(&self) -> &ShutdownEscalationOfOneStream {
        &self.this_streams_identity_and_handles.shutdown_escalation
    }

    /// What this stream's teardown is waiting on, read by its watchdog.
    fn teardown_progress_note(&self) -> &TeardownProgressNoteOfOneStream {
        &self
            .this_streams_identity_and_handles
            .teardown_progress_note
    }

    /// The owner every surface this stream registers with the engine's
    /// surface-sharing service is registered and released under.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn surface_owner_key(&self) -> String {
        self.loaded_stream_identity().to_string()
    }

    /// The node types this stream resolves: the native ones, then the ones
    /// described in its own interpreter.
    pub fn node_types_this_stream_resolves(&self) -> &Arc<NodeTypesOneStreamResolves> {
        &self.node_types_this_stream_resolves
    }

    pub(crate) fn engine_resources_shared_by_every_stream(
        &self,
    ) -> &Arc<EngineResourcesSharedByEveryStream> {
        &self.engine_resources_shared_by_every_stream
    }

    /// The clock this stream's graph names, started now — published on a board
    /// its helpers read when it is not the machine's own clock.
    fn this_streams_clock_starting_now(
        &self,
        engine: &EngineResourcesSharedByEveryStream,
    ) -> Result<StreamClock> {
        let stream_clock_spec = self.compiler.scope(|graph, _tx| graph.stream_clock_spec());
        stream_clock_spec.refusal_unless_this_runtime_runs_it(self.stream_id())?;
        let stream_clock = StreamClock::starting_now(stream_clock_spec);
        if stream_clock_spec.is_real() {
            return Ok(stream_clock);
        }
        let board_service_name = format!(
            "streamlib/{}/stream-clock/{}",
            self.stream_id(),
            crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix()
        );
        let board = engine
            .iceoryx2_node
            .create_stream_clock_board(&board_service_name, stream_clock.published())?;
        Ok(stream_clock.published_on(board))
    }

    /// The runtime context made at this stream's start, `None` before it and
    /// after its stop.
    #[cfg(test)]
    pub(crate) fn runtime_context_while_started(&self) -> Option<Arc<RuntimeContext>> {
        self.runtime_context.lock().clone()
    }

    /// Run this started stream's scaled clock at `scale` from now on and
    /// publish it to the stream's helpers; its graph keeps the clock it
    /// declared, which the next start runs.
    ///
    /// Refused naming the stream when it is not started, its clock is real or
    /// stepped, or `scale` is one this runtime does not run; and a scale above
    /// 1, which runs the clock in steps, naming a node that runs its own
    /// callbacks when the stream holds one.
    pub fn set_stream_clock_scale(&self, scale: f64) -> Result<StreamClockReading> {
        let Some(this_streams_runtime_context) = self.runtime_context.lock().clone() else {
            return Err(Error::Runtime(format!(
                "the stream `{}` is not started, so it has no clock running to scale",
                self.stream_id()
            )));
        };
        let rescaled = this_streams_runtime_context
            .stream_clock
            .rescale_unless_a_node_holds_it_under_real_time(
                self.stream_id(),
                scale,
                MediaClock::now().as_nanos() as i64,
                || self.a_node_that_paces_its_own_callbacks(),
            )?;
        tracing::info!(
            "the stream `{}` runs its clock at {scale}× from now on",
            self.stream_id()
        );
        Ok(rescaled)
    }

    /// The display name of a node in this stream that runs its own callbacks
    /// inside the step barrier and has not stopped or failed, when one does.
    fn a_node_that_paces_its_own_callbacks(&self) -> Option<String> {
        self.compiler.scope_reading(|graph| {
            graph
                .traversal()
                .v(())
                .iter()
                .find(|node| {
                    node.get::<StepProgressOfOneNodeComponent>()
                        .is_some_and(|step_progress| step_progress.0.paces_its_own_callbacks())
                        && !node.get::<StateComponent>().is_some_and(|state| {
                            matches!(
                                state.current(),
                                ProcessorState::Stopped | ProcessorState::Error
                            )
                        })
                })
                .map(|node| node.display_name.clone())
        })
    }

    /// This stream's step ledger as it stands: each node's step progress and
    /// each link's bags.
    pub(crate) fn step_ledger_reading(&self) -> StepLedgerReading {
        self.compiler.scope_reading(StepLedgerReading::read_off)
    }

    pub(super) fn publish_on_this_streams_topic(&self, event: RuntimeEvent) {
        self.loaded_stream_identity()
            .publish_on_this_streams_topic(event);
    }

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the stream: the engine's GPU context — created now when no stream
    /// has started — viewed through this stream's surface store and pipeline
    /// cache, its own runtime context, then a commit of every queued change.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        self.start_with_the_graph_state(GraphState::Running)
    }

    /// Start the stream as [`Self::start`] does with every processor it
    /// prepares paused, so none processes until [`Self::resume`].
    pub fn start_paused(self: &Arc<Self>) -> Result<()> {
        self.start_with_the_graph_state(GraphState::Paused)
    }

    #[tracing::instrument(name = "stream.start", skip_all, fields(stream = %self.stream_id()))]
    fn start_with_the_graph_state(
        self: &Arc<Self>,
        graph_state_at_the_start: GraphState,
    ) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let _one_start_or_stop_at_a_time = self.one_start_or_stop_at_a_time.lock();
        if self.this_streams_shutdown_escalation().is_requested() {
            return Err(Error::Runtime(format!(
                "the stream `{}` was not started: its shutdown has been requested, and a stream \
                 that shut down is loaded again rather than restarted",
                self.stream_id()
            )));
        }
        self.compiler.scope_reading(
            crate::core::compiler::compiler_ops::input_port_pin_values::refuse_a_function_node_input_with_no_value_as_the_stream_starts,
        )?;
        *self.status.lock() = RuntimeStatus::Starting;
        self.is_held_stopped.store(false, Ordering::Release);
        tracing::info!("[start] Starting the stream `{}`", self.stream_id());
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStarting);

        let engine = &self.engine_resources_shared_by_every_stream;
        let gpu = engine
            .gpu_context_view_for_a_starting_stream(|engine_gpu_context| {
                self.this_streams_view_of_the_engines_gpu_context(engine_gpu_context)
            })
            .map_err(|engine_refusal| {
                Error::EngineResourceRefusedAtAStreamsStart(engine_refusal.to_string())
            })?;

        let stream_clock = Arc::new(self.this_streams_clock_starting_now(engine)?);
        let audio_clock = platform_audio_clock_for_one_stream();

        // The context holds this stream as its runtime operations; the cycle
        // is broken when `stop()` clears the context.
        let this_stream_as_its_runtime_operations: Arc<dyn RuntimeOperations> =
            Arc::clone(self) as Arc<dyn RuntimeOperations>;
        let this_streams_runtime_context = Arc::new(RuntimeContext::new(
            gpu,
            stream_clock,
            Arc::clone(&engine.runtime_id),
            this_stream_as_its_runtime_operations,
            engine.tokio_runtime_variant.handle(),
            engine.iceoryx2_node.clone(),
            Arc::clone(&audio_clock),
            engine.runtime_directory.clone(),
            #[cfg(target_os = "linux")]
            engine.surface_socket_path.clone(),
            #[cfg(target_os = "macos")]
            engine.surface_share_mach_service_rendezvous.clone(),
            Arc::clone(&engine.shared_native_services),
            Arc::clone(&self.this_streams_identity_and_handles),
        ));
        *self.runtime_context.lock() = Some(Arc::clone(&this_streams_runtime_context));

        self.compiler.scope(|graph, _tx| {
            graph.set_state(graph_state_at_the_start);
        });

        // Started before the commit so the commit compiles.
        *self.status.lock() = RuntimeStatus::Started;

        tracing::info!("[start] Committing pending graph operations");
        self.compiler.commit(&this_streams_runtime_context)?;

        if !this_streams_runtime_context.stream_clock.spec().is_real() {
            let earlier_step_coordinator = self.step_coordinator.lock().take();
            if let Some(earlier_step_coordinator) = earlier_step_coordinator {
                earlier_step_coordinator.stop();
            }
            *self.step_coordinator.lock() = Some(StreamStepCoordinatorOfOneStream::start(
                Weak::clone(&self.this_stream),
                self.stream_id(),
                Arc::clone(&this_streams_runtime_context.stream_clock),
            )?);
        }

        tracing::info!("[start] The stream `{}` started", self.stream_id());
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStarted);
        if graph_state_at_the_start == GraphState::Paused {
            *self.status.lock() = RuntimeStatus::Paused;
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimePaused);
        }
        Ok(())
    }

    /// This stream's view of the engine's GPU context: its own surface store,
    /// owned as `<runtime id>/<stream name>#<stream tag>` and connected to the
    /// engine's one surface-sharing service, and its own pipeline-cache
    /// directory.
    fn this_streams_view_of_the_engines_gpu_context(
        &self,
        engine_gpu_context: &GpuContext,
    ) -> Result<GpuContext> {
        let pipeline_cache_directory = self
            .project_directory()
            .join(".tatolab")
            .join("cache")
            .join("pipeline-cache");

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let surface_store = Some(
            self.engine_resources_shared_by_every_stream
                .a_connected_surface_store_owned_by(self.surface_owner_key())?,
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let surface_store = None;

        Ok(engine_gpu_context.view_for_one_loaded_stream(
            self.stream_tag(),
            surface_store,
            &pipeline_cache_directory,
        ))
    }

    /// Stop the stream: every processor removed, its audio clock stopped, its
    /// surface store closed and every surface it registered released, and its
    /// runtime context cleared. Never touches the engine's GPU device, surface
    /// service, iceoryx2 node or tokio.
    ///
    /// Runs every step even when removing the processors failed, and reports
    /// that failure once the rest is down. Idempotent. The stream stays loaded,
    /// its name taken, until [`Runner::unload_stream`] ends it.
    ///
    /// [`Runner::unload_stream`]: crate::core::runtime::Runner::unload_stream
    #[tracing::instrument(name = "stream.stop", skip_all, fields(stream = %self.stream_id()))]
    pub fn stop(&self) -> Result<()> {
        let _one_start_or_stop_at_a_time = self.one_start_or_stop_at_a_time.lock();
        self.stop_holding_the_start_or_stop_lock().0
    }

    /// [`Self::stop`] with the start-or-stop lock already held, answering beside its outcome
    /// the nodes and links the graph held as their removal was queued, in the one graph scope
    /// that queued it, so no edit lands between the two.
    fn stop_holding_the_start_or_stop_lock(
        &self,
    ) -> (
        Result<()>,
        Vec<LiveNodeAsTheGraphHoldsIt>,
        Vec<(LinkUniqueId, RunningLinkBetweenTwoNodesByUuid)>,
    ) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        {
            let mut stream_status = self.status.lock();
            if *stream_status == RuntimeStatus::Stopped {
                tracing::debug!("[stop] Already stopped");
                return (Ok(()), Vec::new(), Vec::new());
            }
            *stream_status = RuntimeStatus::Stopping;
        }

        tracing::info!("[stop] Stopping the stream `{}`", self.stream_id());
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStopping);

        let step_coordinator = self.step_coordinator.lock().take();
        if let Some(step_coordinator) = step_coordinator {
            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("the step coordinator");
            step_coordinator.stop();
        }

        let this_streams_runtime_context_while_started = self.runtime_context.lock().clone();
        let (processor_count, nodes_torn_down, links_torn_down) =
            self.compiler.scope(|graph, tx| {
                let nodes_torn_down = the_live_nodes_of(graph);
                let links_torn_down = the_live_links_by_node_uuid_of(graph);
                let processor_ids: Vec<ProcessorUniqueId> = graph.traversal().v(()).ids();
                let count = processor_ids.len();
                for proc_id in processor_ids {
                    tx.log(PendingOperation::RemoveProcessor(proc_id));
                }
                graph.set_state(GraphState::Idle);
                (count, nodes_torn_down, links_torn_down)
            });
        tracing::info!("[stop] Queued removal of {} processor(s)", processor_count);

        let mut processor_removal_outcome = Ok(());
        if let Some(this_streams_runtime_context) = this_streams_runtime_context_while_started {
            tracing::debug!("[stop] Committing processor teardown");
            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("removing every processor");
            processor_removal_outcome = self.compiler.commit(&this_streams_runtime_context);
            if let Err(removal_failure) = &processor_removal_outcome {
                tracing::error!("[stop] Removing the processors failed: {removal_failure}");
            }
            *self.graph_only_nodes_as_drawn.lock() = GraphOnlyNodesAsDrawn::default();
            self.compiler.scope(|this_streams_graph, _tx| {
                this_streams_graph.set_node_names_the_graph_only_nodes_hold(HashSet::new())
            });

            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("the audio clock");
            if let Err(e) = this_streams_runtime_context.audio_clock().stop() {
                tracing::warn!("[stop] Failed to stop audio clock: {}", e);
            }

            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("the stream's surface store");
            this_streams_runtime_context.gpu.clear_surface_store();

            // After the store closes: its leases, like its helpers', are
            // released as the service sees the connection go.
            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("freeing the stream's shared pool entries");
            this_streams_runtime_context
                .gpu
                .free_every_unheld_shared_pool_entry_the_stream_last_acquired(self.stream_tag());
        }
        self.release_every_surface_this_stream_registered();

        *self.runtime_context.lock() = None;

        *self.status.lock() = RuntimeStatus::Stopped;
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStopped);
        tracing::info!("[stop] The stream `{}` stopped", self.stream_id());
        (processor_removal_outcome, nodes_torn_down, links_torn_down)
    }

    /// Stop this stream's playback and keep its graph: every processor instance, device claim,
    /// surface and the clock torn down as [`Self::stop`] does, then each node and link put back
    /// unbuilt under its UUID, so the stream reads and edits held stopped and its next
    /// [`Self::start`] runs the graph fresh. Reads no graph document and describes no type.
    #[tracing::instrument(name = "stream.stop_playback", skip_all, fields(stream = %self.stream_id()))]
    pub fn stop_playback_keeping_the_graph(&self) -> Result<()> {
        let _one_start_or_stop_at_a_time = self.one_start_or_stop_at_a_time.lock();
        if self.runtime_context.lock().is_none() {
            self.is_held_stopped.store(true, Ordering::Release);
            return Ok(());
        }
        let graph_only_nodes_as_drawn = self.graph_only_nodes_as_drawn.lock().clone();
        let (teardown_outcome, nodes_of_the_graph, links_of_the_graph) =
            self.stop_holding_the_start_or_stop_lock();

        let mut processor_of_each_node_uuid: HashMap<Uuid, ProcessorUniqueId> =
            HashMap::with_capacity(nodes_of_the_graph.len());
        for node in nodes_of_the_graph {
            let spec = ProcessorSpec::new(node.processor_type, node.config)
                .with_display_name(node.display_name)
                .with_pins_and_studio_fields(node.pins, node.studio)
                .with_exposed_config_fields(node.exposed_config_fields)
                .with_node_uuid(node.node_uuid);
            let put_back = self.add_processor_of_a_validated_graph_load(spec)?;
            processor_of_each_node_uuid.insert(node.node_uuid, put_back.processor_id);
        }
        let processor_of = |node_uuid: &Uuid| -> Result<ProcessorUniqueId> {
            processor_of_each_node_uuid
                .get(node_uuid)
                .cloned()
                .ok_or_else(|| {
                    Error::GraphError(format!("the held graph holds no node {node_uuid}"))
                })
        };
        for (_, link) in links_of_the_graph {
            self.connect(
                OutputLinkPortRef::new(processor_of(&link.source_node)?, &link.source_port),
                InputLinkPortRef::new(processor_of(&link.target_node)?, &link.target_port),
            )?;
        }
        let node_names_the_graph_only_nodes_hold = graph_only_nodes_as_drawn.node_names_cast();
        self.compiler.scope(|this_streams_graph, _tx| {
            this_streams_graph
                .set_node_names_the_graph_only_nodes_hold(node_names_the_graph_only_nodes_hold)
        });
        *self.graph_only_nodes_as_drawn.lock() = graph_only_nodes_as_drawn;
        *self.status.lock() = RuntimeStatus::Initial;
        self.is_held_stopped.store(true, Ordering::Release);
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        teardown_outcome
    }

    /// Ask for this stream's shutdown: its level moves to graceful and its own
    /// shutdown thread starts, once. A second request escalates nothing
    /// further.
    pub fn ask_for_this_streams_shutdown(&self, reason: &str) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        if self.this_streams_shutdown_escalation().raise_to_graceful() {
            tracing::info!(
                reason,
                "the shutdown of the stream `{}` was requested",
                self.stream_id()
            );
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeShutdown);
        }
        self.start_this_streams_shutdown_thread_once();
    }

    /// Force this stream's shutdown: every helper's ladder skips to
    /// terminating its process group, and a native processor thread still
    /// inside its callback is abandoned. Starts its shutdown thread if no
    /// request has.
    pub fn force_this_streams_shutdown(&self, reason: &str) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        if self.this_streams_shutdown_escalation().raise_to_forced() {
            tracing::warn!(
                reason,
                "the shutdown of the stream `{}` was forced",
                self.stream_id()
            );
        }
        self.start_this_streams_shutdown_thread_once();
    }

    /// Start the thread that arms this stream's watchdog, stops the stream,
    /// takes it out of the stream table and marks it ended — once.
    fn start_this_streams_shutdown_thread_once(&self) {
        if self.shutdown_thread_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(this_stream) = self.this_stream.upgrade() else {
            return;
        };
        let started = std::thread::Builder::new()
            .name(format!("stream-shutdown-{}", self.stream_id()))
            .spawn(move || this_stream.shut_this_stream_down_under_its_watchdog());
        if let Err(spawn_failure) = started {
            // Released so the next request, or the engine's next walk, retries.
            self.shutdown_thread_started.store(false, Ordering::SeqCst);
            tracing::error!(
                "the shutdown thread of the stream `{}` could not start; the next request \
                 retries it: {spawn_failure}",
                self.stream_id()
            );
        }
    }

    fn shut_this_stream_down_under_its_watchdog(self: Arc<Self>) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let watchdog = ArmedTeardownWatchdogOfOneStream::arm(
            self.stream_id(),
            self.teardown_watchdog_budget,
            self.teardown_progress_note().clone(),
            {
                // Weak, so a disarmed watchdog thread that has not yet seen
                // its disarm holds nothing of the stream or the engine.
                let this_stream = Arc::downgrade(&self);
                move |what_its_teardown_was_waiting_on| {
                    if let Some(this_stream) = this_stream.upgrade() {
                        this_stream.abandon_this_stream_its_teardown_watchdog_fired_on(
                            what_its_teardown_was_waiting_on,
                        );
                    }
                }
            },
        );
        // Caught, so a panicking teardown still leaves the table and is marked
        // ended: its waiters would otherwise wait with nothing to bound them.
        let stop_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.stop()))
            .unwrap_or_else(|teardown_panic| {
                if let Some(this_streams_runtime_context) = self.runtime_context.lock().take() {
                    self.free_the_shared_pool_entries_of_a_stream_whose_teardown_panicked(
                        &this_streams_runtime_context,
                    );
                }
                self.release_every_surface_this_stream_registered();
                Err(Error::Runtime(format!(
                    "its teardown panicked: {}",
                    what_a_panic_said(teardown_panic.as_ref())
                )))
            });
        drop(watchdog);
        if self.end_claimed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.engine_resources_shared_by_every_stream
            .remove_from_the_stream_table(&self);
        let how_it_ended = match stop_outcome {
            Ok(()) => HowALoadedStreamEnded::Stopped,
            Err(stop_failure) => {
                tracing::error!(
                    "the stream `{}` failed to stop: {stop_failure}",
                    self.stream_id()
                );
                HowALoadedStreamEnded::StoppedReportingAFailure(stop_failure.to_string())
            }
        };
        self.let_go_of_this_stream_then_mark_it_ended(how_it_ended);
    }

    /// What this stream's watchdog does on expiry: kill the stream's helper
    /// process groups, count its hung teardown thread as abandoned, and unload
    /// it. Every other stream is left running.
    fn abandon_this_stream_its_teardown_watchdog_fired_on(
        self: Arc<Self>,
        what_its_teardown_was_waiting_on: String,
    ) {
        if self.end_claimed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let killed_helper_process_groups =
            crate::core::runtime::kill_every_registered_helper_process_group_of_one_stream(
                self.stream_tag(),
            );
        self.engine_resources_shared_by_every_stream
            .remove_from_the_stream_table(&self);
        tracing::error!(
            "the teardown of the stream `{}` did not finish within {}s; it was still waiting on \
             {what_its_teardown_was_waiting_on}. Its {killed_helper_process_groups} helper \
             process group(s) were killed, its teardown thread was abandoned, and it was \
             unloaded; every other stream keeps running.",
            self.stream_id(),
            self.teardown_watchdog_budget.as_secs_f64(),
        );
        crate::core::runtime::count_threads_abandoned_in_this_process(
            1,
            &format!("the teardown of the stream `{}`", self.stream_id()),
            self.stream_id(),
        );
        self.let_go_of_this_stream_then_mark_it_ended(
            HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            },
        );
    }

    /// Write this stream's records to its project's log and its function-node
    /// states to its record, let go of the stream, then mark it ended — so a
    /// waiter that wakes finds them on disk whole.
    fn let_go_of_this_stream_then_mark_it_ended(
        self: Arc<Self>,
        how_it_ended: HowALoadedStreamEnded,
    ) {
        self.log_route().write_every_record_it_emitted_and_sync();
        self.engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .record_the_states_of_a_stream_that_ended(self.stream_id());
        let the_end_of_this_stream = Arc::clone(&self.the_end_of_this_stream);
        drop(self);
        the_end_of_this_stream.mark(how_it_ended);
    }

    /// How this stream ended, `None` while it has not.
    pub fn how_this_stream_ended(&self) -> Option<HowALoadedStreamEnded> {
        self.the_end_of_this_stream.how_it_ended.lock().clone()
    }

    /// Whether this stream has ended: shut down and out of the stream table.
    pub fn has_ended(&self) -> bool {
        self.the_end_of_this_stream.is_recorded()
    }

    /// Block for at most `budget` until this stream has ended, saying whether
    /// it has.
    pub(crate) fn wait_for_this_streams_end_within(&self, budget: Duration) -> bool {
        let the_end = &self.the_end_of_this_stream;
        let mut how_it_ended = the_end.how_it_ended.lock();
        if how_it_ended.is_none() {
            the_end.has_ended.wait_for(&mut how_it_ended, budget);
        }
        how_it_ended.is_some()
    }

    /// Free the shared pool entries a stream whose teardown panicked last
    /// acquired, as its stop would have.
    fn free_the_shared_pool_entries_of_a_stream_whose_teardown_panicked(
        &self,
        this_streams_runtime_context: &RuntimeContext,
    ) {
        // The panic may have come from this freeing; a second one must not
        // escape the shutdown thread before the stream is marked ended.
        let freeing_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            this_streams_runtime_context
                .gpu
                .free_every_unheld_shared_pool_entry_the_stream_last_acquired(self.stream_tag())
        }));
        if let Err(freeing_panic) = freeing_outcome {
            tracing::error!(
                "freeing the shared pool entries of the stream `{}` panicked; they are freed \
                 once idle past the window instead: {}",
                self.stream_id(),
                what_a_panic_said(freeing_panic.as_ref())
            );
        }
    }

    /// Release every surface this stream registered with the engine's
    /// surface-sharing service, which keeps a registration from this process
    /// past its connection's close.
    fn release_every_surface_this_stream_registered(&self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.engine_resources_shared_by_every_stream
            .release_every_surface_registered_by(&self.surface_owner_key());
    }

    /// How this stream ended as its waiters report it: `Ok` for a clean stop,
    /// else the failure, and `Ok` while it has not ended.
    pub(crate) fn how_this_stream_ended_as_a_waiter_reports_it(&self) -> Result<()> {
        match self.how_this_stream_ended() {
            None | Some(HowALoadedStreamEnded::Stopped) => Ok(()),
            Some(HowALoadedStreamEnded::StoppedReportingAFailure(stop_failure)) => {
                Err(Error::Runtime(format!(
                    "the stream `{}` failed to stop: {stop_failure}",
                    self.stream_id()
                )))
            }
            Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            }) => Err(Error::Runtime(format!(
                "the teardown of the stream `{}` outlived its watchdog while waiting on \
                 {what_its_teardown_was_waiting_on}; the stream was abandoned and unloaded",
                self.stream_id()
            ))),
        }
    }

    /// The lifecycle status of this stream.
    pub fn status(&self) -> RuntimeStatus {
        *self.status.lock()
    }

    /// Whether this stream is held stopped: loaded as a graph model that reads and edits, with
    /// no processor, runtime context or clock until its start.
    pub fn is_held_stopped(&self) -> bool {
        self.is_held_stopped.load(Ordering::Acquire)
    }

    /// Hold this stream, loaded and never started, stopped until its start.
    pub fn hold_stopped_without_a_start(&self) {
        self.is_held_stopped.store(true, Ordering::Release);
    }

    /// The processors whose threads were abandoned past their shutdown budget
    /// and have not returned since.
    pub fn processor_threads_abandoned_and_still_running(
        &self,
    ) -> Vec<crate::core::runtime::ProcessorDisplayNameAndId> {
        self.compiler
            .processor_threads_abandoned_and_still_running()
    }

    /// Kill any describe this stream is running and refuse every later one —
    /// its host's user interrupted the load that started it.
    pub fn interrupt_every_processor_interpreter_describe(&self) {
        self.processor_interpreter_launch_record
            .interrupt_every_describe();
    }

    // =========================================================================
    // Graph operations
    // =========================================================================

    /// Add a processor to this stream's graph.
    pub fn add_processor(&self, spec: impl Into<ProcessorSpec>) -> Result<ProcessorUniqueId> {
        <Self as RuntimeOperations>::add_processor(self, spec.into())
    }

    /// Remove a processor from this stream's graph.
    pub fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        <Self as RuntimeOperations>::remove_processor(self, processor_id)
    }

    /// Connect two ports in this stream's graph.
    pub fn connect(
        &self,
        from: impl Into<OutputLinkPortRef>,
        to: impl Into<InputLinkPortRef>,
    ) -> Result<LinkUniqueId> {
        <Self as RuntimeOperations>::connect(self, from.into(), to.into())
    }

    /// Disconnect a link in this stream's graph.
    pub fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()> {
        <Self as RuntimeOperations>::disconnect(self, link_id)
    }

    /// Refuse `change` to `processor_id` while a drawn link to a variable or
    /// interface node names it.
    pub(crate) fn refuse_a_node_change_a_drawn_link_names(
        &self,
        processor_id: &ProcessorUniqueId,
        change: NodeChangeADrawnLinkBlocks,
    ) -> Result<()> {
        let Some(node_name) = self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(processor_id)
                .first()
                .map(|node| node.display_name.clone())
        }) else {
            return Ok(());
        };
        self.graph_only_nodes_as_drawn
            .lock()
            .refuse_a_node_change_a_drawn_link_names(&node_name, change)
    }

    /// Rename the node `processor_id` to `requested_name` cast, answering the cast.
    pub(crate) fn rename_node(
        &self,
        processor_id: &ProcessorUniqueId,
        requested_name: &str,
    ) -> Result<String> {
        self.refuse_a_node_change_a_drawn_link_names(
            processor_id,
            NodeChangeADrawnLinkBlocks::Rename,
        )?;
        let renamed = self
            .compiler
            .scope(|graph, _tx| graph.traversal_mut().rename_v(processor_id, requested_name))?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(renamed)
    }

    /// Set the value on input `input` of the node `processor_id`, `None` taking it off.
    pub(crate) fn set_pin_value(
        &self,
        processor_id: &ProcessorUniqueId,
        input: &str,
        value: Option<serde_json::Value>,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| -> Result<()> {
            let node = graph
                .traversal_mut()
                .v(processor_id)
                .first_mut()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;
            if !node.has_input(input) {
                return Err(Error::ProcessorPortNotFound {
                    processor_id: node.display_name.clone(),
                    port_name: input.to_string(),
                    direction: crate::core::PortDirection::Input,
                });
            }
            if value.is_some() && node.is_an_exposed_config_field_input(input) {
                return Err(Error::Configuration(format!(
                    "input `{input}` of `{}` is a config field it exposes, which reads the node's \
                     config while no link feeds it, and takes no pin value. To fix: set `{input}` \
                     in its config",
                    node.display_name
                )));
            }
            match value {
                Some(value) => {
                    crate::core::compiler::compiler_ops::input_port_pin_values::refuse_a_pin_value_a_process_node_cannot_read(
                        node, input, &value,
                    )?;
                    node.pins.insert(input.to_string(), value);
                }
                None => {
                    node.pins.remove(input);
                }
            }
            crate::core::compiler::compiler_ops::function_node_island::hand_each_reader_the_island_a_compile_changed(
                graph,
                &[],
            )?;
            crate::core::compiler::compiler_ops::input_port_pin_values::hand_each_helper_the_pin_values_its_input_ports_read(
                graph,
            )
        })?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    /// Take `exposed_config_fields` as the config fields the node `processor_id` takes as
    /// inputs, in place of those it took; a field still linked is refused being taken off.
    pub(crate) fn set_exposed_config_fields(
        &self,
        processor_id: &ProcessorUniqueId,
        exposed_config_fields: Vec<String>,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| -> Result<()> {
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;
            let declared_ports: Vec<crate::core::graph::PortInfo> = node
                .ports
                .inputs
                .iter()
                .filter(|port| !node.is_an_exposed_config_field_input(&port.name))
                .chain(&node.ports.outputs)
                .cloned()
                .collect();
            let exposed_config_field_input_ports =
                crate::core::graph::the_input_ports_of_the_exposed_config_fields(
                    &node.display_name,
                    &node.processor_type,
                    &self.node_types_this_stream_resolves,
                    &declared_ports,
                    &exposed_config_fields,
                )?;
            let fields_still_linked: Vec<String> = graph
                .traversal()
                .e(())
                .iter()
                .filter(|link| link.to_port().processor_id() == processor_id)
                .map(|link| link.to_port().port_name().to_string())
                .filter(|port| {
                    node.is_an_exposed_config_field_input(port)
                        && !exposed_config_fields.contains(port)
                })
                .collect();
            if !fields_still_linked.is_empty() {
                return Err(Error::Configuration(format!(
                    "`{}` cannot stop exposing {} as inputs while a link feeds each. To fix: \
                     disconnect it first",
                    node.display_name,
                    fields_still_linked
                        .iter()
                        .map(|field| format!("`{field}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            graph
                .traversal_mut()
                .v(processor_id)
                .first_mut()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?
                .replace_the_exposed_config_fields(
                    exposed_config_fields,
                    exposed_config_field_input_ports,
                );
            crate::core::compiler::compiler_ops::function_node_island::hand_each_reader_the_island_a_compile_changed(
                graph,
                &[],
            )?;
            crate::core::compiler::compiler_ops::input_port_pin_values::hand_each_helper_the_config_fields_its_node_exposes_as_inputs(
                graph,
            )
        })?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    /// Replace the studio fields of the node `processor_id` whole.
    pub(crate) fn set_studio_fields(
        &self,
        processor_id: &ProcessorUniqueId,
        studio: serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        self.compiler.scope(|graph, _tx| -> Result<()> {
            graph
                .traversal_mut()
                .v(processor_id)
                .first_mut()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?
                .studio = studio;
            Ok(())
        })?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    /// Update a processor's configuration, applied at the next commit.
    pub fn update_processor_config<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        self.log_a_processor_config_update_for_the_next_commit(processor_id, config)?;
        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    /// Check `config` against everything that holds a processor's configuration and log it
    /// for the next commit, publishing nothing.
    pub(super) fn log_a_processor_config_update_for_the_next_commit<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        let config_json =
            serde_json::to_value(&config).map_err(|e| crate::core::Error::Config(e.to_string()))?;

        self.compiler.scope(|graph, tx| -> Result<()> {
            if let Some(node) = graph.traversal().v(processor_id).first() {
                crate::core::graph::refuse_a_config_its_node_types_config_schema_refuses(
                    &node.display_name,
                    &node.processor_type,
                    &self.node_types_this_stream_resolves,
                    &config_json,
                )?;
            }
            crate::core::graph::refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse(
                graph,
                processor_id,
                &config_json,
            )?;
            crate::core::graph::refuse_a_live_config_naming_a_node_the_graph_does_not_hold(
                graph,
                &self.node_types_this_stream_resolves,
                processor_id,
                &config_json,
            )?;
            if let Some(node) = graph.traversal().v(processor_id).first() {
                self.graph_only_nodes_as_drawn
                    .lock()
                    .interface_as_drawn
                    .refuse_a_config_update_that_breaks_a_declared_port(
                        &self.node_types_this_stream_resolves,
                        &crate::core::graph_snapshot::GraphSnapshotNode {
                            id: Some(node.node_uuid),
                            name: node.display_name.clone(),
                            processor_type: node.processor_type.clone(),
                            config: node.config.clone().unwrap_or_default(),
                            pins: node.pins.clone(),
                            exposed_config_fields: node.exposed_config_fields.clone(),
                            studio: node.studio.clone(),
                            fragment: None,
                        },
                        &config_json,
                    )?;
            }
            tx.log(PendingOperation::UpdateProcessorConfig {
                processor_id: processor_id.clone(),
                config_to_apply: config_json,
            });
            Ok(())
        })
    }

    /// How many nodes this stream's live graph holds.
    pub fn node_count(&self) -> usize {
        self.compiler
            .scope(|graph, _tx| graph.traversal().v(()).iter().count())
    }

    /// Export this stream's graph as JSON: topology, processor states,
    /// metrics and buffer levels.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        let machine_name = self
            .engine_resources_shared_by_every_stream
            .machine_name
            .name()
            .to_string();
        let (mut graph_response, stateful_function_nodes_by_node_uuid, current_value_of_each_graph_input) =
            self.compiler.scope(|graph, _tx| {
                let stateful_function_nodes_by_node_uuid: HashMap<String, (String, Vec<String>)> =
                    graph
                        .traversal()
                        .v(())
                        .iter()
                        .filter(|node| {
                            matches!(
                                node.node_kind(),
                                Some(crate::core::descriptors::NodeKind::Function { .. })
                            )
                        })
                        .map(|node| {
                            (
                                node.node_uuid.to_string(),
                                (
                                    node.display_name.clone(),
                                    crate::core::compiler::compiler_ops::function_node_island::the_state_fields_of_a_function_node(
                                        graph,
                                        &node.processor_type,
                                    ),
                                ),
                            )
                        })
                        .filter(|(_, (_, state_fields))| !state_fields.is_empty())
                        .collect();
                (
                    graph.to_graph_response(machine_name),
                    stateful_function_nodes_by_node_uuid,
                    the_current_value_of_each_running_graph_input(graph),
                )
            });
        graph_response.references = self.reference_edges_the_configs_of(&graph_response.nodes);
        self.render_the_state_of_each_stateful_function_node_into(
            &mut graph_response.nodes,
            &stateful_function_nodes_by_node_uuid,
        );
        self.graph_only_nodes_as_drawn
            .lock()
            .render_into(&mut graph_response);
        render_the_current_value_of_each_graph_input_into(
            &mut graph_response.nodes,
            current_value_of_each_graph_input,
        );
        serde_json::to_value(graph_response)
            .map_err(|_| Error::GraphError("Unable to serialize graph".into()))
    }

    /// Render, under each stateful function node's `function_node_state`
    /// component, each field of the state its stream carries now for the
    /// state fields the node declares.
    fn render_the_state_of_each_stateful_function_node_into(
        &self,
        nodes: &mut [crate::core::json_schema::ProcessorNodeOutput],
        stateful_function_nodes_by_node_uuid: &HashMap<String, (String, Vec<String>)>,
    ) {
        let function_node_states = &self.this_streams_identity_and_handles.function_node_states;
        for node in nodes {
            let Some(state) = stateful_function_nodes_by_node_uuid.get(&node.id).and_then(
                |(function_node_name, state_fields)| {
                    function_node_states
                        .the_current_state_of(function_node_name)
                        .filter(|state| &state.state_fields == state_fields)
                },
            ) else {
                continue;
            };
            let state_by_field: serde_json::Map<String, serde_json::Value> = state
                .state_fields
                .into_iter()
                .zip(state.state_values)
                .map(|(state_field, state_value)| (state_field, serde_json::json!(state_value)))
                .collect();
            node.components.insert(
                "function_node_state".to_string(),
                serde_json::Value::Object(state_by_field),
            );
        }
    }

    /// Every reference edge the configs of `nodes` hold, under the keys their
    /// types mark; a config holding a malformed one renders none for that key.
    fn reference_edges_the_configs_of(
        &self,
        nodes: &[crate::core::json_schema::ProcessorNodeOutput],
    ) -> Vec<ConfigNodeReferenceEdgeOutput> {
        nodes
            .iter()
            .flat_map(|node| {
                let Some(config) = &node.config else {
                    return Vec::new();
                };
                crate::core::graph::config_keys_of_a_node_type_holding_a_node_reference(
                    &self.node_types_this_stream_resolves,
                    &node.processor_type,
                )
                .into_iter()
                .filter_map(|key| {
                    crate::core::graph::config_node_reference_edges_of_one_node(
                        &node.name,
                        config,
                        vec![key],
                    )
                    .ok()
                })
                .flatten()
                .map(|reference| ConfigNodeReferenceEdgeOutput {
                    referenced_node: cast_exposed_name_to_url_safe(&reference.edge.referenced_node)
                        .map(std::borrow::Cow::into_owned)
                        .unwrap_or(reference.edge.referenced_node),
                    ..reference.edge
                })
                .collect()
            })
            .collect()
    }

    // =========================================================================
    // Per-processor pause/resume
    // =========================================================================

    /// Pause a specific processor.
    pub fn pause_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        self.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                Error::Runtime(format!(
                    "Processor '{}' has no ProcessorPauseGate",
                    processor_id
                ))
            })?;

            if pause_gate.is_paused() {
                return Ok(());
            }

            pause_gate
                .clone_inner()
                .store(true, std::sync::atomic::Ordering::Release);

            // One still in setup takes the gate's state when its setup returns.
            if let Some(state) = node.get::<StateComponent>() {
                state.transition_to_only_from(ProcessorState::Running, ProcessorState::Paused);
            }

            let event = Event::processor(processor_id, ProcessorEvent::Paused);
            PUBSUB.publish(&event.topic(), &event);

            tracing::info!("[{}] Processor paused", processor_id);
            Ok(())
        })
    }

    /// Resume a specific processor.
    pub fn resume_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        self.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node.get::<ProcessorPauseGateComponent>().ok_or_else(|| {
                Error::Runtime(format!(
                    "Processor '{}' has no ProcessorPauseGate",
                    processor_id
                ))
            })?;

            if !pause_gate.is_paused() {
                return Ok(());
            }

            pause_gate
                .clone_inner()
                .store(false, std::sync::atomic::Ordering::Release);

            if let Some(state) = node.get::<StateComponent>() {
                state.transition_to_only_from(ProcessorState::Paused, ProcessorState::Running);
            }

            let event = Event::processor(processor_id, ProcessorEvent::Resumed);
            PUBSUB.publish(&event.topic(), &event);

            tracing::info!("[{}] Processor resumed", processor_id);
            Ok(())
        })
    }

    /// Whether a specific processor is paused.
    pub fn is_processor_paused(&self, processor_id: &ProcessorUniqueId) -> Result<bool> {
        self.compiler.scope(|graph, _tx| {
            let node = graph
                .traversal()
                .v(processor_id)
                .first()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            let pause_gate = node
                .get::<ProcessorPauseGateComponent>()
                .ok_or_else(|| Error::ProcessorNotFound(processor_id.to_string()))?;

            Ok(pause_gate.is_paused())
        })
    }

    // =========================================================================
    // Graph readiness
    // =========================================================================

    /// Take hold of every processor's state, to wait on without the graph.
    ///
    /// The graph lock is released before anything blocks on what it hands
    /// back: the transitions being waited for are made by processor threads
    /// that need that lock.
    pub fn observable_graph_readiness(&self) -> ObservableGraphReadiness {
        ObservableGraphReadiness::new(self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(())
                .iter()
                .filter_map(|node| {
                    Some((node.id.clone(), node.get::<StateComponent>()?.clone_inner()))
                })
                .collect()
        }))
    }

    /// Each capture device a node of this stream refused at `setup()` because
    /// nobody had asked the user for its access, once each, in order.
    pub fn capture_devices_whose_access_was_never_asked_for(
        &self,
    ) -> Vec<PrivacyGatedCaptureDevice> {
        let mut capture_devices: Vec<PrivacyGatedCaptureDevice> =
            self.compiler.scope(|graph, _tx| {
                graph
                    .traversal()
                    .v(())
                    .iter()
                    .filter_map(|node| {
                        node.get::<StateComponent>()?
                            .clone_inner()
                            .capture_device_access_never_asked_for()
                    })
                    .collect()
            });
        capture_devices.sort();
        capture_devices.dedup();
        capture_devices
    }

    /// Block until every processor in this stream has finished `setup` and
    /// reached `Running`, giving up after `timeout`.
    pub fn wait_until_every_processor_is_running(&self, timeout: Duration) -> Result<()> {
        self.observable_graph_readiness()
            .wait_until_every_processor_is_running(timeout)
    }

    // =========================================================================
    // Whole-stream pause/resume
    // =========================================================================

    /// Pause every processor in this stream; one a compile prepares while it
    /// is paused starts paused.
    pub fn pause(&self) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        *self.status.lock() = RuntimeStatus::Pausing;
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimePausing);

        let processor_ids =
            self.set_the_graph_state_and_take_every_prepared_processor_id(GraphState::Paused);

        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.pause_processor(processor_id) {
                tracing::warn!("[{}] Failed to pause: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        *self.status.lock() = RuntimeStatus::Paused;
        if failures.is_empty() {
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimePaused);
        } else {
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimePauseFailed {
                error: format!("{} processor(s) rejected pause", failures.len()),
            });
        }
        Ok(())
    }

    /// Set this stream's graph state to `graph_state` and take the id of every
    /// processor a compile prepared, under one graph lock: a processor prepared
    /// after it takes its pause gate from `graph_state`, and one prepared
    /// before it is in the ids.
    fn set_the_graph_state_and_take_every_prepared_processor_id(
        &self,
        graph_state: GraphState,
    ) -> Vec<ProcessorUniqueId> {
        self.compiler.scope(|graph, _tx| {
            graph.set_state(graph_state);
            graph
                .traversal()
                .v(())
                .iter()
                .filter(|node| node.has::<ProcessorPauseGateComponent>())
                .map(|node| node.id.clone())
                .collect()
        })
    }

    /// Resume every processor in this stream, then compile every graph change
    /// logged while it was paused.
    pub fn resume(&self) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        *self.status.lock() = RuntimeStatus::Starting;
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeResuming);

        let processor_ids =
            self.set_the_graph_state_and_take_every_prepared_processor_id(GraphState::Running);

        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.resume_processor(processor_id) {
                tracing::warn!("[{}] Failed to resume: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        *self.status.lock() = RuntimeStatus::Started;
        let committed = match self.runtime_context.lock().clone() {
            Some(this_streams_runtime_context) => {
                self.compiler.commit(&this_streams_runtime_context)
            }
            None => Ok(()),
        };
        if failures.is_empty() {
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeResumed);
        } else {
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeResumeFailed {
                error: format!("{} processor(s) rejected resume", failures.len()),
            });
        }
        committed
    }

    // =========================================================================
    // Graph load
    // =========================================================================

    /// Load `graph` into this stream, which is not in the stream table yet:
    /// every type it names that no native registration holds described in the
    /// stream's own interpreter, the graph validated against what the stream
    /// resolves, each node added under its name, and each link connected by
    /// name.
    pub(crate) fn load_graph_snapshot_into_this_stream(
        &self,
        graph: &GraphSnapshot,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        graph
            .clock
            .refusal_unless_this_runtime_runs_it(self.stream_id())?;
        let graph_whose_every_node_holds_a_uuid =
            graph.with_a_uuid_minted_for_each_node_holding_none();
        self.processor_interpreter_launch_record
            .describe_and_register_every_type_a_load_names(
                &graph_whose_every_node_holds_a_uuid
                    .node_types_its_running_nodes_name_at_every_level(),
                lend_directory,
            )?;
        self.processor_interpreter_launch_record
            .describe_and_register_the_node_types_installed_modules_declare(lend_directory);
        let resolution = graph_whose_every_node_holds_a_uuid
            .with_graph_only_nodes_resolved(&self.node_types_this_stream_resolves)?;
        let graph = &resolution.graph_the_runtime_runs;
        resolution.validate(&self.node_types_this_stream_resolves)?;
        let node_names_the_graph_only_nodes_hold =
            resolution.graph_only_nodes_as_drawn.node_names_cast();
        self.compiler.scope(|this_streams_graph, _tx| {
            this_streams_graph.set_stream_clock_spec(graph.clock);
            this_streams_graph.set_stream_asset(graph.asset);
            this_streams_graph.set_stream_display_name(graph.display_name.clone());
            this_streams_graph
                .set_node_names_the_graph_only_nodes_hold(node_names_the_graph_only_nodes_hold);
        });

        let mut processor_id_by_node_name: HashMap<String, ProcessorUniqueId> = HashMap::new();
        for node in &graph.nodes {
            let mut spec = ProcessorSpec::new(node.processor_type.clone(), node.config.clone())
                .with_display_name(node.name.clone())
                .with_pins_and_studio_fields(node.pins.clone(), node.studio.clone())
                .with_exposed_config_fields(node.exposed_config_fields.clone());
            if let Some(node_uuid) = node.id {
                spec = spec.with_node_uuid(node_uuid);
            }
            let added = self.add_processor_of_a_validated_graph_load(spec)?;
            processor_id_by_node_name.insert(added.name, added.processor_id);
        }
        let processor_id_of = |node: &str| -> Result<ProcessorUniqueId> {
            let cast = cast_exposed_name_to_url_safe(node)?;
            processor_id_by_node_name
                .get(cast.as_ref())
                .cloned()
                .ok_or_else(|| Error::GraphError(format!("the graph holds no node `{node}`")))
        };

        for link in &graph.links {
            self.connect(
                OutputLinkPortRef::new(processor_id_of(&link.source.node)?, &link.source.port),
                InputLinkPortRef::new(processor_id_of(&link.target.node)?, &link.target.port),
            )
            .map_err(|refusal| {
                match resolution
                    .graph_only_nodes_as_drawn
                    .variables_as_drawn
                    .links_resolved_from_a_variable
                    .iter()
                    .find(|resolved| resolved.link == *link)
                {
                    Some(resolved) => Error::GraphError(format!(
                        "`GetVariable` node `{}` reads the variable `{}`, which holds \
                         `{}/{}`, into `{}/{}`, and that link is refused: {refusal}",
                        resolved.get_variable_node,
                        resolved.variable,
                        link.source.node,
                        link.source.port,
                        link.target.node,
                        link.target.port
                    )),
                    None => refusal,
                }
            })?;
        }
        *self.graph_only_nodes_as_drawn.lock() = resolution.graph_only_nodes_as_drawn;
        Ok(())
    }
}

/// The message a caught panic carried, when it carried one as text.
pub(crate) fn what_a_panic_said(panic_payload: &(dyn std::any::Any + Send)) -> &str {
    panic_payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic that carried no message")
}

/// The value each running top-level `GraphInput` carries now, by its node's UUID.
fn the_current_value_of_each_running_graph_input(
    graph: &crate::core::graph::Graph,
) -> HashMap<String, serde_json::Value> {
    use crate::core::graph::GraphNodeWithComponents;
    graph
        .traversal()
        .v(())
        .iter()
        .filter(|node| {
            node.processor_type.as_str()
                == crate::core::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE
        })
        .filter_map(|node| {
            let processor_instance =
                node.get::<crate::core::graph::ProcessorInstanceComponent>()?;
            let mut processor_instance = processor_instance.0.lock();
            let current_value = processor_instance
                .as_any_mut()
                .downcast_mut::<crate::core::processors::NodeWhoseOutputsAClientWritesProcessor>()?
                .current_value_of_the_output(
                    crate::core::graph_interface_resolution::GRAPH_INTERFACE_VALUE_PORT_NAME,
                )?
                .clone();
            Some((node.node_uuid.to_string(), current_value))
        })
        .collect()
}

/// Render each `GraphInput`'s current value under its `graph_interface` component, as
/// `current_value`.
fn render_the_current_value_of_each_graph_input_into(
    nodes: &mut [crate::core::json_schema::ProcessorNodeOutput],
    mut current_value_of_each_graph_input: HashMap<String, serde_json::Value>,
) {
    for node in nodes {
        let Some(current_value) = current_value_of_each_graph_input.remove(&node.id) else {
            continue;
        };
        if let Some(serde_json::Value::Object(graph_interface)) = node
            .components
            .get_mut(crate::core::graph_interface_resolution::GRAPH_INTERFACE_COMPONENT_KEY)
        {
            graph_interface.insert(
                GRAPH_INPUT_CURRENT_VALUE_COMPONENT_FIELD.to_string(),
                current_value,
            );
        }
    }
}

/// The field of a `GraphInput`'s `graph_interface` component holding the value it carries now:
/// the last a client wrote, else its `default`.
pub const GRAPH_INPUT_CURRENT_VALUE_COMPONENT_FIELD: &str = "current_value";
