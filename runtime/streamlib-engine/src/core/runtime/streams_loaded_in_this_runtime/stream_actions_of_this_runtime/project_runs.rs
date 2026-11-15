// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// Read the request's project — its project file and each stream asset's graph document
    /// — make its `.venv` unless its interpreter runs, then record, load and start each stream
    /// asset under the id its project and stream asset give it.
    ///
    /// A load runs no project code beyond describing its node types, and is never refused for
    /// what a document or the project's environment holds: a document that does not read, or
    /// whose load is refused, records its stream failed with the reason. A stream whose start
    /// is refused, whose project's `.venv` cannot be made, or whose node code does not describe
    /// a type its graph names is recorded failed and held stopped from its document. A record
    /// of the project whose stream asset no
    /// document holds any more is unloaded and forgotten.
    ///
    /// A load of a stream already recorded replaces it under the same id, keeping its display
    /// name: the running stream is unloaded once the new record is written, and never put back.
    /// Refused, with nothing changed, for a directory holding no project, an id another stream
    /// holds, a second copy of a project another directory holds, and on a runtime given no
    /// state directory.
    ///
    /// Run for the stream action `progress` reports when one runs it: the
    /// project's log entered with the action's id, each step reported, and a
    /// refusal written to the project's log.
    pub(super) fn run_the_project_reporting_to(
        &self,
        request: ProjectStreamsToReadAndLoad,
        progress: Option<&StreamActionProgress<'_>>,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamRunOutcome> {
        let _in_the_projects_log = self.enter_the_log_of_the_project_a_load_takes_in(
            &request.project_directory,
            progress.map(|progress| progress.action_id.as_str()),
        );
        let run_outcome = self.run_the_project_in_its_log(request, progress, is_interrupted);
        match (&run_outcome, &_in_the_projects_log) {
            (Ok(run_outcome), Some(_)) => self.log_each_stream_a_run_failed_in_its_projects_log(
                run_outcome,
                progress.map(|progress| progress.action_id.as_str()),
            ),
            (Err(refusal), _) => {
                if let Some(progress) = progress
                    && !progress.saw_its_cancel()
                {
                    tracing::warn!("the stream action {} failed: {refusal}", progress.action_id);
                }
            }
            (Ok(_), None) => {}
        }
        if let Some(progress) = progress
            && progress.saw_its_cancel()
        {
            tracing::info!(
                "the stream action {} was cancelled at the step it was on; the streams it had not \
                 reached stay as recorded",
                progress.action_id
            );
        }
        run_outcome
    }

    /// Write why each stream `run_outcome` failed to its project's log, stamped
    /// with the stream and the action that ran it.
    pub(super) fn log_each_stream_a_run_failed_in_its_projects_log(
        &self,
        run_outcome: &StreamRunOutcome,
        action_id: Option<&str>,
    ) {
        for stream in &run_outcome.streams {
            let StreamStateAfterItsRun::Failed { failed_because } = &stream.state else {
                continue;
            };
            ProjectLogRouteEntry::for_a_stream_of_the_project_in(
                &stream.stream_location.project_directory,
                self.engine_resources_shared_by_every_stream
                    .runtime_id
                    .as_str(),
                &stream.stream_id,
                action_id,
            )
            .run_entered(|| {
                tracing::error!("the stream `{}` failed: {failed_because}", stream.stream_id)
            });
        }
    }

    /// Run `request` inside the project's log its caller entered, reporting each step to `progress`.
    pub(super) fn run_the_project_in_its_log(
        &self,
        request: ProjectStreamsToReadAndLoad,
        progress: Option<&StreamActionProgress<'_>>,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamRunOutcome> {
        let report_the_step = |step: StreamActionStep| {
            if let Some(progress) = progress {
                progress.is_on(step);
            }
        };
        let ProjectStreamsToReadAndLoad {
            project_directory,
            only_the_stream_assets,
            playback_after_the_load,
            keeps_each_loaded_stream_whose_node_code_is_unchanged,
        } = request;
        let Some(stream_records) = self.stream_records() else {
            return Err(Error::Configuration(format!(
                "the stream in {} was not run: this runtime records no stream, because its \
                 host gave it no state directory to record them in; run it in `tatolabd`",
                project_directory.display()
            )));
        };
        report_the_step(StreamActionStep::ReadingTheGraphDocuments);
        self.move_each_tatolab_ops_node_of_the_project_to_core_nodes(&project_directory);
        let project = ProjectAsALoadReadsIt::read(&project_directory)?;
        let every_stream_asset: Vec<StreamAssetRead> = project
            .stream_documents
            .iter()
            .map(|stream_document| project.stream_asset_read_from(stream_document))
            .collect();
        refuse_streams_whose_graph_documents_share_a_node_uuid(
            &project.project_directory,
            project.stream_documents.iter().zip(
                every_stream_asset
                    .iter()
                    .map(|stream_asset| &stream_asset.graph_or_why_none),
            ),
        )?;
        let mut stream_assets: Vec<StreamAssetRead> = every_stream_asset
            .into_iter()
            .filter(|stream_asset| {
                only_the_stream_assets
                    .as_ref()
                    .is_none_or(|only| only.contains(&stream_asset.stream_location.stream_asset))
            })
            .collect();
        {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            for stream_asset in &stream_assets {
                self.refuse_a_run_of_a_stream_whose_id_another_holds(
                    stream_records,
                    &project,
                    stream_asset,
                )?;
            }
        }
        if let Some(progress) = progress {
            progress.works_on(
                stream_assets
                    .iter()
                    .map(|stream_asset| {
                        let recorded = self.the_record_of_the_stream_at(
                            stream_records,
                            &stream_asset.stream_location,
                        );
                        StreamOfAStreamAction {
                            stream_id: recorded.as_ref().map_or_else(
                                || {
                                    self.the_stream_id_a_run_records(
                                        stream_records,
                                        &project,
                                        stream_asset,
                                    )
                                },
                                |record| record.stream_id.clone(),
                            ),
                            display_name: stream_asset.display_name.clone(),
                            stream_location: Some(stream_asset.stream_location.clone()),
                            state: None,
                        }
                    })
                    .collect(),
            );
        }

        report_the_step(StreamActionStep::MakingTheVenv);
        let stream_environment = StreamEnvironment {
            project_directory: project.project_directory.clone(),
            interpreter: the_projects_venv_interpreter(&project_directory),
        };
        let mut why_the_projects_venv_was_not_made = None;
        match self.make_the_projects_venv(
            &project.project_directory,
            is_interrupted,
            progress.map(StreamActionProgress::report_of_the_next_venv_sync),
        ) {
            Ok(()) => {
                if let Some(lend_directory) = self
                    .engine_resources_shared_by_every_stream
                    .processor_interpreter_lend_directory
                    .get()
                {
                    warn_of_what_binds_the_project_to_one_floor(
                        &stream_environment,
                        lend_directory,
                        is_interrupted,
                    );
                }
            }
            Err(venv_not_made) => {
                let failed_because = match venv_not_made {
                    ProjectVenvNotMade::Refused(refusal) => return Err(refusal),
                    ProjectVenvNotMade::Failed { failed_because } => failed_because,
                };
                why_the_projects_venv_was_not_made = Some(failed_because);
            }
        }

        report_the_step(StreamActionStep::Starting);
        let node_code_of_the_project =
            ProjectNodeCodeContentHash::of_the_project_at(&project.project_directory);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let stream_assets_the_project_holds_as_it_starts =
            ProjectAsALoadReadsIt::read(&project_directory)?.stream_assets();
        stream_assets.retain(|stream_asset| {
            stream_assets_the_project_holds_as_it_starts
                .contains(&stream_asset.stream_location.stream_asset)
        });
        let mut streams_loaded_or_failed = Vec::with_capacity(stream_assets.len());
        for stream_asset in stream_assets {
            if progress.is_some_and(StreamActionProgress::is_cancelled) {
                return Ok(StreamRunOutcome {
                    project_directory,
                    streams: streams_loaded_or_failed,
                });
            }
            let stream_id =
                self.the_stream_id_a_run_records(stream_records, &project, &stream_asset);
            if keeps_each_loaded_stream_whose_node_code_is_unchanged
                && let Some(kept) = self.keep_the_loaded_stream_reconciled_to_its_graph_document(
                    &stream_id,
                    &stream_asset,
                    node_code_of_the_project,
                )
            {
                streams_loaded_or_failed.push(kept);
                continue;
            }
            let loaded_or_failed = self.record_and_load_one_stream_a_run_read(
                stream_records,
                StreamARunRead {
                    stream_id,
                    stream_asset,
                    stream_environment: &stream_environment,
                    playback_after_the_load,
                    why_the_projects_venv_was_not_made: why_the_projects_venv_was_not_made
                        .as_deref(),
                    is_interrupted,
                },
            )?;
            self.remember_the_node_code_the_stream_was_described_from(
                &loaded_or_failed,
                node_code_of_the_project,
            );
            streams_loaded_or_failed.push(loaded_or_failed);
        }
        self.forget_every_record_of_the_project_whose_stream_asset_is_gone(
            stream_records,
            &project,
            &stream_assets_the_project_holds_as_it_starts,
        );
        Ok(StreamRunOutcome {
            project_directory,
            streams: streams_loaded_or_failed,
        })
    }

    /// Keep the loaded stream `stream_id`, its live graph reconciled to the graph document
    /// `stream_asset` was read from, when the project's node code is `node_code_of_the_project`,
    /// as it was when the stream was loaded, and its record is neither failed nor of another
    /// location; `None` when it must be loaded afresh, a document its live graph does not
    /// reconcile to among them.
    pub(super) fn keep_the_loaded_stream_reconciled_to_its_graph_document(
        &self,
        stream_id: &str,
        stream_asset: &StreamAssetRead,
        node_code_of_the_project: ProjectNodeCodeContentHash,
    ) -> Option<StreamLoadedOrFailedByARun> {
        let described_from = self
            .stream_actions
            .node_code_each_loaded_stream_was_described_from
            .lock()
            .get(stream_id)
            .copied();
        if described_from != Some(node_code_of_the_project)
            || stream_asset.graph_or_why_none.is_err()
        {
            return None;
        }
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id)?;
        let record = self.current_stream_record_of_the_id(stream_id).ok()??;
        if record.is_failed() || record.location != stream_asset.stream_location {
            return None;
        }
        let (_, stream_document) =
            the_graph_document_of_the_stream_at(&stream_asset.stream_location).ok()?;
        let graph_document: serde_json::Value =
            serde_json::from_slice(stream_document.bytes.as_ref().ok()?).ok()?;
        if let Err(refusal) = loaded
            .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(graph_document)
        {
            tracing::info!(
                "the stream `{stream_id}` is loaded again from its graph document, which its \
                 live graph does not reconcile to: {refusal}"
            );
            return None;
        }
        if loaded.display_name() != stream_asset.display_name {
            loaded.rename(stream_asset.display_name.clone());
        }
        tracing::info!(
            "the stream `{stream_id}` is kept loaded and reconciled to its graph document, its \
             project's node code as it was at its load"
        );
        let stream_tag = loaded.stream_tag();
        let node_count = loaded.node_count();
        let state = if loaded.is_held_stopped() {
            StreamStateAfterItsRun::HeldStopped {
                stream_tag,
                node_count,
            }
        } else {
            StreamStateAfterItsRun::Running {
                stream_tag,
                node_count,
            }
        };
        Some(StreamLoadedOrFailedByARun::of_the_record(
            record,
            stream_asset.display_name.clone(),
            state,
            true,
        ))
    }

    /// Remember that the stream a load took in was described from `node_code_of_the_project`,
    /// or forget it for a stream the load recorded failed.
    pub(super) fn remember_the_node_code_the_stream_was_described_from(
        &self,
        loaded_or_failed: &StreamLoadedOrFailedByARun,
        node_code_of_the_project: ProjectNodeCodeContentHash,
    ) {
        let mut node_code_each_loaded_stream_was_described_from = self
            .stream_actions
            .node_code_each_loaded_stream_was_described_from
            .lock();
        match loaded_or_failed.state {
            StreamStateAfterItsRun::Failed { .. } => {
                node_code_each_loaded_stream_was_described_from.remove(&loaded_or_failed.stream_id);
            }
            StreamStateAfterItsRun::Running { .. } | StreamStateAfterItsRun::HeldStopped { .. } => {
                node_code_each_loaded_stream_was_described_from
                    .insert(loaded_or_failed.stream_id.clone(), node_code_of_the_project);
            }
        }
    }

    /// Show the loaded stream of `project`'s stream asset `stream_asset` under
    /// `display_name`, which its graph document now writes; a stream not loaded reads it from
    /// its document at its next load or listing. Answers the stream's id, `None` when none is
    /// loaded.
    pub fn show_the_stream_of_a_stream_asset_under(
        &self,
        project: Uuid,
        stream_asset: Uuid,
        display_name: &str,
    ) -> Option<String> {
        let loaded = self.every_loaded_stream().into_iter().find(|loaded| {
            let location = loaded.stream_location();
            location.project == project && location.stream_asset == stream_asset
        })?;
        loaded.rename(display_name.to_string());
        Some(loaded.stream_id().to_string())
    }

    /// Queue the load of `stream_asset`, a stream asset just created in the project in
    /// `project_directory`, alone on the project's stream action queue, playing while the
    /// project plays and held stopped while its playback is stopped; `None`, with nothing
    /// queued, when the project is not open.
    pub fn load_the_stream_of_a_created_stream_asset(
        &self,
        project_directory: &Path,
        stream_asset: Uuid,
    ) -> Option<StreamActionQueued> {
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return None;
        }
        let playback_after_the_load =
            self.the_playback_a_load_of_the_open_project_keeps(&project_directory);
        Some(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            Some(BTreeSet::from([stream_asset])),
            playback_after_the_load,
            StreamActionKind::Load,
        ))
    }

    /// Bring the streams the open project in `project_directory` runs as its own in line with
    /// what its streams place, as its graph documents are on disk now: each stream another
    /// stream of the project places is unloaded and forgotten, and the load of each stream asset
    /// placed nowhere that the runtime records no stream of is queued. `None` when no load is
    /// queued.
    pub fn bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionQueued> {
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return None;
        }
        let stream_records = self.stream_records()?;
        let project = match ProjectAsALoadReadsIt::read(&project_directory) {
            Ok(project) => project,
            Err(not_read) => {
                tracing::warn!(
                    project_directory = %project_directory.display(),
                    %not_read,
                    "the streams the project runs as its own were not brought in line with what \
                     its streams place"
                );
                return None;
            }
        };
        let stream_assets_the_project_runs_as_its_own = project.stream_assets();
        let stream_assets_the_runtime_records = {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            self.forget_every_record_of_the_project_whose_stream_asset_is_gone(
                stream_records,
                &project,
                &stream_assets_the_project_runs_as_its_own,
            );
            self.current_records_of_the_project(&project_directory)
                .into_iter()
                .map(|record| record.location.stream_asset)
                .collect::<BTreeSet<Uuid>>()
        };
        let stream_assets_to_load: BTreeSet<Uuid> = stream_assets_the_project_runs_as_its_own
            .into_iter()
            .filter(|stream_asset| !stream_assets_the_runtime_records.contains(stream_asset))
            .collect();
        if stream_assets_to_load.is_empty() {
            return None;
        }
        let playback_after_the_load =
            self.the_playback_a_load_of_the_open_project_keeps(&project_directory);
        Some(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            Some(stream_assets_to_load),
            playback_after_the_load,
            StreamActionKind::Load,
        ))
    }

    /// The id a run records `stream_asset` of `project` under: its record's, else the one its
    /// project's name, its name and its two UUIDs give it.
    pub(super) fn the_stream_id_a_run_records(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project: &ProjectAsALoadReadsIt,
        stream_asset: &StreamAssetRead,
    ) -> String {
        self.the_record_of_the_stream_at(stream_records, &stream_asset.stream_location)
            .map_or_else(
                || {
                    stream_asset
                        .stream_location
                        .stream_id_named(&project.project_file.name, &stream_asset.stream_name)
                },
                |record| record.stream_id,
            )
    }

    /// Refuse a run of `stream_asset` of `project` whose id another stream holds, loaded or
    /// recorded, or that another copy of its project holds.
    pub(super) fn refuse_a_run_of_a_stream_whose_id_another_holds(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project: &ProjectAsALoadReadsIt,
        stream_asset: &StreamAssetRead,
    ) -> Result<()> {
        let stream_id = self.the_stream_id_a_run_records(stream_records, project, stream_asset);
        self.the_recorded_stream_a_run_of_its_location_replaces(
            &stream_id,
            &stream_asset.stream_location,
        )
        .map(|_| ())
    }

    /// Record the stream asset a run read — failed with the reason when its document gave no
    /// graph — keeping the display name of the record it replaces, and load its graph, started
    /// or held stopped as the run's playback says. A replace is recorded first, then the running stream unloaded; a new stream is
    /// recorded once loaded, before its start, so a load a machine shutdown abandons leaves
    /// nothing recorded. A load or start that is refused records the stream failed with the
    /// reason. A record that cannot be written leaves the running stream as it was.
    pub(super) fn record_and_load_one_stream_a_run_read(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        read: StreamARunRead<'_>,
    ) -> Result<StreamLoadedOrFailedByARun> {
        let StreamARunRead {
            stream_id,
            stream_asset:
                StreamAssetRead {
                    stream_location,
                    stream_name: _,
                    display_name,
                    graph_or_why_none,
                },
            stream_environment,
            playback_after_the_load,
            why_the_projects_venv_was_not_made,
            is_interrupted,
        } = read;
        let playback_after_the_load = if why_the_projects_venv_was_not_made.is_some() {
            ProjectPlaybackAfterItsLoad::HeldStopped
        } else {
            playback_after_the_load
        };
        let (previous_record, previously_loaded) =
            self.the_recorded_stream_a_run_of_its_location_replaces(&stream_id, &stream_location)?;
        let mut stream_record =
            StreamRecord::of_a_running_stream(stream_id, stream_location, stream_environment);
        if playback_after_the_load == ProjectPlaybackAfterItsLoad::HeldStopped {
            stream_record.mark_open_with_its_playback_stopped();
        }
        let (graph, failed_because_its_document_gave_no_graph) = match graph_or_why_none {
            Ok(graph) => (Some(graph), None),
            Err(failed_because) => {
                stream_record.failed_because = Some(failed_because.clone());
                (None, Some(failed_because))
            }
        };
        let replaced_a_recorded_stream = previous_record.is_some();
        let a_new_stream_recorded_once_loaded =
            !replaced_a_recorded_stream && failed_because_its_document_gave_no_graph.is_none();
        if !a_new_stream_recorded_once_loaded {
            stream_record = stream_records.write_the_record_a_load_made(stream_record)?;
            if let Some(previously_loaded) = previously_loaded {
                unload_a_stream_an_action_took_back(&previously_loaded, "its record is replaced");
            }
        }
        let state = match (graph, failed_because_its_document_gave_no_graph) {
            (Some(graph), _) => self.load_a_recorded_stream_recording_it_failed_when_refused(
                stream_records,
                &mut stream_record,
                &display_name,
                &graph,
                a_new_stream_recorded_once_loaded,
                playback_after_the_load,
                is_interrupted,
            )?,
            (None, failed_because) => StreamStateAfterItsRun::Failed {
                failed_because: failed_because.unwrap_or_default(),
            },
        };
        let state = match (why_the_projects_venv_was_not_made, state) {
            (Some(why_the_venv_was_not_made), StreamStateAfterItsRun::HeldStopped { .. })
            | (Some(why_the_venv_was_not_made), StreamStateAfterItsRun::Failed { .. }) => {
                record_a_stream_held_from_its_document_failed(
                    stream_records,
                    &mut stream_record,
                    why_the_venv_was_not_made.to_string(),
                )?
            }
            (_, state) => state,
        };
        Ok(StreamLoadedOrFailedByARun::of_the_record(
            stream_record,
            display_name,
            state,
            replaced_a_recorded_stream,
        ))
    }

    /// The record of `stream_location` a run of it replaces, and its stream
    /// when it is loaded. Refused naming both for an id another location
    /// holds, and for a stream loaded under the id that no record holds.
    pub(super) fn the_recorded_stream_a_run_of_its_location_replaces(
        &self,
        stream_id: &str,
        stream_location: &StreamLocation,
    ) -> Result<(Option<StreamRecord>, Option<Arc<LoadedStreamInThisRuntime>>)> {
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        let record = self.stream_record_of_the_id(stream_id)?;
        refuse_an_id_another_location_holds(
            stream_id,
            stream_location,
            loaded.as_deref(),
            record.as_ref(),
        )?;
        let record = match record {
            Some(StreamRecordAsRead::Current(record)) => Some(record),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(_)) | None => None,
        };
        match (record, loaded) {
            (_, Some(loaded))
                if loaded.holding() == LoadedStreamHolding::LivesWithTheHostProcess =>
            {
                Err(a_stream_id_already_loaded_refusal(
                    &loaded,
                    STOP_IT_FIRST_TO_LOAD_THIS_ONE,
                ))
            }
            (None, Some(loaded)) => Err(a_stream_id_already_loaded_refusal(
                &loaded,
                STOP_IT_FIRST_TO_LOAD_THIS_ONE,
            )),
            (record, loaded) => Ok((record, loaded)),
        }
    }

    /// Enter, on this thread, the log of the project in `project_directory`
    /// for a load of it, so its venv making and document reading land there under no
    /// stream; `None` for a directory that is not a project — one that does not
    /// resolve or holds no `tatolab-project.json` — whose load logs to the runtime's own log.
    pub(super) fn enter_the_log_of_the_project_a_load_takes_in(
        &self,
        project_directory: &Path,
        action_id: Option<&str>,
    ) -> Option<TheProjectsLogEnteredForOneLoad> {
        let project_directory = std::fs::canonicalize(project_directory).ok()?;
        if why_a_directory_is_no_project(&project_directory).is_some() {
            return None;
        }
        let entry = ProjectLogRouteEntry::for_the_project_in(
            &project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
            action_id,
        );
        Some(TheProjectsLogEnteredForOneLoad {
            _entered_on_this_thread: entry.enter_on_this_thread(),
            entry,
        })
    }
}

/// Refuse a project two of whose stream graph documents hold a node of one UUID, naming both
/// documents, since a node's UUID keys the values it keeps in the project's store.
pub(super) fn refuse_streams_whose_graph_documents_share_a_node_uuid<'read>(
    project_directory: &Path,
    each_stream_document_and_its_graph: impl IntoIterator<
        Item = (
            &'read ProjectStreamDocument,
            &'read std::result::Result<GraphSnapshot, String>,
        ),
    >,
) -> Result<()> {
    let mut document_and_node_path_of_each_node_uuid: HashMap<Uuid, (String, String)> =
        HashMap::new();
    for (stream_document, graph_or_why_none) in each_stream_document_and_its_graph {
        let Ok(graph) = graph_or_why_none else {
            continue;
        };
        let document_path = stream_document.path.to_string();
        for (node_path, node) in graph.nodes_at_every_level() {
            let Some(node_uuid) = node.id else {
                continue;
            };
            match document_and_node_path_of_each_node_uuid.get(&node_uuid) {
                Some((first_document_path, first_node_path))
                    if *first_document_path != document_path =>
                {
                    return Err(Error::Configuration(format!(
                        "the project in {} was not loaded: its graph documents \
                         `{first_document_path}` and `{document_path}` both hold the node \
                         {node_uuid} (`{first_node_path}` and `{node_path}`), and a node's UUID \
                         keys the values it keeps, so each stream's nodes need UUIDs of their \
                         own; a copied graph document keeps the UUIDs of the one it was copied \
                         from",
                        project_directory.display(),
                    )));
                }
                Some(_) => {}
                None => {
                    document_and_node_path_of_each_node_uuid
                        .insert(node_uuid, (document_path.clone(), node_path));
                }
            }
        }
    }
    Ok(())
}

/// How a run refused for an id a stream another caller loaded holds is told to
/// go on.
pub(super) const STOP_IT_FIRST_TO_LOAD_THIS_ONE: &str = "stop it first to load this one";

/// Refuse a load of `stream_location` under `stream_id` when the id is held by
/// another stream — loaded, or recorded — or by the same stream of another copy of its
/// project, naming both. A record an earlier runtime wrote from the same project
/// directory holds nothing: the load writes its own record over it.
pub(super) fn refuse_an_id_another_location_holds(
    stream_id: &str,
    stream_location: &StreamLocation,
    loaded: Option<&LoadedStreamInThisRuntime>,
    record: Option<&StreamRecordAsRead>,
) -> Result<()> {
    let refuse = |held_by: String| {
        Err(Error::GraphError(format!(
            "the stream id `{stream_id}` of {stream_location} is held by {held_by}; two \
             streams cannot share one id, so `rm` the stream that holds it, or give a second \
             copy of a project its own project UUID"
        )))
    };
    let held_by_another = |held: &StreamLocation| {
        !held.is_the_same_stream_as(stream_location)
            || another_copy_of_the_project_holds(held, &stream_location.project_directory)
    };
    if let Some(loaded) = loaded {
        let loaded_location = loaded.stream_location();
        if held_by_another(loaded_location) {
            return refuse(format!("the loaded stream from {loaded_location}"));
        }
    }
    match record {
        Some(StreamRecordAsRead::Current(record)) if held_by_another(&record.location) => {
            refuse(format!(
                "a{} recorded stream from {}",
                the_state_a_refusal_names_a_record_by(record),
                record.location
            ))
        }
        Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))
            if earlier
                .project_directory
                .as_deref()
                .is_some_and(|recorded_directory| {
                    the_same_directory(recorded_directory, &stream_location.project_directory)
                }) =>
        {
            Ok(())
        }
        Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => refuse(format!(
            "a record an earlier runtime wrote{}",
            earlier
                .project_directory
                .as_ref()
                .map(|project_directory| format!(" from {}", project_directory.display()))
                .unwrap_or_default()
        )),
        _ => Ok(()),
    }
}

/// One stream asset a run read, the id it records it under, and what interrupts its load.
pub(super) struct StreamARunRead<'run> {
    stream_id: String,
    stream_asset: StreamAssetRead,
    stream_environment: &'run StreamEnvironment,
    playback_after_the_load: ProjectPlaybackAfterItsLoad,
    /// Why the project's `.venv` was not made, which holds the stream stopped and failed.
    why_the_projects_venv_was_not_made: Option<&'run str>,
    is_interrupted: &'run dyn Fn() -> bool,
}

/// A project's log entered on the thread of one load of the project; on drop,
/// every record the load wrote is written to the log and synced, so a reader
/// right after the load finds them.
pub(super) struct TheProjectsLogEnteredForOneLoad {
    entry: Arc<ProjectLogRouteEntry>,
    _entered_on_this_thread: ProjectLogRouteEntryEnteredOnThisThread,
}

impl Drop for TheProjectsLogEnteredForOneLoad {
    fn drop(&mut self) {
        self.entry.write_every_record_it_emitted_and_sync();
    }
}

/// The state a refusal names a record by, a leading space before it:
/// ` stopped`, ` closed`, ` failed`, or nothing for one the runtime re-loads playing.
pub(super) fn the_state_a_refusal_names_a_record_by(record: &StreamRecord) -> &'static str {
    match record.listing_state() {
        StreamListingState::Failed => " failed",
        StreamListingState::Stopped => " stopped",
        StreamListingState::Closed => " closed",
        StreamListingState::Running
        | StreamListingState::Paused
        | StreamListingState::Preparing
        | StreamListingState::Stopping => "",
    }
}
