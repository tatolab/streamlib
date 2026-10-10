// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::graph_change_listener::GraphChangeListener;
use super::processor_interpreter_launch_record::ProcessorInterpreterLaunchRecordOfOneStream;
use super::runtime::EngineResourcesSharedByEveryStream;
use super::stream_actions_of_this_runtime::LoadedStreamHolding;
use super::{
    ArmedTeardownWatchdogOfOneStream, RuntimeOperations, RuntimeStatus,
    ShutdownEscalationOfOneStream, StreamEnvironment, TeardownProgressNoteOfOneStream,
};
use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::compiler::{Compiler, PendingOperation};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use crate::core::context::SoftwareAudioClock;
use crate::core::context::{
    AudioClockConfig, GpuContext, LoadedStreamARuntimeContextBelongsTo, RuntimeContext,
    SharedAudioClock, TimeContext,
};
use crate::core::graph::{
    GraphNodeWithComponents, GraphState, LinkUniqueId, ObservableGraphReadiness,
    ProcessorPauseGateComponent, ProcessorUniqueId, StateComponent, cast_exposed_name_to_url_safe,
};
use crate::core::graph_snapshot::GraphSnapshot;
use crate::core::logging::{LoadedStreamLogRecordsPage, LoadedStreamLogRoute};
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
    pub(crate) fn next_in_this_process() -> Result<Self> {
        static NEXT_LOADED_STREAM_TAG: AtomicU32 = AtomicU32::new(1);
        NEXT_LOADED_STREAM_TAG
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |tag| {
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
    /// How long this stream's teardown has before its watchdog abandons it.
    teardown_watchdog_budget: Duration,
    /// Held for the whole of a start or a stop, so a stop that lands while
    /// the stream starts waits for the start rather than missing its context.
    one_start_or_stop_at_a_time: Mutex<()>,
    /// Set by the one request that starts this stream's shutdown thread.
    shutdown_thread_started: AtomicBool,
    /// Set by whichever of the shutdown thread and the watchdog ends this
    /// stream first.
    end_claimed: AtomicBool,
    the_end_of_this_stream: Arc<TheEndOfOneLoadedStream>,
    /// This stream, for the shutdown thread a `&self` request starts.
    this_stream: Weak<Self>,
    /// Whether the runtime keeps this stream or it lives as long as what
    /// loaded it.
    holding: Mutex<LoadedStreamHolding>,
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
        stream_name: String,
        project_directory: PathBuf,
        stream_environment: Option<StreamEnvironment>,
        teardown_watchdog_budget: Duration,
    ) -> Result<Arc<Self>> {
        let stream_tag = LoadedStreamTag::next_in_this_process()?;
        let log_route = LoadedStreamLogRoute::open_in_project_directory(
            engine_resources_shared_by_every_stream.runtime_id.as_str(),
            &stream_name,
            &project_directory,
        );
        let _entered_this_streams_log_route = log_route.enter_on_this_thread();
        let loaded_stream_identity = LoadedStreamIdentity {
            runtime_id: engine_resources_shared_by_every_stream
                .runtime_id
                .to_string(),
            stream_name: stream_name.clone(),
            stream_tag,
        };
        let node_types_this_stream_resolves = Arc::new(NodeTypesOneStreamResolves::new());
        let compiler = Arc::new(Compiler::new_resolving_node_types_through(Arc::clone(
            &node_types_this_stream_resolves,
        )));
        compiler.scope(|graph, _tx| graph.set_loaded_stream_name(stream_name.clone()));
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
            "Loading the stream `{stream_name}` from {}",
            project_directory.display()
        );

        let shutdown_escalation = ShutdownEscalationOfOneStream::default();
        let this_streams_identity_and_handles = Arc::new(LoadedStreamARuntimeContextBelongsTo {
            identity: loaded_stream_identity.clone(),
            project_directory: Arc::from(project_directory.as_path()),
            shutdown_escalation: shutdown_escalation.clone(),
            teardown_progress_note: TeardownProgressNoteOfOneStream::default(),
            log_route: Arc::clone(&log_route),
        });
        Ok(Arc::new_cyclic(|this_stream| Self {
            processor_interpreter_launch_record: Arc::new(
                ProcessorInterpreterLaunchRecordOfOneStream::new(
                    loaded_stream_identity.clone(),
                    stream_environment,
                    Arc::clone(&node_types_this_stream_resolves),
                    LoadedStreamAHelperProcessBelongsTo {
                        stream_tag,
                        shutdown_escalation: shutdown_escalation.clone(),
                    },
                ),
            ),
            node_types_this_stream_resolves,
            this_streams_identity_and_handles,
            engine_resources_shared_by_every_stream,
            compiler,
            runtime_context,
            status,
            _graph_change_listener: graph_change_listener,
            teardown_watchdog_budget,
            one_start_or_stop_at_a_time: Mutex::new(()),
            shutdown_thread_started: AtomicBool::new(false),
            end_claimed: AtomicBool::new(false),
            the_end_of_this_stream: Arc::default(),
            this_stream: this_stream.clone(),
            holding: Mutex::new(LoadedStreamHolding::Attached),
        }))
    }

    // =========================================================================
    // Identity
    // =========================================================================

    /// The stream's URL-safe cast name, its key in the stream table.
    pub fn stream_name(&self) -> &str {
        &self.this_streams_identity_and_handles.identity.stream_name
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

    /// Whether the runtime keeps this stream or it lives as long as what
    /// loaded it.
    pub fn holding(&self) -> LoadedStreamHolding {
        *self.holding.lock()
    }

    /// Hold this stream as `holding`, set by the load before the stream
    /// enters the table.
    pub(crate) fn hold_as(&self, holding: LoadedStreamHolding) {
        *self.holding.lock() = holding;
    }

    /// The stream's active JSONL log segment, `None` when it writes none.
    pub fn jsonl_log_path(&self) -> Option<&Path> {
        self.log_route().jsonl_log_path()
    }

    /// Where the records this stream's threads emit go.
    pub fn log_route(&self) -> &Arc<LoadedStreamLogRoute> {
        &self.this_streams_identity_and_handles.log_route
    }

    /// This stream's log records numbered after `after`, at most `max_count`
    /// of them, from the most recent its log route holds in memory.
    pub fn log_records_after(&self, after: u64, max_count: usize) -> LoadedStreamLogRecordsPage {
        self.log_route().log_records_after(after, max_count)
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

    /// The runtime context made at this stream's start, `None` before it and
    /// after its stop.
    pub(crate) fn runtime_context_while_started(&self) -> Option<Arc<RuntimeContext>> {
        self.runtime_context.lock().clone()
    }

    fn publish_on_this_streams_topic(&self, event: RuntimeEvent) {
        self.loaded_stream_identity()
            .publish_on_this_streams_topic(event);
    }

    // =========================================================================
    // Lifecycle
    // =========================================================================

    /// Start the stream: the engine's GPU context — created now when no stream
    /// has started — viewed through this stream's surface store and pipeline
    /// cache, its own runtime context, then a commit of every queued change.
    #[tracing::instrument(name = "stream.start", skip_all, fields(stream = %self.stream_name()))]
    pub fn start(self: &Arc<Self>) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let _one_start_or_stop_at_a_time = self.one_start_or_stop_at_a_time.lock();
        if self.this_streams_shutdown_escalation().is_requested() {
            return Err(Error::Runtime(format!(
                "the stream `{}` was not started: its shutdown has been requested, and a stream \
                 that shut down is loaded again rather than restarted",
                self.stream_name()
            )));
        }
        *self.status.lock() = RuntimeStatus::Starting;
        tracing::info!("[start] Starting the stream `{}`", self.stream_name());
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStarting);

        let engine = &self.engine_resources_shared_by_every_stream;
        let gpu = engine.gpu_context_view_for_a_starting_stream(|engine_gpu_context| {
            self.this_streams_view_of_the_engines_gpu_context(engine_gpu_context)
        })?;

        let time = Arc::new(TimeContext::new());
        let audio_clock = an_audio_clock_for_one_stream();

        // The context holds this stream as its runtime operations; the cycle
        // is broken when `stop()` clears the context.
        let this_stream_as_its_runtime_operations: Arc<dyn RuntimeOperations> =
            Arc::clone(self) as Arc<dyn RuntimeOperations>;
        let this_streams_runtime_context = Arc::new(RuntimeContext::new(
            gpu,
            time,
            Arc::clone(&engine.runtime_id),
            Arc::clone(&engine.runtime_name),
            this_stream_as_its_runtime_operations,
            engine.tokio_runtime_variant.handle(),
            engine.iceoryx2_node.clone(),
            Arc::clone(&audio_clock),
            engine.runtime_directory.clone(),
            #[cfg(target_os = "linux")]
            engine.surface_socket_path.clone(),
            #[cfg(target_os = "macos")]
            engine.surface_share_mach_service_rendezvous.clone(),
            Arc::clone(&self.this_streams_identity_and_handles),
        ));
        *self.runtime_context.lock() = Some(Arc::clone(&this_streams_runtime_context));

        this_streams_runtime_context.ensure_platform_ready()?;

        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Running);
        });

        // Started before the commit so the commit compiles.
        *self.status.lock() = RuntimeStatus::Started;

        tracing::info!("[start] Committing pending graph operations");
        self.compiler.commit(&this_streams_runtime_context)?;

        tracing::info!("[start] The stream `{}` started", self.stream_name());
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStarted);
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
            .join(".streamlib")
            .join("cache")
            .join("pipeline-cache");

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let surface_store = Some(
            self.engine_resources_shared_by_every_stream
                .a_connected_surface_store_owned_by(self.surface_owner_key())?,
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let surface_store = None;

        Ok(engine_gpu_context.view_for_one_loaded_stream(surface_store, &pipeline_cache_directory))
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
    #[tracing::instrument(name = "stream.stop", skip_all, fields(stream = %self.stream_name()))]
    pub fn stop(&self) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let _one_start_or_stop_at_a_time = self.one_start_or_stop_at_a_time.lock();
        {
            let mut stream_status = self.status.lock();
            if *stream_status == RuntimeStatus::Stopped {
                tracing::debug!("[stop] Already stopped");
                return Ok(());
            }
            *stream_status = RuntimeStatus::Stopping;
        }

        tracing::info!("[stop] Stopping the stream `{}`", self.stream_name());
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStopping);

        let this_streams_runtime_context_while_started = self.runtime_context.lock().clone();
        let processor_count = self.compiler.scope(|graph, tx| {
            let processor_ids: Vec<ProcessorUniqueId> = graph.traversal().v(()).ids();
            let count = processor_ids.len();
            for proc_id in processor_ids {
                tx.log(PendingOperation::RemoveProcessor(proc_id));
            }
            graph.set_state(GraphState::Idle);
            count
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

            #[cfg(target_os = "macos")]
            crate::core::window_event_pump::release_the_windows_handed_back_while_the_event_pump_was_not_driven();

            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("the audio clock");
            if let Err(e) = this_streams_runtime_context.audio_clock().stop() {
                tracing::warn!("[stop] Failed to stop audio clock: {}", e);
            }

            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("the stream's surface store");
            this_streams_runtime_context.gpu.clear_surface_store();
        }
        self.release_every_surface_this_stream_registered();

        *self.runtime_context.lock() = None;

        *self.status.lock() = RuntimeStatus::Stopped;
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeStopped);
        tracing::info!("[stop] The stream `{}` stopped", self.stream_name());
        processor_removal_outcome
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
                self.stream_name()
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
                self.stream_name()
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
            .name(format!("stream-shutdown-{}", self.stream_name()))
            .spawn(move || this_stream.shut_this_stream_down_under_its_watchdog());
        if let Err(spawn_failure) = started {
            // Released so the next request, or the engine's next walk, retries.
            self.shutdown_thread_started.store(false, Ordering::SeqCst);
            tracing::error!(
                "the shutdown thread of the stream `{}` could not start; the next request \
                 retries it: {spawn_failure}",
                self.stream_name()
            );
        }
    }

    fn shut_this_stream_down_under_its_watchdog(self: Arc<Self>) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let watchdog = ArmedTeardownWatchdogOfOneStream::arm(
            self.stream_name(),
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
                *self.runtime_context.lock() = None;
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
                    self.stream_name()
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
            self.stream_name(),
            self.teardown_watchdog_budget.as_secs_f64(),
        );
        crate::core::runtime::count_threads_abandoned_in_this_process(
            1,
            &format!("the teardown of the stream `{}`", self.stream_name()),
        );
        self.let_go_of_this_stream_then_mark_it_ended(
            HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            },
        );
    }

    /// Close this stream's JSONL log, let go of the stream, then mark it
    /// ended — so a waiter that wakes finds the log on disk whole.
    fn let_go_of_this_stream_then_mark_it_ended(
        self: Arc<Self>,
        how_it_ended: HowALoadedStreamEnded,
    ) {
        self.log_route().close_the_jsonl_log_file();
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
        self.the_end_of_this_stream.how_it_ended.lock().is_some()
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
                    self.stream_name()
                )))
            }
            Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            }) => Err(Error::Runtime(format!(
                "the teardown of the stream `{}` outlived its watchdog while waiting on \
                 {what_its_teardown_was_waiting_on}; the stream was abandoned and unloaded",
                self.stream_name()
            ))),
        }
    }

    /// The lifecycle status of this stream.
    pub fn status(&self) -> RuntimeStatus {
        *self.status.lock()
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

    /// Update a processor's configuration, applied at the next commit.
    pub fn update_processor_config<C: Serialize>(
        &self,
        processor_id: &ProcessorUniqueId,
        config: C,
    ) -> Result<()> {
        let config_json =
            serde_json::to_value(&config).map_err(|e| crate::core::Error::Config(e.to_string()))?;

        self.compiler.scope(|_graph, tx| {
            tx.log(PendingOperation::UpdateProcessorConfig {
                processor_id: processor_id.clone(),
                config_to_apply: config_json,
            });
        });

        self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        Ok(())
    }

    /// Export this stream's graph as JSON: topology, processor states,
    /// metrics and buffer levels.
    pub fn to_json(&self) -> Result<serde_json::Value> {
        let runtime_name = self
            .engine_resources_shared_by_every_stream
            .runtime_name
            .as_str()
            .to_string();
        self.compiler.scope(|graph, _tx| {
            serde_json::to_value(graph.to_graph_response(runtime_name))
                .map_err(|_| Error::GraphError("Unable to serialize graph".into()))
        })
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

            if let Some(state) = node.get::<StateComponent>() {
                state.transition_to(ProcessorState::Paused);
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
                state.transition_to(ProcessorState::Running);
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

    /// Block until every processor in this stream has finished `setup` and
    /// reached `Running`, giving up after `timeout`.
    pub fn wait_until_every_processor_is_running(&self, timeout: Duration) -> Result<()> {
        self.observable_graph_readiness()
            .wait_until_every_processor_is_running(timeout)
    }

    // =========================================================================
    // Whole-stream pause/resume
    // =========================================================================

    /// Pause every processor in this stream.
    pub fn pause(&self) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        *self.status.lock() = RuntimeStatus::Pausing;
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimePausing);

        let processor_ids: Vec<ProcessorUniqueId> = self
            .compiler
            .scope(|graph, _tx| graph.traversal().v(()).ids());

        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.pause_processor(processor_id) {
                tracing::warn!("[{}] Failed to pause: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Paused);
        });

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

    /// Resume every processor in this stream.
    pub fn resume(&self) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        *self.status.lock() = RuntimeStatus::Starting;
        self.publish_on_this_streams_topic(RuntimeEvent::RuntimeResuming);

        let processor_ids: Vec<ProcessorUniqueId> = self
            .compiler
            .scope(|graph, _tx| graph.traversal().v(()).ids());

        let mut failures = Vec::new();
        for processor_id in &processor_ids {
            if let Err(e) = self.resume_processor(processor_id) {
                tracing::warn!("[{}] Failed to resume: {}", processor_id, e);
                failures.push((processor_id.clone(), e));
            }
        }

        self.compiler.scope(|graph, _tx| {
            graph.set_state(GraphState::Running);
        });

        *self.status.lock() = RuntimeStatus::Started;
        if failures.is_empty() {
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeResumed);
        } else {
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeResumeFailed {
                error: format!("{} processor(s) rejected resume", failures.len()),
            });
        }
        Ok(())
    }

    // =========================================================================
    // Graph load
    // =========================================================================

    /// Load `graph` into this stream, which is not in the stream table yet:
    /// every type it names that no native registration holds described in the
    /// stream's own interpreter, the graph validated against what the stream
    /// resolves, each node added under its name, each link connected by name,
    /// and each exposure put at its level in the stream's live exposure map.
    pub(crate) fn load_graph_snapshot_into_this_stream(
        &self,
        graph: &GraphSnapshot,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        self.processor_interpreter_launch_record
            .describe_and_register_every_type_a_load_names(
                graph.nodes.iter().map(|node| &node.processor_type),
                lend_directory,
            )?;
        graph.validate(&self.node_types_this_stream_resolves)?;

        let mut processor_id_by_node_name: HashMap<String, ProcessorUniqueId> = HashMap::new();
        for node in &graph.nodes {
            let added = self.add_processor_reporting_its_name(
                ProcessorSpec::new(node.processor_type.clone(), node.config.clone())
                    .with_display_name(node.name.clone()),
            )?;
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
            )?;
        }

        for exposed in &graph.exposed {
            self.set_output_port_exposure_level(&exposed.node, &exposed.port, exposed.level)?;
        }
        Ok(())
    }
}

/// The message a caught panic carried, when it carried one as text.
fn what_a_panic_said(panic_payload: &(dyn std::any::Any + Send)) -> &str {
    panic_payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic that carried no message")
}

/// An audio clock for one stream, platform-specific for best precision. It
/// paces deviceless audio only, so whatever needs it is what starts it.
fn an_audio_clock_for_one_stream() -> SharedAudioClock {
    let audio_clock_config = AudioClockConfig::default();
    #[cfg(target_os = "macos")]
    {
        tracing::info!(
            "[start] Creating CoreAudioClock (GCD): {}Hz, {} samples/tick",
            audio_clock_config.sample_rate,
            audio_clock_config.buffer_size
        );
        Arc::new(crate::apple::CoreAudioClock::new(audio_clock_config))
    }
    #[cfg(target_os = "linux")]
    {
        tracing::info!(
            "[start] Creating LinuxTimerFdAudioClock: {}Hz, {} samples/tick",
            audio_clock_config.sample_rate,
            audio_clock_config.buffer_size
        );
        Arc::new(crate::linux::LinuxTimerFdAudioClock::new(
            audio_clock_config,
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        tracing::info!(
            "[start] Creating SoftwareAudioClock: {}Hz, {} samples/tick",
            audio_clock_config.sample_rate,
            audio_clock_config.buffer_size
        );
        Arc::new(SoftwareAudioClock::new(audio_clock_config))
    }
}
