// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::compiler::PendingOperation;
use crate::core::context::{
    GpuContext, RuntimeContext, StreamClock, StreamClockReading,
    platform_audio_clock_for_one_stream,
};
use crate::core::error::PrivacyGatedCaptureDevice;
use crate::core::graph::graph_snapshot::GraphOnlyNodesAsDrawn;
use crate::core::graph::{
    GraphNodeWithComponents, GraphState, ObservableGraphReadiness, ProcessorPauseGateComponent,
    ProcessorUniqueId, StateComponent, StepProgressOfOneNodeComponent,
};
use crate::core::media_clock::MediaClock;
use crate::core::processors::{ProcessorSpec, ProcessorState};
use crate::core::pubsub::{Event, PUBSUB, ProcessorEvent, RuntimeEvent};
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::{
    LiveLinkAsGiven, LiveNodeAsTheGraphHoldsIt, the_live_links_as_given_by_node_uuid_of,
    the_live_nodes_as_given_of,
};
use crate::core::runtime::runtime::EngineResourcesSharedByEveryStream;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_step_coordinator::StreamStepCoordinatorOfOneStream;
use crate::core::runtime::{RuntimeOperations, RuntimeStatus};
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

impl LoadedStreamInThisRuntime {
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
        if let Some(why_it_cannot_play) =
            self.why_its_graph_as_held_keeps_this_stream_from_playing()
        {
            return Err(Error::Runtime(format!(
                "the stream `{}` was not started: {why_it_cannot_play}",
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
        if graph_state_at_the_start == GraphState::Paused {
            stream_clock.hold_where_it_stands_for_a_pause();
        }
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
        Vec<LiveLinkAsGiven>,
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
                let nodes_torn_down = the_live_nodes_as_given_of(graph);
                let links_torn_down = the_live_links_as_given_by_node_uuid_of(graph);
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
        for LiveLinkAsGiven {
            link_as_given: link,
            ..
        } in links_of_the_graph
        {
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

    /// Whether this stream is held stopped: loaded as a graph model that reads and edits, with
    /// no processor, runtime context or clock until its start.
    pub fn is_held_stopped(&self) -> bool {
        self.is_held_stopped.load(Ordering::Acquire)
    }

    /// Hold this stream, loaded and never started, stopped until its start.
    pub fn hold_stopped_without_a_start(&self) {
        self.is_held_stopped.store(true, Ordering::Release);
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
        if let Some(this_streams_runtime_context) = self.runtime_context.lock().clone() {
            this_streams_runtime_context
                .stream_clock
                .hold_where_it_stands_for_a_pause();
        }

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

        if let Some(this_streams_runtime_context) = self.runtime_context.lock().clone() {
            this_streams_runtime_context
                .stream_clock
                .carry_on_from_where_the_pause_held_it();
        }
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
}
