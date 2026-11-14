// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// The kind of the last action under way on the project in `project_directory` that
    /// opens or closes it or starts or stops its playback; `None` when none is.
    pub(super) fn the_last_lifecycle_or_playback_action_queued_on_the_project(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionKind> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
            .iter()
            .rev()
            .find(|action| {
                the_same_directory(&action.project_directory, project_directory)
                    && matches!(
                        action.kind,
                        StreamActionKind::Open
                            | StreamActionKind::StartPlayback
                            | StreamActionKind::Stop
                            | StreamActionKind::Close
                            | StreamActionKind::Remove
                            | StreamActionKind::Delete
                    )
            })
            .map(|action| action.kind)
    }

    /// Whether the last action under way on the project in `project_directory` that opens or
    /// closes it or starts or stops its playback is a playback start.
    pub(super) fn is_a_playback_start_the_last_action_queued_on_the_project(
        &self,
        project_directory: &Path,
    ) -> bool {
        self.the_last_lifecycle_or_playback_action_queued_on_the_project(project_directory)
            == Some(StreamActionKind::StartPlayback)
    }

    /// Whether the project in `project_directory` is open once the actions queued on it have
    /// run: the last of them that opens, closes or plays it opens or plays it, or, with none
    /// queued, a stream of it is loaded or a record of it is not closed.
    pub(super) fn the_project_is_open_once_its_queued_actions_run(
        &self,
        project_directory: &Path,
    ) -> bool {
        match self.the_last_lifecycle_or_playback_action_queued_on_the_project(project_directory) {
            Some(StreamActionKind::Open | StreamActionKind::StartPlayback) => true,
            Some(StreamActionKind::Close | StreamActionKind::Remove | StreamActionKind::Delete) => {
                false
            }
            _ => {
                !self
                    .loaded_streams_of_the_project(project_directory)
                    .is_empty()
                    || self
                        .current_records_of_the_project(project_directory)
                        .iter()
                        .any(|record| !record.closed)
            }
        }
    }

    /// Queue the opening of the project in `request`'s directory on its stream action queue,
    /// answering the action's id at once: it reads every stream asset's graph document, makes
    /// the project's `.venv`, and holds each stream loaded and stopped, its graph there to read
    /// and edit, nothing running and nothing claimed; a stream whose document gives no graph or
    /// whose load is refused is recorded failed. The project stays open across the runtime's
    /// restarts until a `close_project`. Refused, with nothing queued, for a directory not named
    /// by its absolute path and for a project already open once its queued actions run, naming
    /// [`Self::reload_project`].
    pub fn open_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued> {
        let project_directory =
            the_absolute_project_directory_resolved(&request.project_directory)?;
        if self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is already open, so there is none to open; `reload_project` \
                 re-reads what changed on disk",
                project_directory.display()
            )));
        }
        Ok(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            None,
            ProjectPlaybackAfterItsLoad::HeldStopped,
            StreamActionKind::Open,
        ))
    }

    /// Queue the reload of the open project in `request`'s directory on its stream action
    /// queue, answering the action's id at once: [`Self::queue_the_load_of_the_stream_assets_of_the_project`]'s load, which
    /// re-reads every stream asset's graph document and replaces each stream under its id,
    /// keeping the project's playback as it is — playing, paused or stopped — and retries its
    /// failed streams. Refused, with nothing queued, for a directory not named by its absolute
    /// path and for a project that is not open, naming [`Self::open_project`].
    pub fn reload_project(
        &self,
        request: ProjectOpenOrReloadRequest,
    ) -> Result<StreamActionQueued> {
        let project_directory =
            the_absolute_project_directory_resolved(&request.project_directory)?;
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is not open, so there is none to reload; `open_project` \
                 opens it",
                project_directory.display()
            )));
        }
        let playback_after_the_load =
            self.the_playback_a_load_of_the_open_project_keeps(&project_directory);
        Ok(self.queue_the_load_of_the_stream_assets_of_the_project(
            project_directory,
            None,
            playback_after_the_load,
            StreamActionKind::Load,
        ))
    }

    /// The playback a load of the open project in `project_directory` gives each stream it
    /// loads: playing while the project plays once its queued actions run, or a stream of it
    /// failed while it played, and held stopped otherwise.
    pub(super) fn the_playback_a_load_of_the_open_project_keeps(
        &self,
        project_directory: &Path,
    ) -> ProjectPlaybackAfterItsLoad {
        let a_stream_failed_while_it_played = || {
            self.current_records_of_the_project(project_directory)
                .iter()
                .any(|record| record.is_failed() && !record.stopped)
        };
        if self.the_project_runs_once_its_queued_actions_run(project_directory)
            || a_stream_failed_while_it_played()
        {
            ProjectPlaybackAfterItsLoad::Playing
        } else {
            ProjectPlaybackAfterItsLoad::HeldStopped
        }
    }

    /// Queue the closing of the open project in `project_directory` on its queue, and answer
    /// the action: it ends the playback of every stream of the project, unloads each, forgets
    /// its function-node states, and records each closed, so the runtime's restart leaves the
    /// project unloaded until an `open_project`. It deletes nothing: the project's files, its
    /// `.venv` and its synced dependencies stay. Each record carries the close as a pending
    /// mark until the action records it closed, so a crash finishes it at the next start.
    /// Refused — with nothing queued — for a directory not named by its absolute path, a
    /// project whose removal is queued, and a project that is not open once its queued actions
    /// run.
    pub fn close_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is not open, so there is none to close. Loaded: {}. \
                 Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        let action_id = self.next_stream_action_id();
        let mut streams: Vec<StreamOfAStreamAction> = Vec::new();
        let ids_of_the_projects_records =
            self.ids_of_every_record_of_the_project(&project_directory);
        if let Some(stream_records) = self.stream_records() {
            for stream_id in &ids_of_the_projects_records {
                let marked = stream_records.update(stream_id, |record| match record {
                    Some(record) => {
                        refuse_a_stream_whose_project_removal_is_queued(stream_id, record)?;
                        record.queue_the_pending_stream_action(
                            &action_id,
                            PendingStreamActionKind::Close,
                        );
                        Ok::<_, Error>(Some(record.clone()))
                    }
                    None => Ok(None),
                });
                match marked {
                    Ok(Ok(Some(record))) => streams.push(StreamOfAStreamAction {
                        display_name: the_display_name_of_the_stream_at(
                            &record.location,
                            &record.stream_id,
                        ),
                        stream_id: record.stream_id,
                        stream_location: Some(record.location),
                        state: None,
                    }),
                    Ok(Ok(None)) => {}
                    Ok(Err(refusal)) | Err(refusal) => {
                        for stream_marked_already in &streams {
                            self.finish_the_pending_stream_action_on_the_record(
                                &stream_marked_already.stream_id,
                                &action_id,
                            );
                        }
                        return Err(refusal);
                    }
                }
            }
        }
        for loaded in self.loaded_streams_of_the_project(&project_directory) {
            if !ids_of_the_projects_records
                .iter()
                .any(|recorded| recorded == loaded.stream_id())
            {
                streams.push(StreamOfAStreamAction {
                    stream_id: loaded.stream_id().to_string(),
                    display_name: loaded.display_name(),
                    stream_location: Some(loaded.stream_location().clone()),
                    state: None,
                });
            }
        }
        let stream_ids: Vec<String> = streams
            .iter()
            .map(|stream| stream.stream_id.clone())
            .collect();
        self.forget_that_the_project_is_paused(&project_directory);
        let project_directory_closing = project_directory.clone();
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory,
            kind: StreamActionKind::Close,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams,
            failed_because: None,
        };
        Ok(self
            .stream_actions
            .stream_action_queue_of_each_project
            .queue_the_stream_action(
                action,
                Box::new(move |runner: &Runner| {
                    runner.close_the_project_for_its_stream_action(
                        &action_id,
                        &project_directory_closing,
                        &stream_ids,
                    );
                }),
                &self.tokio_handle(),
            ))
    }

    /// Run the close the stream action `action_id` queued of the streams `stream_ids` of the
    /// project in `project_directory`: unload each loaded one, its playback ended, forget its
    /// function-node states, record each closed, close the project's store, and finish the
    /// action done, or failed naming each stream whose record was not written closed.
    pub(super) fn close_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: &Path,
        stream_ids: &[String],
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let mut states = Vec::with_capacity(stream_ids.len());
        let mut not_closed = Vec::new();
        for stream_id in stream_ids {
            let _one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(stream_id) {
                unload_a_stream_an_action_took_back(&loaded, "its project closed");
            }
            self.forget_the_function_node_states_of_the_stream(stream_id);
            let recorded = match self.stream_records() {
                Some(stream_records) => stream_records
                    .change_the_record(stream_id, |record| {
                        record.mark_closed();
                        record.finish_the_pending_stream_action(action_id);
                    })
                    .map(|_| ()),
                None => Ok(()),
            };
            match recorded {
                Ok(()) => states.push((stream_id.clone(), StreamListingState::Closed)),
                Err(not_written) => not_closed.push(format!(
                    "the stream `{stream_id}` is unloaded and was not recorded closed, so the \
                     runtime's restart loads it again: {not_written}"
                )),
            }
        }
        self.engine_resources_shared_by_every_stream
            .project_stores_open_in_this_runtime
            .close_the_store_of_the_project_at(
                project_directory,
                every_node_uuid_the_graph_documents_of_the_project_hold(project_directory).as_ref(),
            );
        progress.finish(if not_closed.is_empty() {
            Ok(states)
        } else {
            Err(not_closed.join("; "))
        });
    }

    /// Queue a load of every stream asset of the project in `project_directory`, absolute and
    /// resolved, on its stream action queue, answering the action's id at once;
    /// [`Runner::describe_stream_action`] follows it. The load reads each stream asset's graph
    /// document, makes the project's `.venv`, unbounded, and loads or fails each stream under
    /// the id its project and stream asset give it: a directory that does not exist or holds no
    /// `tatolab-project.json`, and an id another stream holds, fail the action.
    pub(super) fn queue_the_load_of_the_stream_assets_of_the_project(
        &self,
        project_directory: PathBuf,
        only_the_stream_assets: Option<BTreeSet<Uuid>>,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        kind: StreamActionKind,
    ) -> StreamActionQueued {
        let keeps_each_loaded_stream_whose_node_code_is_unchanged = kind == StreamActionKind::Load;
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams: Vec::new(),
            failed_because: None,
        };
        queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.load_the_project_for_its_stream_action(
                    &action_id,
                    project_directory,
                    only_the_stream_assets,
                    playback_after_the_load,
                    keeps_each_loaded_stream_whose_node_code_is_unchanged,
                );
            }),
            &self.tokio_handle(),
        )
    }

    /// Whether the project in `project_directory` plays once the actions queued on it have
    /// run: the last of them that opens, closes or plays it is a playback start, or, with none
    /// queued, a loaded stream of it is not held stopped or a record of it not loaded is
    /// running.
    pub(super) fn the_project_runs_once_its_queued_actions_run(
        &self,
        project_directory: &Path,
    ) -> bool {
        match self.the_last_lifecycle_or_playback_action_queued_on_the_project(project_directory) {
            Some(kind) => kind == StreamActionKind::StartPlayback,
            None => {
                let loaded_streams_of_the_project =
                    self.loaded_streams_of_the_project(project_directory);
                loaded_streams_of_the_project
                    .iter()
                    .any(|loaded| !loaded.is_held_stopped())
                    || self
                        .current_records_of_the_project(project_directory)
                        .iter()
                        .filter(|record| {
                            !loaded_streams_of_the_project
                                .iter()
                                .any(|loaded| loaded.stream_id() == record.stream_id)
                        })
                        .any(|record| record.listing_state() == StreamListingState::Running)
            }
        }
    }

    /// Create a project from the starter `tatolab new` writes, or from the sample project
    /// `request` names, and queue its open, answering the action's id and the id its stream
    /// asset `main` loads as at once. The directory is `request`'s, created when missing, or one
    /// drawn under `$HOME/streams`. The action writes the starter's files or copies the
    /// sample's under a new project UUID, has the host commit them as the first commit of the
    /// project's history, then opens it as [`Runner::open_project`] does, the stream asset
    /// `main` shown under the folder's name and held stopped; a directory already holding a file
    /// the create writes fails the action with nothing written. Refused for a sample the runtime
    /// unit does not carry, naming the ones it does, a relative directory, one that cannot be
    /// created, and a draw that finds no free name.
    pub fn create_project(&self, request: CreateProjectRequest) -> Result<ProjectCreationQueued> {
        let sample = request
            .sample
            .as_deref()
            .map(|sample_name| {
                the_sample_project_named(
                    self.sample_projects_directory_of_this_runtime_unit()
                        .as_deref(),
                    sample_name,
                )
                .map_err(|refusal| Error::Configuration(refusal.to_string()))
            })
            .transpose()?;
        let project_directory = match request.project_directory {
            Some(project_directory) => {
                if !project_directory.is_absolute() {
                    return Err(Error::Configuration(format!(
                        "`project_directory` must be an absolute path; got `{}`",
                        project_directory.display()
                    )));
                }
                std::fs::create_dir_all(&project_directory).map_err(|not_created| {
                    Error::Configuration(format!(
                        "{} could not be created, so no project was created there: {not_created}",
                        project_directory.display()
                    ))
                })?;
                project_directory
            }
            None => draw_new_project_directory_under_the_streams_root()?,
        };
        let project_directory =
            std::fs::canonicalize(&project_directory).unwrap_or(project_directory);
        let scaffold = match sample {
            Some(sample) => {
                NewProjectScaffold::of_a_new_project_from_a_sample_in(&project_directory, sample)
            }
            None => NewProjectScaffold::of_a_new_project_in(&project_directory),
        };
        let stream_id = scaffold.stream_id_in(&project_directory);
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::Open,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams: vec![StreamOfAStreamAction {
                stream_id: stream_id.clone(),
                display_name: scaffold.project_name.clone(),
                stream_location: Some(scaffold.stream_location_in(&project_directory)),
                state: None,
            }],
            failed_because: None,
        };
        let project_directory_the_action_creates = project_directory.clone();
        let queued = queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.create_the_project_for_its_stream_action(
                    &action_id,
                    project_directory_the_action_creates,
                    &scaffold,
                );
            }),
            &self.tokio_handle(),
        );
        Ok(ProjectCreationQueued {
            action_id: queued.action_id,
            project_directory: queued.project_directory,
            stream_id,
        })
    }

    /// Every sample project the runtime unit carries beside this runtime's lend, in name order;
    /// none when it was handed no lend.
    pub fn list_samples(&self) -> ListSamplesToolResult {
        ListSamplesToolResult {
            samples: self
                .sample_projects_directory_of_this_runtime_unit()
                .map(|samples_directory| every_sample_project_in(&samples_directory))
                .unwrap_or_default()
                .into_iter()
                .map(|sample| ListSamplesToolResultSample {
                    name: sample.listed.name,
                    title: sample.listed.title,
                    summary: sample.listed.summary,
                })
                .collect(),
        }
    }

    pub(super) fn sample_projects_directory_of_this_runtime_unit(&self) -> Option<PathBuf> {
        self.engine_resources_shared_by_every_stream
            .processor_interpreter_lend_directory
            .get()
            .and_then(sample_projects_directory_beside_the_lend)
    }

    /// Run the load the stream action `action_id` queued of the project in
    /// `project_directory`, reporting its steps, and finish the action done,
    /// failed, or cancelled once it saw its cancel.
    pub(super) fn load_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: PathBuf,
        only_the_stream_assets: Option<BTreeSet<Uuid>>,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        keeps_each_loaded_stream_whose_node_code_is_unchanged: bool,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        self.load_the_project_reporting_to(
            action_id,
            &progress,
            ProjectStreamsToReadAndLoad {
                project_directory,
                only_the_stream_assets,
                playback_after_the_load,
                keeps_each_loaded_stream_whose_node_code_is_unchanged,
            },
        );
    }

    /// Run the load of the project in `project_directory` for the stream
    /// action `action_id`, reporting to `progress`, and finish the action.
    pub(super) fn load_the_project_reporting_to(
        &self,
        action_id: &str,
        progress: &StreamActionProgress<'_>,
        request: ProjectStreamsToReadAndLoad,
    ) {
        let run_outcome = match why_a_directory_is_no_project(&request.project_directory) {
            Some(why_it_is_no_project) => {
                tracing::warn!("the stream action {action_id} failed: {why_it_is_no_project}");
                Err(why_it_is_no_project)
            }
            None => self
                .run_the_project_reporting_to(request, Some(progress), &|| {
                    progress.is_interrupted()
                })
                .map_err(|refusal| refusal.to_string()),
        };
        self.finish_the_load_action(action_id, progress, run_outcome);
    }

    /// Write `scaffold` into `project_directory` for the stream action
    /// `action_id` [`Runner::create_project`] queued, have the host commit it as the first
    /// commit of the project's history, then run its load; a scaffold that cannot be written
    /// or committed fails the action, written to the project's log, and a cancel seen before
    /// the write ends it cancelled with nothing written.
    pub(super) fn create_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: PathBuf,
        scaffold: &NewProjectScaffold,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        if progress.is_interrupted() {
            self.finish_the_load_action(
                action_id,
                &progress,
                Err("it was interrupted before the project's files were written".to_string()),
            );
            return;
        }
        let written = write_the_new_project_scaffold_into(&project_directory, scaffold)
            .and_then(|()| self.commit_the_first_revision_of_a_created_project(&project_directory));
        if let Err(not_written) = written {
            ProjectLogRouteEntry::for_the_project_in(
                &project_directory,
                self.engine_resources_shared_by_every_stream
                    .runtime_id
                    .as_str(),
                Some(action_id),
            )
            .run_entered(|| tracing::warn!("the stream action {action_id} failed: {not_written}"));
            self.finish_the_load_action(action_id, &progress, Err(not_written.to_string()));
            return;
        }
        self.load_the_project_reporting_to(
            action_id,
            &progress,
            ProjectStreamsToReadAndLoad {
                project_directory,
                only_the_stream_assets: None,
                playback_after_the_load: ProjectPlaybackAfterItsLoad::HeldStopped,
                keeps_each_loaded_stream_whose_node_code_is_unchanged: false,
            },
        );
    }

    /// Finish the load action `action_id` done with the streams `run_outcome`
    /// gave, failed for why, or cancelled once `progress` saw its cancel.
    pub(super) fn finish_the_load_action(
        &self,
        action_id: &str,
        progress: &StreamActionProgress<'_>,
        run_outcome: std::result::Result<StreamRunOutcome, String>,
    ) {
        let saw_its_cancel = progress.saw_its_cancel();
        self.stream_actions
            .stream_action_queue_of_each_project
            .change_the_held_stream_action(action_id, |action| {
                action.step = None;
                match run_outcome {
                    Ok(run_outcome) => {
                        action.state = if saw_its_cancel {
                            StreamActionState::Cancelled
                        } else {
                            StreamActionState::Done
                        };
                        action.streams = run_outcome
                            .streams
                            .into_iter()
                            .map(|stream| StreamOfAStreamAction {
                                state: Some(match stream.state {
                                    StreamStateAfterItsRun::Running { .. } => {
                                        StreamListingState::Running
                                    }
                                    StreamStateAfterItsRun::HeldStopped { .. } => {
                                        StreamListingState::Stopped
                                    }
                                    StreamStateAfterItsRun::Failed { .. } => {
                                        StreamListingState::Failed
                                    }
                                }),
                                stream_id: stream.stream_id,
                                display_name: stream.display_name,
                                stream_location: Some(stream.stream_location),
                            })
                            .collect();
                    }
                    Err(_) if saw_its_cancel => action.state = StreamActionState::Cancelled,
                    Err(failed_because) => {
                        action.state = StreamActionState::Failed;
                        action.failed_because = Some(failed_because);
                    }
                }
            });
    }
}
