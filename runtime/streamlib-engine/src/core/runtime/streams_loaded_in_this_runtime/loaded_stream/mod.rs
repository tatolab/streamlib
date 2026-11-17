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
use crate::core::json_schema::ConfigNodeReferenceEdgeOutput;
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

mod live_graph_edits;
mod shutdown_and_teardown;
mod start_and_stop;

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

    pub(crate) fn the_handles_a_runtime_context_of_this_stream_belongs_to(
        &self,
    ) -> Arc<LoadedStreamARuntimeContextBelongsTo> {
        Arc::clone(&self.this_streams_identity_and_handles)
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

    /// This stream's step ledger as it stands: each node's step progress and
    /// each link's bags.
    pub(crate) fn step_ledger_reading(&self) -> StepLedgerReading {
        self.compiler.scope_reading(StepLedgerReading::read_off)
    }

    pub(in crate::core::runtime) fn publish_on_this_streams_topic(&self, event: RuntimeEvent) {
        self.loaded_stream_identity()
            .publish_on_this_streams_topic(event);
    }

    /// The lifecycle status of this stream.
    pub fn status(&self) -> RuntimeStatus {
        *self.status.lock()
    }

    /// How many nodes this stream's live graph holds.
    pub fn node_count(&self) -> usize {
        self.compiler
            .scope(|graph, _tx| graph.traversal().v(()).iter().count())
    }

    pub fn to_json(&self) -> Result<serde_json::Value> {
        let machine_name = self
            .engine_resources_shared_by_every_stream
            .machine_name
            .name()
            .to_string();
        let (mut graph_response, stateful_function_nodes_by_node_uuid, current_value_of_each_graph_input) =
            self.compiler.scope(|graph, _tx| {
                let stateful_function_nodes_by_node_uuid: HashMap<uuid::Uuid, Vec<String>> =
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
                                node.node_uuid,
                                crate::core::compiler::compiler_ops::function_node_island::the_state_fields_of_a_function_node(
                                    graph,
                                    &node.node_type,
                                ),
                            )
                        })
                        .filter(|(_, state_fields)| !state_fields.is_empty())
                        .collect();
                (
                    graph.to_graph_response(machine_name),
                    stateful_function_nodes_by_node_uuid,
                    the_current_value_of_each_graph_input(graph, &|node_uuid| {
                        self.this_streams_identity_and_handles
                            .the_store_of_the_node(node_uuid)
                    }),
                )
            });
        graph_response.references = self.reference_edges_the_configs_of(&graph_response.nodes);
        self.render_the_state_of_each_stateful_function_node_into(
            &mut graph_response.nodes,
            &stateful_function_nodes_by_node_uuid,
        );
        self.graph_only_nodes_as_drawn
            .lock()
            .render_into(&mut graph_response, &self.node_types_this_stream_resolves);
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
        nodes: &[crate::core::json_schema::GraphNodeOutput],
    ) -> Vec<ConfigNodeReferenceEdgeOutput> {
        nodes
            .iter()
            .flat_map(|node| {
                let Some(config) = &node.config else {
                    return Vec::new();
                };
                crate::core::graph::config_keys_of_a_node_type_holding_a_node_reference(
                    &self.node_types_this_stream_resolves,
                    &node.node_type,
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
/// The value each top-level `GraphInput` of `graph` carries, by node UUID: a running one's
/// current value, and a stopped one's value at its next Play.
pub(in crate::core::runtime) fn the_current_value_of_each_graph_input(
    graph: &crate::core::graph::Graph,
    the_store_of_the_node: &dyn Fn(
        uuid::Uuid,
    ) -> Result<crate::core::context::StoreScopedToThisNode>,
) -> HashMap<String, serde_json::Value> {
    use crate::core::graph::GraphNodeWithComponents;
    use crate::core::nodes::NodeWhoseOutputsAClientWrites;
    graph
        .traversal()
        .v(())
        .iter()
        .filter(|node| {
            node.node_type.as_str()
                == crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE
        })
        .filter_map(|node| {
            let current_value = match node.get::<crate::core::graph::NodeInstanceComponent>() {
                Some(node_instance) => node_instance
                    .0
                    .lock()
                    .as_any_mut()
                    .downcast_mut::<NodeWhoseOutputsAClientWrites>()?
                    .current_value_of_the_output(
                        crate::core::graph::graph_interface_resolution::GRAPH_INTERFACE_VALUE_PORT_NAME,
                    )?
                    .clone(),
                None => {
                    let store = the_store_of_the_node(node.node_uuid)
                        .inspect_err(|not_opened| {
                            tracing::warn!(
                                "`GraphInput` node `{}` does not run and its project's store \
                                 did not open, so `graph` shows its default: {not_opened}",
                                node.display_name
                            )
                        })
                        .ok();
                    NodeWhoseOutputsAClientWrites::the_value_a_stopped_top_level_graph_input_carries(
                        node, store,
                    )
                    .ok()??
                }
            };
            Some((node.node_uuid.to_string(), current_value))
        })
        .collect()
}

/// Render each `GraphInput`'s current value under its `graph_interface` component, as
/// `current_value`.
fn render_the_current_value_of_each_graph_input_into(
    nodes: &mut [crate::core::json_schema::GraphNodeOutput],
    mut current_value_of_each_graph_input: HashMap<String, serde_json::Value>,
) {
    for node in nodes {
        let Some(current_value) = current_value_of_each_graph_input.remove(&node.id) else {
            continue;
        };
        if let Some(serde_json::Value::Object(graph_interface)) = node
            .components
            .get_mut(crate::core::graph::graph_interface_resolution::GRAPH_INTERFACE_COMPONENT_KEY)
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
pub(crate) const GRAPH_INPUT_CURRENT_VALUE_COMPONENT_FIELD: &str = "current_value";
