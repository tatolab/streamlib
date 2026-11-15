// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Loading streams into the engine, finding a loaded one, and the refusals a load can meet.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::graph::names_listed_for_a_refusal;
use crate::core::runtime::streams_loaded_in_this_runtime::loaded_stream::{
    LoadedStreamInThisRuntime, LoadedStreamNaming,
};
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::LoadedStreamHolding;
use crate::core::{Error, Result};

use super::{
    LOAD_UNDER_ANOTHER_STREAM_ID_IN_CODE, OptionsForLoadingOneStream, Runner,
    StreamLoadObservingInterrupts, a_stream_id_already_loaded_refusal,
    tear_down_a_stream_that_never_entered_the_table, the_cast_stream_id_a_load_names,
    walk_a_stream_being_loaded_to_the_machines_shutdown_level, what_interrupted_a_load,
};

impl Runner {
    /// Load `graph` as a stream: built in a scope of its own and published to
    /// the stream table only once every node and link is in.
    ///
    /// The stream's id — `load_options`' key, else the graph's name, cast
    /// URL-safe — is refused when it casts to nothing, when there is none, and
    /// when a stream of that id is loaded; a graph holding no node loads as an
    /// empty stream. A load refused anywhere after that is torn down through the
    /// stream's own stop and inserts nothing.
    pub fn load_stream_from_graph_snapshot(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let stream = self.build_the_stream_a_load_names(
            load_options,
            LoadedStreamHolding::LivesWithTheHostProcess,
        )?;
        self.load_the_graph_into_the_stream(&stream, graph)?;
        self.insert_a_complete_stream(stream)
    }

    /// Load `graph` as a stream on a thread of its own while this one watches
    /// for a machine shutdown request. Once one is seen the stream being
    /// loaded — which is in no table the machine's walk reaches — is walked to
    /// the machine's level and its describes interrupted. A load a machine
    /// shutdown cut short is abandoned without a refusal.
    pub fn load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
    ) -> Result<StreamLoadObservingInterrupts> {
        self.load_stream_held_as_unless_interrupted(
            graph,
            load_options,
            LoadedStreamHolding::LivesWithTheHostProcess,
            &crate::core::runtime::is_the_machines_shutdown_requested,
        )
    }

    /// [`Self::load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested`],
    /// the stream held as `holding` from the moment it enters the table, the
    /// load abandoned once `is_interrupted` reports true.
    pub(in crate::core::runtime) fn load_stream_held_as_unless_interrupted(
        &self,
        graph: &GraphSnapshot,
        load_options: OptionsForLoadingOneStream,
        holding: LoadedStreamHolding,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamLoadObservingInterrupts> {
        use std::sync::mpsc::RecvTimeoutError;

        use crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL;

        let stream = match self.build_the_stream_a_load_names(load_options, holding) {
            Ok(stream) => stream,
            // Read after the build, so an interrupt landing during it abandons
            // the load rather than refusing it.
            Err(build_refusal) if is_interrupted() => {
                tracing::info!(
                    "{} before the stream loaded, so it was never loaded; building it reported: \
                     {build_refusal}",
                    what_interrupted_a_load()
                );
                return Ok(StreamLoadObservingInterrupts::AbandonedForAnInterrupt);
            }
            Err(build_refusal) => return Err(build_refusal),
        };
        let load_outcome = std::thread::scope(|scope| {
            // Never sent on: the loading thread's end drops it, a panic included.
            let (load_ended_sender, load_ended_receiver) = std::sync::mpsc::channel::<()>();
            let stream_being_loaded = &stream;
            let loading = scope.spawn(move || {
                let load_outcome = self.load_the_graph_into_the_stream(stream_being_loaded, graph);
                drop(load_ended_sender);
                load_outcome
            });
            loop {
                // Repeated, because the load forgets an interrupt that lands
                // before it begins.
                if is_interrupted() {
                    walk_a_stream_being_loaded_to_the_machines_shutdown_level(&stream);
                }
                if let Err(RecvTimeoutError::Disconnected) = load_ended_receiver
                    .recv_timeout(RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL)
                {
                    break;
                }
            }
            loading
                .join()
                .unwrap_or_else(|load_panic| std::panic::resume_unwind(load_panic))
        });

        if !is_interrupted() {
            load_outcome?;
            return self
                .insert_a_complete_stream(stream)
                .map(StreamLoadObservingInterrupts::Loaded);
        }
        match load_outcome {
            Ok(()) => {
                tracing::info!(
                    "{} while the stream `{}` loaded, so it was never loaded or started",
                    what_interrupted_a_load(),
                    stream.stream_id()
                );
                tear_down_a_stream_that_never_entered_the_table(&stream);
            }
            Err(interrupted_load) => tracing::info!(
                "{} while the stream loaded, so it was never started; the interrupted load \
                 reported: {interrupted_load}",
                what_interrupted_a_load()
            ),
        }
        Ok(StreamLoadObservingInterrupts::AbandonedForAnInterrupt)
    }

    /// Load an empty stream a Rust host builds in code, published to the
    /// stream table at once. `load_options` must name it.
    pub fn load_an_empty_stream(
        &self,
        load_options: OptionsForLoadingOneStream,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.load_an_empty_stream_with_its_teardown_watchdog_budget(
            load_options,
            crate::core::runtime::ENGINE_TEARDOWN_WATCHDOG_BUDGET,
        )
    }

    /// Load an empty stream whose teardown watchdog fires after
    /// `teardown_watchdog_budget` rather than the engine's budget.
    #[cfg(test)]
    pub(crate) fn load_an_empty_stream_whose_teardown_watchdog_fires_after(
        &self,
        load_options: OptionsForLoadingOneStream,
        teardown_watchdog_budget: Duration,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.load_an_empty_stream_with_its_teardown_watchdog_budget(
            load_options,
            teardown_watchdog_budget,
        )
    }

    pub(super) fn load_an_empty_stream_with_its_teardown_watchdog_budget(
        &self,
        load_options: OptionsForLoadingOneStream,
        teardown_watchdog_budget: Duration,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.refuse_a_load_while_the_machine_shuts_down(&load_options.stream_id)?;
        let stream_id = the_cast_stream_id_a_load_names(&load_options.stream_id)?;
        let stream = self.build_a_stream(
            stream_id,
            load_options,
            teardown_watchdog_budget,
            LoadedStreamHolding::LivesWithTheHostProcess,
        )?;
        self.insert_a_complete_stream(stream)
    }

    /// The loaded stream whose id is exactly `stream_id`, refused naming the
    /// ids of the streams that are loaded.
    pub fn loaded_stream_of_the_id(
        &self,
        stream_id: &str,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.loaded_stream_of_the_id_when_loaded(stream_id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "no stream with the id `{stream_id}` is loaded in this runtime. Loaded: {}",
                    self.loaded_stream_ids_listed_for_a_refusal()
                ))
            })
    }

    /// The loaded stream whose id is exactly `stream_id`, if one is loaded.
    pub(in crate::core::runtime) fn loaded_stream_of_the_id_when_loaded(
        &self,
        stream_id: &str,
    ) -> Option<Arc<LoadedStreamInThisRuntime>> {
        self.engine_resources_shared_by_every_stream
            .loaded_stream_of_the_id_when_loaded(stream_id)
    }

    /// Every loaded stream, read under one lock of the stream table.
    pub fn every_loaded_stream(&self) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        self.engine_resources_shared_by_every_stream
            .every_loaded_stream()
    }

    /// The ids of the loaded streams, in order.
    pub fn ids_of_the_loaded_streams(&self) -> Vec<String> {
        self.engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock()
            .keys()
            .cloned()
            .collect()
    }

    pub(super) fn build_the_stream_a_load_names(
        &self,
        load_options: OptionsForLoadingOneStream,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.refuse_a_load_while_the_machine_shuts_down(&load_options.stream_id)?;
        let stream_id = the_cast_stream_id_a_load_names(&load_options.stream_id)?;
        self.build_a_stream(
            stream_id,
            load_options,
            crate::core::runtime::ENGINE_TEARDOWN_WATCHDOG_BUDGET,
            holding,
        )
    }

    pub(super) fn build_a_stream(
        &self,
        stream_id: String,
        load_options: OptionsForLoadingOneStream,
        teardown_watchdog_budget: Duration,
        holding: LoadedStreamHolding,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        self.refuse_a_load_once_the_engine_is_shut_down(&stream_id)?;
        self.refuse_a_stream_id_already_loaded(&stream_id)?;
        let project_directory = load_options.resolved_project_directory(&stream_id)?;
        let why_none_of_its_node_code_is_read = load_options.why_none_of_its_node_code_is_read;
        let stream = LoadedStreamInThisRuntime::new(
            Arc::clone(&self.engine_resources_shared_by_every_stream),
            LoadedStreamNaming {
                display_name: load_options
                    .display_name
                    .unwrap_or_else(|| stream_id.clone()),
                stream_id,
                stream_location: load_options.stream_location,
            },
            project_directory,
            load_options.stream_environment,
            load_options.capture_device_access_prompting,
            teardown_watchdog_budget,
            holding,
        )?;
        if let Some(why_none_of_its_node_code_is_read) = why_none_of_its_node_code_is_read {
            stream
                .node_types_this_stream_resolves()
                .hold_every_node_type_as_a_placeholder_reading_no_node_code(
                    why_none_of_its_node_code_is_read,
                );
        }
        Ok(stream)
    }

    /// Load `graph` into `stream`, which is not in the table; a refused load
    /// tears the stream down through its own stop.
    pub(super) fn load_the_graph_into_the_stream(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
        graph: &GraphSnapshot,
    ) -> Result<()> {
        let lend_directory = self
            .engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get();
        if let Err(load_refusal) =
            stream.load_graph_snapshot_into_this_stream(graph, lend_directory)
        {
            tear_down_a_stream_that_never_entered_the_table(stream);
            return Err(load_refusal);
        }
        Ok(())
    }

    pub(super) fn insert_a_complete_stream(
        &self,
        stream: Arc<LoadedStreamInThisRuntime>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let mut streams = self
            .engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock();
        if let Err(refusal) = self.refuse_a_load_once_the_engine_is_shut_down(stream.stream_id()) {
            drop(streams);
            tear_down_a_stream_that_never_entered_the_table(&stream);
            return Err(refusal);
        }
        if let Some(first) = streams.get(stream.stream_id()) {
            let refusal =
                a_stream_id_already_loaded_refusal(first, LOAD_UNDER_ANOTHER_STREAM_ID_IN_CODE);
            drop(streams);
            tear_down_a_stream_that_never_entered_the_table(&stream);
            return Err(refusal);
        }
        streams.insert(stream.stream_id().to_string(), Arc::clone(&stream));
        Ok(stream)
    }

    pub(super) fn refuse_a_stream_id_already_loaded(&self, stream_id: &str) -> Result<()> {
        match self
            .engine_resources_shared_by_every_stream
            .streams_loaded_in_this_runtime
            .lock()
            .get(stream_id)
        {
            Some(first) => Err(a_stream_id_already_loaded_refusal(
                first,
                LOAD_UNDER_ANOTHER_STREAM_ID_IN_CODE,
            )),
            None => Ok(()),
        }
    }

    pub(super) fn refuse_a_load_once_the_engine_is_shut_down(&self, stream_id: &str) -> Result<()> {
        if !self
            .engine_resources_shared_by_every_stream
            .shut_down
            .load(Ordering::SeqCst)
        {
            return Ok(());
        }
        Err(Error::Runtime(format!(
            "the stream `{stream_id}` was not loaded: this runtime's engine has shut down; \
             build a new one to load it"
        )))
    }

    pub(super) fn refuse_a_load_while_the_machine_shuts_down(
        &self,
        requested_stream_id: &str,
    ) -> Result<()> {
        if !crate::core::runtime::is_the_machines_shutdown_requested() {
            return Ok(());
        }
        Err(Error::Runtime(format!(
            "the stream `{requested_stream_id}` was not loaded: the machine is shutting every \
             stream down"
        )))
    }

    pub(in crate::core::runtime) fn loaded_stream_ids_listed_for_a_refusal(&self) -> String {
        names_listed_for_a_refusal(self.ids_of_the_loaded_streams(), "none")
    }
}
