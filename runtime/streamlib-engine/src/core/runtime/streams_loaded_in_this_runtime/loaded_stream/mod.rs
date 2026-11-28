// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::core::compiler::Compiler;
use crate::core::compiler::compiler_ops::node_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::context::{
    CaptureDeviceAccessPrompting, LoadedStreamARuntimeContextBelongsTo, RuntimeContext,
};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::graph::graph_snapshot::GraphOnlyNodesAsDrawn;
use crate::core::graph::input_values::{
    LiveInputValuesKeptInTheProjectStore, LiveInputValuesOfAStream,
};
use crate::core::json_schema::ConfigNodeReferenceEdgeOnTheWire;
use crate::core::logging::{ProjectLogRouteEntry, the_project_log_route_entry_of_this_thread};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::pubsub::{EventListener, LoadedStreamIdentity, PUBSUB, RuntimeEvent, topics};
use crate::core::runtime::node_interpreters::node_interpreter_launch_record::NodeInterpreterLaunchRecordOfOneStream;
use crate::core::runtime::runtime::EngineResourcesSharedByEveryStream;
use crate::core::runtime::streams_loaded_in_this_runtime::graph_change_listener::GraphChangeListener;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::LoadedStreamHolding;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_step_coordinator::{
    StepLedgerReading, StreamStepCoordinatorOfOneStream,
};
use crate::core::runtime::{
    RuntimeStatus, ShutdownEscalationOfOneStream, StreamEnvironment, StreamLocation,
    TeardownProgressNoteOfOneStream,
};
use crate::core::{Error, Result};

mod input_values_set_live;
mod live_graph_edits;
pub(crate) use live_graph_edits::InputValueSetOnANode;
mod live_input_values_at_load_and_stop;
mod shutdown_and_teardown;
mod start_and_stop;

pub(in crate::core::runtime) use live_graph_edits::{
    SavedValuesOfAKeptNode, set_the_saved_values_of_each_kept_node_and_hand_them_over,
};

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

/// One stream loaded in a [`Runner`](crate::core::runtime::Runner): its own graph and
/// compiler, status, graph-change listener, runtime context, node types,
/// shutdown and log, under the engine's shared resources.
pub struct LoadedStreamInThisRuntime {
    /// This stream's identity, project directory, shutdown escalation,
    /// teardown progress note and log route — the one value every runtime
    /// context of this stream is handed.
    this_streams_identity_and_handles: Arc<LoadedStreamARuntimeContextBelongsTo>,
    engine_resources_shared_by_every_stream: Arc<EngineResourcesSharedByEveryStream>,
    pub(crate) compiler: Arc<Compiler>,
    /// This stream's runtime context, made at its start and cleared at its stop.
    pub(crate) runtime_context: Arc<Mutex<Option<Arc<RuntimeContext>>>>,
    pub(crate) status: Arc<Mutex<RuntimeStatus>>,
    /// Subscribed to this stream's topic only; held so the subscription lives
    /// as long as the stream.
    _graph_change_listener: Arc<Mutex<dyn EventListener>>,
    pub(crate) node_interpreter_launch_record: Arc<NodeInterpreterLaunchRecordOfOneStream>,
    node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
    /// The fragment, variable and interface nodes, and what they resolved to, of the
    /// graph this stream runs, so `graph` renders them as drawn.
    pub(in crate::core::runtime) graph_only_nodes_as_drawn: Mutex<GraphOnlyNodesAsDrawn>,
    /// Held by the one writer whose graph edits are open on this stream, from the moment they
    /// open until they close, so two writers' edits never interleave in one commit.
    pub(in crate::core::runtime) one_writer_of_the_graph_at_a_time: Arc<tokio::sync::Mutex<()>>,
    /// The thread that opened the batch of graph edits open on this stream, if one is, which a
    /// start, a stop or a resume on it would wait for itself on.
    pub(in crate::core::runtime) thread_holding_a_graph_edit_batch:
        Mutex<Option<std::thread::ThreadId>>,
    /// Whether its playback is stopped and its graph held to read and edit, until its next
    /// start.
    is_held_stopped: AtomicBool,
    /// How long this stream's teardown has before its watchdog abandons it.
    teardown_watchdog_budget: Duration,
    /// Held for the whole of a start or a stop, so a stop that lands while
    /// the stream starts waits for the start rather than missing its context.
    one_start_or_stop_at_a_time: Mutex<()>,
    /// Held across each change of an input's value, live or saved, from its check to its
    /// announcement, so the values each helper and island holds, the live values `graph`
    /// renders and the order the changes are announced in all end on the last change made; a
    /// connect holds it while it judges and makes its link, so a type choice a set puts back
    /// never meets a link made under the choice it replaced.
    pub(in crate::core::runtime) one_input_value_change_at_a_time: Arc<Mutex<()>>,
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
    /// The task that hands each node what a Value wire into one of its settings carried, or
    /// the setting's own value once its wire is taken off, while the stream is started.
    pub(in crate::core::runtime) hand_over_of_what_wires_into_settings_carry:
        Mutex<Option<tokio::task::JoinHandle<()>>>,
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
        let compiler = Arc::new(Compiler::new_of_a_stream(
            Arc::clone(&node_types_this_stream_resolves),
            LiveInputValuesOfAStream::mirrored_into(
                LiveInputValuesKeptInTheProjectStore::of_the_project_at(
                    Arc::clone(
                        &engine_resources_shared_by_every_stream
                            .project_stores_open_in_this_runtime,
                    ),
                    Arc::from(project_directory.as_path()),
                ),
            ),
        ));
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
                function_node_states:
                    Self::the_function_node_states_of_the_stream_kept_in_its_store(
                        &engine_resources_shared_by_every_stream,
                        &stream_id,
                        &project_directory,
                    ),
                project_stores: Arc::clone(
                    &engine_resources_shared_by_every_stream.project_stores_open_in_this_runtime,
                ),
            }),
            node_interpreter_launch_record: Arc::new(NodeInterpreterLaunchRecordOfOneStream::new(
                loaded_stream_identity.clone(),
                Arc::from(project_directory.as_path()),
                stream_environment,
                Arc::clone(&node_types_this_stream_resolves),
                LoadedStreamAHelperProcessBelongsTo {
                    stream_tag,
                    shutdown_escalation: shutdown_escalation.clone(),
                },
            )),
            node_types_this_stream_resolves,
            graph_only_nodes_as_drawn: Mutex::new(GraphOnlyNodesAsDrawn::default()),
            one_writer_of_the_graph_at_a_time: Arc::new(tokio::sync::Mutex::new(())),
            thread_holding_a_graph_edit_batch: Mutex::new(None),
            is_held_stopped: AtomicBool::new(false),
            hand_over_of_what_wires_into_settings_carry: Mutex::new(None),
            engine_resources_shared_by_every_stream,
            compiler,
            runtime_context,
            status,
            _graph_change_listener: graph_change_listener,
            teardown_watchdog_budget,
            one_start_or_stop_at_a_time: Mutex::new(()),
            one_input_value_change_at_a_time: Arc::default(),
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

    /// The function-node states the stream `stream_id` carries, each kept in its node's own
    /// keys of the store of the project at `project_directory`.
    fn the_function_node_states_of_the_stream_kept_in_its_store(
        engine_resources_shared_by_every_stream: &EngineResourcesSharedByEveryStream,
        stream_id: &str,
        project_directory: &Path,
    ) -> Arc<crate::core::runtime::FunctionNodeStatesCarriedForOneStream> {
        let function_node_states = engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .of_the_stream(stream_id);
        let project_stores = Arc::clone(
            &engine_resources_shared_by_every_stream.project_stores_open_in_this_runtime,
        );
        let project_directory = project_directory.to_path_buf();
        function_node_states.keep_each_state_in(Arc::new(move |node_uuid| {
            Ok(crate::core::context::StoreScopedToThisNode::of_the_node(
                project_stores.the_store_of_the_project_at(&project_directory)?,
                node_uuid,
            ))
        }));
        function_node_states
    }

    pub fn project_directory(&self) -> &Path {
        &self.this_streams_identity_and_handles.project_directory
    }

    pub fn stream_environment(&self) -> Option<&StreamEnvironment> {
        self.node_interpreter_launch_record.stream_environment()
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
    pub(in crate::core::runtime) fn teardown_progress_note(
        &self,
    ) -> &TeardownProgressNoteOfOneStream {
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

    /// This stream's step ledger as it stands: each node's step progress and
    /// each link's bags.
    pub(crate) fn step_ledger_reading(&self) -> StepLedgerReading {
        self.compiler.scope_reading(StepLedgerReading::read_off)
    }

    pub(in crate::core::runtime) fn publish_on_this_streams_topic(&self, event: RuntimeEvent) {
        self.loaded_stream_identity()
            .publish_on_this_streams_topic(event);
    }

    /// This stream, held for a task that runs an edit made from synchronous code.
    pub(in crate::core::runtime) fn this_stream_held_for_a_task(
        &self,
    ) -> crate::core::Result<Arc<Self>> {
        self.this_stream.upgrade().ok_or_else(|| {
            crate::core::Error::Runtime(format!(
                "stream `{}` is no longer loaded",
                self.stream_id()
            ))
        })
    }

    /// The lifecycle status of this stream.
    pub fn status(&self) -> RuntimeStatus {
        *self.status.lock()
    }

    /// How many operations are logged on this stream's graph for its next commit to compile,
    /// for a test that proves an operation logs none.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pending_operations_logged_for_a_test(&self) -> usize {
        self.compiler.logged_pending_operations().len()
    }

    /// How many nodes this stream's live graph holds.
    pub fn node_count(&self) -> usize {
        self.compiler
            .scope_reading(|graph| graph.traversal().v(()).iter().count())
    }

    /// This stream's running graph as drawn, the graph every edit and check reads.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        serde_json::to_value(self.the_running_graph_rendered())
            .map_err(|_| Error::GraphError("Unable to serialize graph".into()))
    }

    /// This stream's graph as `graph` answers it: its running graph, each view node with the
    /// settings its view's manifest declares among its inputs.
    pub fn the_graph_as_answered(&self) -> Result<serde_json::Value> {
        let mut graph_response = self.the_running_graph_rendered();
        crate::core::nodes::render_the_settings_of_each_view_node_into(
            &mut graph_response.nodes,
            &self.node_types_this_stream_resolves,
        );
        serde_json::to_value(graph_response)
            .map_err(|_| Error::GraphError("Unable to serialize graph".into()))
    }

    /// This stream's running graph, rendered as `graph` answers it.
    fn the_running_graph_rendered(&self) -> crate::core::json_schema::GraphResponse {
        #[cfg(any(test, feature = "test-support"))]
        crate::core::runtime::graph_renders_and_commits_counted_for_a_test::count_a_graph_render_of(
            self.loaded_stream_identity(),
        );
        let machine_name = self
            .engine_resources_shared_by_every_stream
            .machine_name
            .name()
            .to_string();
        let (mut graph_response, stateful_function_nodes_by_node_uuid, value_of_each_graph_input) =
            self.compiler.scope(|graph, _tx| {
                let stateful_function_nodes_by_node_uuid: HashMap<uuid::Uuid, Vec<String>> =
                    graph
                        .traversal()
                        .v(())
                        .iter()
                        .filter(|node| {
                            crate::core::compiler::compiler_ops::function_node_island::is_a_stateful_function_node(graph, node)
                        })
                        .map(|node| {
                            (
                                node.node_uuid,
                                crate::core::compiler::compiler_ops::function_node_island::the_state_fields_of_a_function_node(
                                    graph,
                                    &node.node_type,
                                ),
                            )
                        })
                        .collect();
                (
                    graph.to_graph_response(machine_name),
                    stateful_function_nodes_by_node_uuid,
                    the_value_of_each_graph_input(graph),
                )
            });
        graph_response.references = self.reference_edges_the_nodes_hold(&graph_response.nodes);
        self.render_the_state_of_each_stateful_function_node_into(
            &mut graph_response.nodes,
            &stateful_function_nodes_by_node_uuid,
        );
        self.graph_only_nodes_as_drawn
            .lock()
            .render_into(&mut graph_response);
        render_the_value_of_each_graph_input_into(
            &mut graph_response.nodes,
            value_of_each_graph_input,
        );
        graph_response
    }

    /// Render, under each stateful function node's `function_node_state`
    /// component, each field of the state its stream carries now for the
    /// state fields the node declares.
    fn render_the_state_of_each_stateful_function_node_into(
        &self,
        nodes: &mut [crate::core::json_schema::GraphNodeOutput],
        stateful_function_nodes_by_node_uuid: &HashMap<uuid::Uuid, Vec<String>>,
    ) {
        let function_node_states = &self.this_streams_identity_and_handles.function_node_states;
        for node in nodes {
            let Some(state) = node.id.parse::<uuid::Uuid>().ok().and_then(|node_uuid| {
                let state_fields = stateful_function_nodes_by_node_uuid.get(&node_uuid)?;
                function_node_states
                    .the_current_state_of(node_uuid)
                    .filter(|state| &state.state_fields == state_fields)
            }) else {
                continue;
            };
            let state_by_field: serde_json::Map<String, serde_json::Value> = state
                .state_fields
                .into_iter()
                .zip(state.state_values)
                .map(|(state_field, state_value)| (state_field, serde_json::json!(state_value)))
                .collect();
            node.components.function_node_state = Some(state_by_field);
        }
    }

    /// Every reference edge `nodes` hold, under the keys their types mark: in a node's config,
    /// else in its input values, where a connection input holds the node its wire brings. A key
    /// holding a malformed one renders none.
    fn reference_edges_the_nodes_hold(
        &self,
        nodes: &[crate::core::json_schema::GraphNodeOutput],
    ) -> Vec<ConfigNodeReferenceEdgeOnTheWire> {
        nodes
            .iter()
            .flat_map(|node| {
                let keys = crate::core::graph::config_keys_of_a_node_type_holding_a_node_reference(
                    &self.node_types_this_stream_resolves,
                    &node.node_type,
                );
                let held_under_the_keys: serde_json::Map<String, serde_json::Value> = keys
                    .iter()
                    .filter_map(|key| {
                        node.input_values
                            .get(&key.config_key)
                            .map(|value| (key.config_key.clone(), value.clone()))
                    })
                    .collect();
                keys.into_iter()
                    .filter_map(|key| {
                        crate::core::graph::config_node_reference_edges_of_one_node(
                            &node.name,
                            &held_under_the_keys,
                            vec![key],
                        )
                        .ok()
                    })
                    .flatten()
                    .map(|reference| ConfigNodeReferenceEdgeOnTheWire {
                        referenced_node: cast_exposed_name_to_url_safe(
                            &reference.edge.referenced_node,
                        )
                        .map(std::borrow::Cow::into_owned)
                        .unwrap_or(reference.edge.referenced_node),
                        ..reference.edge
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
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

/// What `graph` renders of one top-level `GraphInput`'s `value` input: the value it reads now,
/// set live, saved or nothing, and its saved value.
struct ValueOfAGraphInputRendered {
    current_value: Option<serde_json::Value>,
    saved_value: Option<serde_json::Value>,
}

/// What `graph` renders of the `value` input of each top-level `GraphInput` of `graph`, by node
/// UUID, running or not: the value it reads, a value set live over its saved one; of a bundle,
/// the object of the members it reads.
fn the_value_of_each_graph_input(
    graph: &crate::core::graph::Graph,
) -> HashMap<String, ValueOfAGraphInputRendered> {
    use crate::core::graph::graph_interface_resolution::GRAPH_INTERFACE_VALUE_PORT_NAME;
    use crate::core::graph::input_values::{ValuesABundleInputHolds, the_bundle_members_of};
    graph
        .every_graph_input()
        .filter_map(|node| {
            let port = node.input_port(GRAPH_INTERFACE_VALUE_PORT_NAME)?;
            let current_value = match the_bundle_members_of(port) {
                Some(members) => {
                    let live = graph
                        .live_input_values()
                        .value_of(node.node_uuid, &port.name);
                    ValuesABundleInputHolds {
                        live: live.as_ref(),
                        saved: node.input_values.get(&port.name),
                        input_default: port.pin_default.as_ref(),
                    }
                    .the_bundle_read(members)
                }
                None => crate::core::graph::input_values::the_value_an_input_reads(
                    graph,
                    node,
                    port,
                    &crate::core::graph::WhatTheLinksIntoAnInputFeed::default(),
                )
                .and_then(|read| read.value),
            }
            .filter(|value| !value.is_null());
            Some((
                node.node_uuid.to_string(),
                ValueOfAGraphInputRendered {
                    current_value,
                    saved_value: node.input_values.get(&port.name).cloned(),
                },
            ))
        })
        .collect()
}

/// Render the value of each `GraphInput`'s `value` input: the one it reads under its
/// `graph_interface` component, as `current_value`, and its saved one on its `value` input.
fn render_the_value_of_each_graph_input_into(
    nodes: &mut [crate::core::json_schema::GraphNodeOutput],
    mut value_of_each_graph_input: HashMap<String, ValueOfAGraphInputRendered>,
) {
    use crate::core::graph::graph_interface_resolution::{
        GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_VALUE_PORT_NAME,
    };
    for node in nodes {
        let Some(rendered) = value_of_each_graph_input.remove(&node.id) else {
            continue;
        };
        if let Some(graph_interface) = &mut node.components.graph_interface
            && let Some(current_value) = rendered.current_value
        {
            graph_interface.current_value = Some(current_value);
        }
        match &rendered.saved_value {
            Some(saved_value) => node.input_values.insert(
                GRAPH_INTERFACE_VALUE_PORT_NAME.to_string(),
                saved_value.clone(),
            ),
            None => node.input_values.remove(GRAPH_INTERFACE_VALUE_PORT_NAME),
        };
        node.input_values.remove(GRAPH_INPUT_VALUE_FORMER_NAME);
    }
}
