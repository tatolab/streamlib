// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// Queue a stop of the whole project in `project_directory` on its queue, and answer the
    /// action: it ends the playback of every loaded stream of the project — every instance,
    /// device claim, function-node state and the clock torn down — keeps each held with its
    /// graph to read and edit, and records each recorded one stopped. The project stays open
    /// and stopped, across the runtime's restarts too, until a `start_playback` or a
    /// `close_project`. Each record playing once the actions queued before run carries the stop
    /// as a pending mark until the action records it stopped, so a crash finishes it at the
    /// next start. Refused — with nothing queued — for a directory not named by its absolute
    /// path, a project whose removal is queued, and a project none of whose streams runs once
    /// the actions queued before run.
    pub fn stop_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        if !project_directory.is_absolute() {
            return Err(Error::Configuration(format!(
                "`project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            )));
        }
        let project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        let action_id = self.next_stream_action_id();
        let mut streams: Vec<StreamOfAStreamAction> = Vec::new();
        let ids_of_the_projects_records =
            self.ids_of_every_record_of_the_project(&project_directory);
        let the_project_runs_once_its_queued_actions_run =
            self.is_a_playback_start_the_last_action_queued_on_the_project(&project_directory);
        let take_the_marks_off = |streams: &[StreamOfAStreamAction]| {
            for stream_marked_already in streams {
                self.finish_the_pending_stream_action_on_the_record(
                    &stream_marked_already.stream_id,
                    &action_id,
                );
            }
        };
        if let Some(stream_records) = self.stream_records() {
            for stream_id in &ids_of_the_projects_records {
                let marked = stream_records.update(stream_id, |record| match record {
                    Some(record) => {
                        refuse_a_stream_whose_project_removal_is_queued(stream_id, record)?;
                        if !the_project_runs_once_its_queued_actions_run
                            && (record.is_failed()
                                || record.is_stopped_once_its_pending_stream_actions_run())
                        {
                            return Ok(None);
                        }
                        record.queue_the_pending_stream_action(
                            &action_id,
                            PendingStreamActionKind::Stop,
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
                        take_the_marks_off(&streams);
                        return Err(refusal);
                    }
                }
            }
        }
        for loaded in self.every_loaded_stream() {
            if the_same_directory(loaded.project_directory(), &project_directory)
                && !ids_of_the_projects_records
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
        if streams.is_empty() {
            return Err(Error::Runtime(format!(
                "no stream of the project in {} plays, so there is none to stop; `start_playback` \
                 plays it. \
                 Loaded: {}. Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        let stream_ids: Vec<String> = streams
            .iter()
            .map(|stream| stream.stream_id.clone())
            .collect();
        self.forget_that_the_project_is_paused(&project_directory);
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory,
            kind: StreamActionKind::Stop,
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
                    runner.stop_the_project_for_its_stream_action(&action_id, &stream_ids);
                }),
                &self.tokio_handle(),
            ))
    }

    /// Pause every loaded stream of the project in `project_directory` now, never waiting
    /// behind an action on its queue, and answer the streams paused: each processor holds its
    /// state and processes nothing, and a graph change made meanwhile compiles on
    /// [`Self::resume_playback`], and each stream's clock holds where it stands. The project
    /// stays paused until that resume, a `stop_playback`, a `close_project` or a
    /// `remove_project`: a stream a `reload_project` starts meanwhile, `tatolab dev`'s re-load
    /// on a save among them, starts paused. The pause is recorded, so a restart of the runtime
    /// re-loads the project paused. Pausing a paused project pauses nothing more.
    /// Refused for a directory not named by its absolute path, and for a project none of whose
    /// streams plays and whose last action queued is not a playback start.
    pub fn pause_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        let loaded_streams_of_the_project = self.loaded_streams_of_the_project(&project_directory);
        if loaded_streams_of_the_project
            .iter()
            .all(|loaded| loaded.is_held_stopped())
            && !self.is_a_playback_start_the_last_action_queued_on_the_project(&project_directory)
        {
            return Err(Error::Runtime(format!(
                "no stream of the project in {} plays, so there is none to pause; `start_playback` \
                 plays it. \
                 Loaded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal()
            )));
        }
        if !directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, &project_directory))
        {
            directories_of_the_paused_projects.push(project_directory.clone());
        }
        self.record_each_stream_of_the_project_paused(&project_directory, true);
        let mut stream_ids = Vec::new();
        for stream in loaded_streams_of_the_project {
            if stream.status() != RuntimeStatus::Started {
                continue;
            }
            stream.pause()?;
            stream_ids.push(stream.stream_id().to_string());
        }
        Ok(ProjectPausedOrResumed {
            project_directory,
            stream_ids,
        })
    }

    /// Resume every paused stream of the project in `project_directory` now, compiling each
    /// one's graph changes made while it was paused, and answer the streams resumed; a compile
    /// that fails is logged on the stream, as a live edit's is. Refused for a directory not
    /// named by its absolute path, and for a project that is not paused.
    pub fn resume_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        let paused_streams_of_the_project: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .loaded_streams_of_the_project(&project_directory)
            .into_iter()
            .filter(|stream| is_paused(stream.status()))
            .collect();
        let the_project_was_marked_paused = directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, &project_directory));
        if !the_project_was_marked_paused && paused_streams_of_the_project.is_empty() {
            return Err(Error::Runtime(format!(
                "the project in {} is not paused, so there is none to resume",
                project_directory.display()
            )));
        }
        directories_of_the_paused_projects
            .retain(|paused| !the_same_directory(paused, &project_directory));
        self.record_each_stream_of_the_project_paused(&project_directory, false);
        let mut stream_ids = Vec::new();
        for stream in paused_streams_of_the_project {
            if let Err(not_compiled) = stream.resume() {
                let _entered_this_streams_log_route = stream.log_route().enter_on_this_thread();
                tracing::error!(
                    "the stream `{}` resumed, and the graph changes made while it was paused \
                     did not compile: {not_compiled}",
                    stream.stream_id()
                );
            }
            stream_ids.push(stream.stream_id().to_string());
        }
        Ok(ProjectPausedOrResumed {
            project_directory,
            stream_ids,
        })
    }

    /// Start `stream`, paused while its project is paused, holding the paused projects so a
    /// `pause_playback` cannot land between the check and the start.
    pub(super) fn start_the_stream_paused_while_its_project_is(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<()> {
        let directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        if directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, stream.project_directory()))
        {
            stream.start_paused()
        } else {
            stream.start()
        }
    }

    /// Put the project in `project_directory` among the paused projects, so each stream of it
    /// a load starts starts paused.
    pub(super) fn hold_the_project_paused(&self, project_directory: &Path) {
        let mut directories_of_the_paused_projects = self
            .stream_actions
            .directories_of_the_paused_projects
            .lock();
        if !directories_of_the_paused_projects
            .iter()
            .any(|paused| the_same_directory(paused, project_directory))
        {
            directories_of_the_paused_projects.push(project_directory.to_path_buf());
        }
    }

    /// Take the project in `project_directory` off the paused projects, as its stop or
    /// removal is queued.
    pub(super) fn forget_that_the_project_is_paused(&self, project_directory: &Path) {
        self.stream_actions
            .directories_of_the_paused_projects
            .lock()
            .retain(|paused| !the_same_directory(paused, project_directory));
        self.record_each_stream_of_the_project_paused(project_directory, false);
    }

    /// Record each stream of the project in `project_directory` paused, or not; a record
    /// that cannot be written is logged on its stream, and the live pause stands.
    pub(super) fn record_each_stream_of_the_project_paused(
        &self,
        project_directory: &Path,
        paused: bool,
    ) {
        for stream_id in self.ids_of_every_record_of_the_project(project_directory) {
            if let Err(not_recorded) = self.change_the_record_of_the_stream(&stream_id, |record| {
                record.paused = paused && !record.stopped && !record.closed;
            }) {
                tracing::error!(
                    "the stream `{stream_id}` was not recorded {}: {not_recorded}",
                    if paused { "paused" } else { "unpaused" }
                );
            }
        }
    }

    /// Run the stop the stream action `action_id` queued of the streams `stream_ids` of one
    /// project, and finish the action done with each stream stopped, or failed naming each
    /// stream unloaded and not recorded stopped. A stream that is no longer loaded and is
    /// recorded failed or stopped when the stop reaches it is left as it is.
    pub(super) fn stop_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        stream_ids: &[String],
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let mut states = Vec::with_capacity(stream_ids.len());
        let mut not_stopped = Vec::new();
        for stream_id in stream_ids {
            let left_as_it_is = self
                .loaded_stream_of_the_id_when_loaded(stream_id)
                .is_none()
                && self
                    .current_stream_record_of_the_id(stream_id)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.is_failed() || record.stopped);
            if left_as_it_is {
                self.finish_the_pending_stream_action_on_the_record(stream_id, action_id);
                continue;
            }
            match self.stop_the_stream_finishing_its_pending_stop(stream_id, Some(action_id)) {
                Ok(StreamStopOutcome {
                    stop_not_recorded_because: None,
                    ..
                }) => states.push((stream_id.clone(), StreamListingState::Stopped)),
                Ok(StreamStopOutcome {
                    stop_not_recorded_because: Some(not_recorded_because),
                    ..
                }) => not_stopped.push(format!(
                    "the stream `{stream_id}` is unloaded and was not recorded stopped, so the \
                     runtime's restart loads it again: {not_recorded_because}"
                )),
                Err(refusal) => {
                    self.finish_the_pending_stream_action_on_the_record(stream_id, action_id);
                    not_stopped.push(refusal.to_string());
                }
            }
        }
        progress.finish(if not_stopped.is_empty() {
            Ok(states)
        } else {
            Err(not_stopped.join("; "))
        });
    }

    /// Stop the stream whose id is `stream_id`, finishing the pending stop `action_id` on its
    /// record: a loaded recorded one is recorded stopped and its playback ended, its graph held
    /// to read and edit; a stream its host loaded is unloaded. A loaded recorded stream's
    /// playback ends even when its record cannot be read or written, and the outcome says why
    /// it was not recorded stopped. Refused — with nothing
    /// changed — naming the streams the runtime holds when it holds none of
    /// that id, and for a stream already stopped or failed.
    pub(super) fn stop_the_stream_finishing_its_pending_stop(
        &self,
        stream_id: &str,
        action_id: Option<&str>,
    ) -> Result<StreamStopOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        if let Some(hosted) = loaded
            .as_ref()
            .filter(|loaded| loaded.holding() == LoadedStreamHolding::LivesWithTheHostProcess)
        {
            unload_a_stream_an_action_took_back(hosted, "its owner stopped it");
            return Ok(StreamStopOutcome {
                stream_id: stream_id.to_string(),
                stop_not_recorded_because: None,
            });
        }
        let record = match self.current_stream_record_of_the_id(stream_id) {
            Ok(record) => record,
            Err(unreadable) => {
                let Some(loaded) = loaded else {
                    return Err(unreadable);
                };
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                return Ok(a_recorded_stream_unloaded_and_not_recorded_stopped(
                    stream_id.to_string(),
                    &unreadable,
                ));
            }
        };
        match (loaded, record) {
            (None, None) => Err(self.a_stream_this_runtime_does_not_hold(stream_id)),
            (None, Some(record)) if record.is_failed() => Err(Error::Runtime(format!(
                "the stream `{stream_id}` is failed, so it is not loaded to stop: {}. \
                 `reload_project` \
                 retries it, `rm` forgets its project",
                record.failed_because.as_deref().unwrap_or_default()
            ))),
            (None, Some(record)) if record.stopped => Err(Error::Runtime(format!(
                "the stream `{stream_id}` is already stopped; `start_playback` plays its project"
            ))),
            (None, Some(_)) => {
                self.record_the_stream_stopped_finishing_its_pending_stop(stream_id, action_id)?;
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    stop_not_recorded_because: None,
                })
            }
            (Some(loaded), Some(_)) => {
                let recorded_stopped =
                    self.record_the_stream_stopped_finishing_its_pending_stop(stream_id, action_id);
                self.stop_the_playback_of_a_held_stream(&loaded)?;
                Ok(match recorded_stopped {
                    Ok(()) => StreamStopOutcome {
                        stream_id: stream_id.to_string(),
                        stop_not_recorded_because: None,
                    },
                    Err(not_written) => a_recorded_stream_unloaded_and_not_recorded_stopped(
                        stream_id.to_string(),
                        &not_written,
                    ),
                })
            }
            (Some(loaded), None) => {
                unload_a_stream_an_action_took_back(&loaded, "its owner stopped it");
                Ok(StreamStopOutcome {
                    stream_id: stream_id.to_string(),
                    stop_not_recorded_because: None,
                })
            }
        }
    }

    /// End the playback of `loaded`, which stays held: every instance torn down, its graph
    /// kept to read and edit, and its function-node states forgotten so the next start begins
    /// fresh. A stream whose graph cannot be put back is unloaded, and refused naming why.
    pub(super) fn stop_the_playback_of_a_held_stream(
        &self,
        loaded: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<()> {
        let playback_stopped = loaded
            .log_route()
            .run_entered(|| loaded.stop_playback_keeping_the_graph());
        self.forget_the_function_node_states_of_the_stream(loaded.stream_id());
        let Err(not_kept) = playback_stopped else {
            return Ok(());
        };
        let why_it_is_unloaded = format!(
            "the stream `{}` stopped and its graph was not kept to edit, so it is unloaded: \
             {not_kept}",
            loaded.stream_id()
        );
        loaded
            .log_route()
            .run_entered(|| tracing::error!("{why_it_is_unloaded}"));
        unload_a_stream_an_action_took_back(loaded, "its graph was not kept at its stop");
        Err(Error::Runtime(why_it_is_unloaded))
    }

    /// Record the stream `stream_id` stopped, finishing the pending stop
    /// `action_id` on its record.
    pub(super) fn record_the_stream_stopped_finishing_its_pending_stop(
        &self,
        stream_id: &str,
        action_id: Option<&str>,
    ) -> Result<()> {
        self.change_the_record_of_the_stream(stream_id, |record| {
            record.stopped = true;
            if let Some(action_id) = action_id {
                record.finish_the_pending_stream_action(action_id);
            }
        })
    }

    /// Run the scaled clock of the loaded stream whose id is `stream_id` at
    /// `scale` from now on; stream time carries on from where it stands. Live
    /// only: the stream's graph document keeps the clock it names.
    pub fn set_stream_clock_scale(
        &self,
        stream_id: &str,
        scale: f64,
    ) -> Result<StreamClockReading> {
        let loaded = self.loaded_stream_of_the_id(stream_id)?;
        loaded
            .log_route()
            .run_entered(|| loaded.set_stream_clock_scale(scale))
    }

    /// Hold `stream`, loaded and never started, stopped: its graph there to read and edit and
    /// nothing running.
    pub(super) fn hold_a_loaded_stream_stopped(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> StreamStateAfterItsRun {
        stream.hold_stopped_without_a_start();
        StreamStateAfterItsRun::HeldStopped {
            stream_tag: stream.stream_tag(),
            node_count: stream.node_count(),
        }
    }

    /// Queue the start of the playback of the open project in `project_directory` on its stream
    /// action queue, answering the action's id at once: each stream it holds stopped starts
    /// fresh from the graph it holds, on a new clock. The action reads no document, makes no
    /// venv and loads nothing. Refused, with nothing queued, for a directory not named by its
    /// absolute path, a project that is not open, naming [`Self::open_project`], and a project
    /// that plays once its queued actions run — a paused one naming [`Self::resume_playback`].
    pub fn start_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        if !self.the_project_is_open_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(format!(
                "the project in {} is not open, so there is none to play; `open_project` \
                 opens it",
                project_directory.display()
            )));
        }
        if self.the_project_runs_once_its_queued_actions_run(&project_directory) {
            return Err(Error::Runtime(
                if self.is_the_project_paused(&project_directory) {
                    format!(
                        "the project in {} is paused, so there is none to play; \
                         `resume_playback` resumes it",
                        project_directory.display()
                    )
                } else {
                    format!(
                        "the project in {} already plays, so there is none to play; \
                         `stop_playback` stops it",
                        project_directory.display()
                    )
                },
            ));
        }
        Ok(self.queue_the_playback_of_the_held_project(project_directory))
    }

    /// Queue the start of the playback of every held stopped stream of the project in
    /// `project_directory`, answering the action's id at once. The action reads no document
    /// and makes no venv: each stream starts fresh from the graph it holds, paused while its
    /// project is.
    pub(super) fn queue_the_playback_of_the_held_project(
        &self,
        project_directory: PathBuf,
    ) -> StreamActionQueued {
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let streams = self
            .loaded_streams_of_the_project(&project_directory)
            .iter()
            .map(|held| stream_of_a_stream_action_not_yet_taken_in(held))
            .collect();
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::StartPlayback,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams,
            failed_because: None,
        };
        queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.start_the_playback_of_the_held_project(&action_id, &project_directory);
            }),
            &self.tokio_handle(),
        )
    }

    /// Run the playback start the stream action `action_id` queued of the project in
    /// `project_directory`: name the held stopped streams as the ones it takes in — an open
    /// queued ahead of it loads them only after this action is queued — record each running and
    /// start it, and finish the action. A stream whose start is refused is held stopped and recorded failed with the
    /// reason; a project holding no stream stopped fails the action.
    pub(super) fn start_the_playback_of_the_held_project(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Starting);
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let mut states = Vec::new();
        let held_streams: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .loaded_streams_of_the_project(project_directory)
            .into_iter()
            .filter(|loaded| loaded.is_held_stopped())
            .collect();
        if held_streams.is_empty() {
            progress.finish(Err(format!(
                "no stream of the project in {} is held stopped to play: each failed or none \
                 is loaded; `reload_project` retries a failed one",
                project_directory.display()
            )));
            return;
        }
        progress.works_on(
            held_streams
                .iter()
                .map(|held| stream_of_a_stream_action_not_yet_taken_in(held))
                .collect(),
        );
        for held in held_streams {
            if progress.is_cancelled() {
                break;
            }
            let stream_id = held.stream_id().to_string();
            if let Some(why_it_cannot_play) =
                held.why_its_graph_as_held_keeps_this_stream_from_playing()
            {
                held.log_route().run_entered(|| {
                    tracing::error!(
                        "the stream `{stream_id}` was not started, and is held stopped from its \
                         document: {why_it_cannot_play}"
                    )
                });
                states.push((stream_id, StreamListingState::Failed));
                continue;
            }
            if let Err(not_recorded) = self.change_the_record_of_the_stream(&stream_id, |record| {
                record.mark_running_resetting_its_crash_count();
            }) {
                held.log_route().run_entered(|| {
                    tracing::error!(
                        "the stream `{stream_id}` was not started: its record was not written \
                         running: {not_recorded}"
                    )
                });
                states.push((stream_id, StreamListingState::Stopped));
                continue;
            }
            let started = held
                .log_route()
                .run_entered(|| self.start_the_stream_paused_while_its_project_is(&held));
            match started {
                Ok(()) => states.push((
                    stream_id,
                    if is_paused(held.status()) {
                        StreamListingState::Paused
                    } else {
                        StreamListingState::Running
                    },
                )),
                Err(start_refusal) => {
                    held.log_route().run_entered(|| {
                        tracing::error!("the stream `{stream_id}` failed: {start_refusal}")
                    });
                    hold_a_stream_whose_start_was_refused_stopped(&held);
                    let still_held = !held.has_ended();
                    if let Err(not_recorded) =
                        self.change_the_record_of_the_stream(&stream_id, |record| {
                            if still_held {
                                record.mark_open_with_its_playback_stopped();
                            }
                            record.failed_because = Some(start_refusal.to_string());
                        })
                    {
                        tracing::error!("{not_recorded}");
                    }
                    states.push((stream_id, StreamListingState::Failed));
                }
            }
        }
        progress.finish(Ok(states));
    }

    /// Whether the project in `project_directory` is paused, or a loaded stream of it is.
    pub(super) fn is_the_project_paused(&self, project_directory: &Path) -> bool {
        self.stream_actions
            .directories_of_the_paused_projects
            .lock()
            .iter()
            .any(|paused| the_same_directory(paused, project_directory))
            || self
                .loaded_streams_of_the_project(project_directory)
                .iter()
                .any(|stream| is_paused(stream.status()))
    }
}

/// Ask `stream` for its shutdown, wait until it has ended, and log a failing
/// end in its own log — the action that unloaded it goes on regardless.
/// Hold `stream`, whose start was refused, stopped with its graph kept to read and edit;
/// unloaded instead while the machine shuts down, or when its graph is not put back.
/// `stream` as an action names it before taking it in.
pub(super) fn stream_of_a_stream_action_not_yet_taken_in(
    stream: &LoadedStreamInThisRuntime,
) -> StreamOfAStreamAction {
    StreamOfAStreamAction {
        stream_id: stream.stream_id().to_string(),
        display_name: stream.display_name(),
        stream_location: Some(stream.stream_location().clone()),
        state: None,
    }
}

/// The stop of the recorded stream `stream_id`, unloaded already, whose
/// record could not be marked stopped because of `not_recorded`.
pub(super) fn a_recorded_stream_unloaded_and_not_recorded_stopped(
    stream_id: String,
    not_recorded: &dyn std::fmt::Display,
) -> StreamStopOutcome {
    tracing::warn!(
        "the stream `{stream_id}` was unloaded, and is not recorded stopped, so a restart of \
         this runtime loads it again: {not_recorded}"
    );
    StreamStopOutcome {
        stream_id,
        stop_not_recorded_because: Some(not_recorded.to_string()),
    }
}
