// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// Finish, in order, every pending stream action a crash left on a
    /// record — a stop recorded stopped, a removal forgotten — before the runtime
    /// counts its previous run's end and serves any call.
    pub fn finish_the_pending_stream_actions_a_crash_left(&self) {
        if let Some(stream_records) = self.stream_records() {
            finish_the_pending_stream_actions_a_crash_left_on_the_records(stream_records);
        }
    }

    /// Queue one re-load per project holding a record not closed, as a runtime does at its
    /// start, and answer the actions. Each loads only those records of its project, a failed
    /// one held stopped from its document; a record failed for crashing the runtime, one an
    /// earlier runtime wrote and one that cannot be read are logged and left in place.
    pub fn queue_the_reload_of_every_project_at_the_start(&self) -> Vec<StreamActionQueued> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        let mut streams_of_each_project: Vec<(PathBuf, Vec<StreamOfAStreamAction>)> = Vec::new();
        for read in stream_records.read_every() {
            let record = match read {
                Ok(StreamRecordAsRead::Current(record)) => record,
                Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => {
                    tracing::warn!(
                        "the stream `{}` is failed, so it is not re-loaded: {}",
                        earlier.stream_id,
                        earlier.failed_because()
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    continue;
                }
            };
            if record.closed {
                tracing::info!(
                    "the stream `{}` is closed with its project, so it is not re-loaded",
                    record.stream_id
                );
                continue;
            }
            let project_directory = std::fs::canonicalize(&record.location.project_directory)
                .unwrap_or_else(|_| record.location.project_directory.clone());
            let stream = StreamOfAStreamAction {
                display_name: the_display_name_of_the_stream_at(
                    &record.location,
                    &record.stream_id,
                ),
                stream_id: record.stream_id,
                stream_location: Some(record.location),
                state: None,
            };
            match streams_of_each_project
                .iter_mut()
                .find(|(project, _)| *project == project_directory)
            {
                Some((_, streams)) => streams.push(stream),
                None => streams_of_each_project.push((project_directory, vec![stream])),
            }
        }
        streams_of_each_project
            .into_iter()
            .map(|(project_directory, streams)| {
                let action_id = self.next_stream_action_id();
                let stream_ids: Vec<String> = streams
                    .iter()
                    .map(|stream| stream.stream_id.clone())
                    .collect();
                let action = StreamAction {
                    action_id: action_id.clone(),
                    project_directory,
                    kind: StreamActionKind::ReloadAtStart,
                    state: StreamActionState::Queued,
                    step: None,
                    venv_sync: None,
                    streams,
                    failed_because: None,
                };
                self.stream_actions
                    .stream_action_queue_of_each_project
                    .queue_the_stream_action(
                        action,
                        Box::new(move |runner: &Runner| {
                            runner
                                .reload_the_records_for_its_stream_action(&action_id, &stream_ids);
                        }),
                        &self.tokio_handle(),
                    )
            })
            .collect()
    }

    /// Run the re-load at the start the stream action `action_id` queued of
    /// the records `stream_ids`, and finish the action done with each
    /// stream's state; a stream left unloaded but not recorded failed — an
    /// engine refusal or an interrupt, re-loaded at the next start — keeps
    /// no state.
    pub(super) fn reload_the_records_for_its_stream_action(
        &self,
        action_id: &str,
        stream_ids: &[String],
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Starting);
        let Some(stream_records) = self.stream_records() else {
            progress.finish(Ok(Vec::new()));
            return;
        };
        let listed = stream_ids
            .iter()
            .filter_map(|stream_id| stream_records.read(stream_id).transpose())
            .collect();
        let reloads = self.reload_each_recorded_stream_listed(stream_records, listed, &|| {
            progress.is_interrupted()
        });
        let states = reloads
            .iter()
            .filter_map(|reload| match reload {
                StreamReloadAtTheStart::Reloaded { stream_id } => {
                    Some((stream_id.clone(), StreamListingState::Running))
                }
                StreamReloadAtTheStart::HeldStopped { stream_id } => Some((
                    stream_id.clone(),
                    if self
                        .current_stream_record_of_the_id(stream_id)
                        .ok()
                        .flatten()
                        .is_some_and(|record| record.is_failed())
                    {
                        StreamListingState::Failed
                    } else {
                        StreamListingState::Stopped
                    },
                )),
                StreamReloadAtTheStart::NotReloaded { stream_id, .. } => self
                    .current_stream_record_of_the_id(stream_id)
                    .ok()
                    .flatten()
                    .is_some_and(|record| record.is_failed())
                    .then(|| (stream_id.clone(), StreamListingState::Failed)),
                StreamReloadAtTheStart::SkippedAsFailed { stream_id, .. } => {
                    Some((stream_id.clone(), StreamListingState::Failed))
                }
                StreamReloadAtTheStart::RecordUnreadable(_) => None,
            })
            .collect();
        progress.finish(Ok(states));
    }

    /// Re-load each record of `listed` from its stream asset's graph document as it is on
    /// disk. The local API serves while this runs, so each record is read again under the
    /// one-action lock and loaded from that read: a stream stopped or removed since the
    /// listing stays unloaded, and one loaded since is left as it is. A document that gives
    /// no graph records its stream failed. Once `is_interrupted`, the stream being re-loaded
    /// and every one after it stay unloaded and are re-loaded at the runtime's next start. A
    /// venv is never made under the lock: a record that was failed or stopped at the first
    /// read and is neither at the second is read again once its venv is made.
    pub(super) fn reload_each_recorded_stream_listed(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        listed: Vec<std::result::Result<StreamRecordAsRead, StreamRecordReadFailure>>,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Vec<StreamReloadAtTheStart> {
        let mut reloads = Vec::new();
        let mut listed = std::collections::VecDeque::from(listed);
        while let Some(listed_record) = listed.pop_front() {
            if is_interrupted() {
                break;
            }
            let stream_id = match listed_record {
                Ok(listed_record) => listed_record.stream_id().to_string(),
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(StreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            let venv_made_outside_the_lock = match stream_records.read(&stream_id) {
                Ok(Some(StreamRecordAsRead::Current(record)))
                    if !record.closed && record.is_failed_for_crashing_the_runtime() =>
                {
                    Some(Ok(()))
                }
                Ok(Some(StreamRecordAsRead::Current(record))) if !record.closed => {
                    Some(self.make_the_venv_of_a_recorded_stream(&record, is_interrupted))
                }
                _ => None,
            };
            let one_stream_action_at_a_time =
                self.stream_actions.one_stream_action_at_a_time.lock();
            let record = match stream_records.read(&stream_id) {
                Ok(Some(StreamRecordAsRead::Current(record))) => record,
                Ok(Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))) => {
                    let failed_because = earlier.failed_because();
                    tracing::warn!(
                        "the stream `{stream_id}` is failed, so it is not re-loaded: \
                         {failed_because}"
                    );
                    reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                        stream_id,
                        failed_because,
                    });
                    continue;
                }
                Ok(None) => {
                    tracing::info!(
                        "the stream `{stream_id}` was removed before its re-load, so it is not \
                         re-loaded"
                    );
                    continue;
                }
                Err(unreadable) => {
                    tracing::error!("{unreadable}; it is skipped and left in place");
                    reloads.push(StreamReloadAtTheStart::RecordUnreadable(unreadable));
                    continue;
                }
            };
            if record.crashed_the_runtime_even_held_from_its_document()
                && let Some(failed_because) = record.failed_because.clone()
            {
                tracing::error!(
                    "the stream `{stream_id}` crashed the runtime even held from its document, \
                     so it is not loaded at the start: {failed_because}; `tatolab reload` \
                     retries it"
                );
                reloads.push(StreamReloadAtTheStart::SkippedAsFailed {
                    stream_id,
                    failed_because,
                });
                continue;
            }
            if record.closed {
                tracing::info!(
                    "the stream `{stream_id}` is closed with its project, so it is not re-loaded"
                );
                continue;
            }
            if self
                .loaded_stream_of_the_id_when_loaded(&stream_id)
                .is_some()
            {
                tracing::info!(
                    "the stream `{stream_id}` was loaded before its re-load reached it, so it \
                     is left as it is"
                );
                continue;
            }
            let Some(venv_made_outside_the_lock) = venv_made_outside_the_lock else {
                drop(one_stream_action_at_a_time);
                listed.push_front(Ok(StreamRecordAsRead::Current(record)));
                continue;
            };
            let playback_after_the_load = if record.stopped || record.is_failed() {
                ProjectPlaybackAfterItsLoad::HeldStopped
            } else {
                ProjectPlaybackAfterItsLoad::Playing
            };
            if record.comes_back_paused_at_a_restart() {
                self.hold_the_project_paused(&record.location.project_directory);
            }
            let why_its_venv_was_not_made = match &venv_made_outside_the_lock {
                Err(venv_not_made) if !is_interrupted() => Some(format!(
                    "its `.venv` was not made as it re-loaded at the runtime's start: \
                     {venv_not_made}"
                )),
                _ => None,
            };
            let playback_after_the_load = if why_its_venv_was_not_made.is_some() {
                ProjectPlaybackAfterItsLoad::HeldStopped
            } else {
                playback_after_the_load
            };
            let venv_made_outside_the_lock = match venv_made_outside_the_lock {
                Err(_) if why_its_venv_was_not_made.is_some() => Ok(()),
                made_or_interrupted => made_or_interrupted,
            };
            let reloaded = venv_made_outside_the_lock.and_then(|()| {
                let graph = the_graph_the_recorded_stream_holds(&record).map_err(Error::Runtime)?;
                let display_name =
                    the_display_name_of_the_stream_at(&record.location, &record.stream_id);
                self.load_a_recorded_stream_from_its_record(
                    &record,
                    &display_name,
                    &graph,
                    CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor,
                    playback_after_the_load,
                    is_interrupted,
                )
            });
            match reloaded {
                Ok((_, Some(start_refusal @ Error::EngineResourceRefusedAtAStreamsStart(_)))) => {
                    tracing::error!(
                        "the stream `{stream_id}` is held stopped, its start refused, and is \
                         started again at the runtime's next start: {start_refusal}"
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: start_refusal,
                    });
                }
                Ok((_, Some(start_refusal))) => {
                    let failed_because =
                        format!("its start at the runtime's start was refused: {start_refusal}");
                    tracing::error!(
                        "the stream `{stream_id}` is held stopped, and is failed: {failed_because}"
                    );
                    if let Err(not_written) =
                        stream_records.change_the_record(&stream_id, |recorded| {
                            recorded.mark_open_with_its_playback_stopped();
                            recorded.failed_because = Some(failed_because);
                        })
                    {
                        tracing::error!(
                            "the stream `{stream_id}` was not recorded failed: {not_written}"
                        );
                    }
                    reloads.push(StreamReloadAtTheStart::HeldStopped { stream_id });
                }
                Ok((stream, None)) => {
                    let why_it_cannot_play = why_its_venv_was_not_made
                        .clone()
                        .or_else(|| stream.why_its_graph_as_held_keeps_this_stream_from_playing());
                    if let Some(why_it_cannot_play) = why_it_cannot_play
                        && !record.is_failed()
                    {
                        tracing::error!(
                            "the stream `{stream_id}` re-loaded held stopped from its document, \
                             and is failed: {why_it_cannot_play}"
                        );
                        if let Err(not_written) =
                            stream_records.change_the_record(&stream_id, |recorded| {
                                recorded.failed_because = Some(why_it_cannot_play);
                            })
                        {
                            tracing::error!(
                                "the stream `{stream_id}` was not recorded failed: {not_written}"
                            );
                        }
                        reloads.push(StreamReloadAtTheStart::HeldStopped { stream_id });
                        continue;
                    }
                    if let Some(failed_because) = &record.failed_because {
                        tracing::warn!(
                            "the stream `{stream_id}` is failed, so it is re-loaded held stopped \
                             from its document: {failed_because}; a play or `tatolab reload` \
                             retries it"
                        );
                        reloads.push(StreamReloadAtTheStart::HeldStopped { stream_id });
                        continue;
                    }
                    tracing::info!(
                        "the stream `{stream_id}` re-loaded from {} with {} nodes",
                        record.location,
                        stream.node_count()
                    );
                    reloads.push(match playback_after_the_load {
                        ProjectPlaybackAfterItsLoad::HeldStopped => {
                            StreamReloadAtTheStart::HeldStopped { stream_id }
                        }
                        ProjectPlaybackAfterItsLoad::Playing => {
                            StreamReloadAtTheStart::Reloaded { stream_id }
                        }
                    });
                }
                Err(reload_refusal @ Error::EngineResourceRefusedAtAStreamsStart(_)) => {
                    tracing::error!(
                        "the stream `{stream_id}` was not re-loaded, and is re-loaded at the \
                         runtime's next start: {reload_refusal}"
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) if is_interrupted() => {
                    tracing::info!(
                        "the stream `{stream_id}` was not re-loaded, because {}: \
                         {reload_refusal}",
                        what_interrupted_a_load()
                    );
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
                Err(reload_refusal) => {
                    let failed_because =
                        format!("it did not re-load at the runtime's start: {reload_refusal}");
                    match stream_records.change_the_record(&stream_id, |recorded| {
                        if !recorded.is_failed() {
                            recorded.failed_because = Some(failed_because);
                        }
                    }) {
                        Ok(_) => tracing::error!(
                            "the stream `{stream_id}` did not re-load, and is failed: \
                             {reload_refusal}; `tatolab reload` retries it"
                        ),
                        Err(not_written) => tracing::error!(
                            "the stream `{stream_id}` did not re-load: {reload_refusal}; and it \
                             was not recorded failed: {not_written}"
                        ),
                    }
                    reloads.push(StreamReloadAtTheStart::NotReloaded {
                        stream_id,
                        refusal: reload_refusal,
                    });
                }
            }
        }
        reloads
    }

    /// Add one to the crash count of the stream `stream_id`, already failed for crashing the
    /// runtime, which a clean end never resets: past
    /// [`RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM`] it crashed the runtime even held from
    /// its document, and is not loaded at a start at all.
    pub(super) fn count_a_crash_against_a_stream_already_failed_for_crashing(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_id: &str,
    ) {
        if let Err(not_written) = stream_records.change_the_record(stream_id, |recorded| {
            recorded.runtime_crashes_in_a_row_implicating_it += 1;
        }) {
            tracing::error!(
                "the stream `{stream_id}`'s count of the runtime's crashes was not recorded: \
                 {not_written}"
            );
        }
    }

    /// Count how the runtime's previous run ended against every current
    /// record not failed for crashing the runtime, before any re-load: a clean end resets each crash
    /// count; a crash adds one to each stream it was pinned on — failing it at
    /// [`RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM`] — and resets every
    /// other's. Returns the ids of the streams it failed.
    pub fn count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &self,
        how_the_previous_run_ended: &HowThePreviousRuntimeRunEnded,
    ) -> Vec<String> {
        let no_crash = CrashOfThePreviousRuntimeRun::default();
        let crash = match how_the_previous_run_ended {
            HowThePreviousRuntimeRunEnded::Cleanly => &no_crash,
            HowThePreviousRuntimeRunEnded::Crashed(crash) => {
                log_the_previous_runtime_runs_crash(crash);
                crash
            }
        };
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let mut streams_failed = Vec::new();
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(mut record)) = read else {
                continue;
            };
            if record.is_failed_for_crashing_the_runtime() {
                if crash
                    .causes_by_implicated_stream
                    .contains_key(&record.stream_id)
                {
                    self.count_a_crash_against_a_stream_already_failed_for_crashing(
                        stream_records,
                        &record.stream_id,
                    );
                }
                continue;
            }
            let crashes_in_a_row_before = record.runtime_crashes_in_a_row_implicating_it;
            match crash.causes_by_implicated_stream.get(&record.stream_id) {
                Some(cause) => {
                    record.runtime_crashes_in_a_row_implicating_it += 1;
                    let crashes_in_a_row = record.runtime_crashes_in_a_row_implicating_it;
                    if crashes_in_a_row >= RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM {
                        record.failed_because = Some(format!(
                            "it was implicated in the runtime's last {crashes_in_a_row} crashes \
                             in a row, the last: {cause}"
                        ));
                    }
                }
                None if crashes_in_a_row_before == 0 => continue,
                None => record.runtime_crashes_in_a_row_implicating_it = 0,
            }
            let counted = (
                record.runtime_crashes_in_a_row_implicating_it,
                record.failed_because.clone(),
            );
            if let Err(not_written) =
                stream_records.change_the_record(&record.stream_id, |recorded| {
                    (
                        recorded.runtime_crashes_in_a_row_implicating_it,
                        recorded.failed_because,
                    ) = counted;
                })
            {
                tracing::error!(
                    "the stream `{}`'s count of the runtime's crashes was not recorded: \
                     {not_written}",
                    record.stream_id
                );
                continue;
            }
            if record.is_failed_for_crashing_the_runtime()
                && let Some(failed_because) = &record.failed_because
            {
                tracing::error!(
                    "the stream `{}` is failed: {failed_because}; it is held at a start with \
                     none of its node code read until `tatolab reload` tries it",
                    record.stream_id
                );
                streams_failed.push(record.stream_id);
            }
        }
        streams_failed
    }

    /// Load `graph` as the stream `stream_record` records — writing the record once it is
    /// loaded when `record_it_once_loaded`, and abandoning it once
    /// `is_interrupted` reports true — then start it or hold it stopped as
    /// `playback_after_the_load` says. A refused load or start records the stream failed with
    /// the reason; a record that cannot be written unloads the stream and is refused.
    pub(super) fn load_a_recorded_stream_recording_it_failed_when_refused(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_record: &mut StreamRecord,
        display_name: &str,
        graph: &GraphSnapshot,
        record_it_once_loaded: bool,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<StreamStateAfterItsRun> {
        let loaded = self.load_a_recorded_stream(
            OneStreamAStreamActionLoads::recorded_by(stream_record, display_name, graph)
                .abandoned_once(is_interrupted),
        );
        let stream = match loaded {
            Ok(stream) => stream,
            Err(load_refusal) => {
                return record_a_refused_load_failed(stream_records, stream_record, load_refusal);
            }
        };
        if record_it_once_loaded
            && let Err(not_written) = stream_records
                .write_the_record_a_load_made(stream_record.clone())
                .map(|written| *stream_record = written)
        {
            unload_a_stream_an_action_took_back(&stream, "its record was not written");
            return Err(not_written);
        }
        if let Some(why_it_cannot_play) =
            stream.why_its_graph_as_held_keeps_this_stream_from_playing()
        {
            stream.hold_stopped_without_a_start();
            return record_a_stream_held_from_its_document_failed(
                stream_records,
                stream_record,
                why_it_cannot_play,
            );
        }
        if playback_after_the_load == ProjectPlaybackAfterItsLoad::HeldStopped {
            return Ok(self.hold_a_loaded_stream_stopped(&stream));
        }
        if let Err(start_refusal) = self.start_the_stream_paused_while_its_project_is(&stream) {
            hold_a_stream_whose_start_was_refused_stopped(&stream);
            if !stream.has_ended() {
                stream_record.mark_open_with_its_playback_stopped();
            }
            return record_a_refused_load_failed(stream_records, stream_record, start_refusal);
        }
        Ok(StreamStateAfterItsRun::Running {
            stream_tag: stream.stream_tag(),
            node_count: stream.node_count(),
        })
    }

    /// Make the `.venv` that `record`'s interpreter is in, unless it runs,
    /// refused for why when it cannot be made or `is_interrupted` cut it short.
    /// An interpreter in no project's `.venv` is left as it is.
    pub(super) fn make_the_venv_of_a_recorded_stream(
        &self,
        record: &StreamRecord,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<()> {
        let Some(project_directory) = the_project_directory_whose_venv_holds(&record.interpreter)
        else {
            return Ok(());
        };
        Ok(self.make_the_projects_venv(&project_directory, is_interrupted, None)?)
    }

    /// Load a recorded stream from `record` and the graph `graph` its document holds, started
    /// or held stopped as `playback_after_the_load` says, its capture devices asking the user
    /// as `capture_device_access_prompting` says, the load abandoned once `is_interrupted`
    /// reports true. A start that is refused holds the stream stopped, answered beside it.
    pub(super) fn load_a_recorded_stream_from_its_record(
        &self,
        record: &StreamRecord,
        display_name: &str,
        graph: &GraphSnapshot,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
        is_interrupted: &dyn Fn() -> bool,
    ) -> Result<(Arc<LoadedStreamInThisRuntime>, Option<Error>)> {
        let stream_to_load = OneStreamAStreamActionLoads::recorded_by(record, display_name, graph)
            .with_capture_device_access_prompting(capture_device_access_prompting)
            .reading_none_of_its_node_code(
                record
                    .failed_because
                    .clone()
                    .filter(|_| record.is_failed_for_crashing_the_runtime()),
            )
            .abandoned_once(is_interrupted);
        let stream = self.load_a_recorded_stream(stream_to_load)?;
        let start_refusal = self.start_a_loaded_recorded_stream_unless_it_is_held_stopped(
            &stream,
            playback_after_the_load,
        );
        stream.log_route().write_every_record_it_emitted_and_sync();
        match start_refusal {
            Some(start_refusal) if stream.has_ended() => Err(start_refusal),
            start_refusal => Ok((stream, start_refusal)),
        }
    }

    /// Start `stream` as `playback_after_the_load` says, or hold it stopped when that says so
    /// or its document names a node type not described; a refused start holds it stopped and
    /// is answered.
    pub(super) fn start_a_loaded_recorded_stream_unless_it_is_held_stopped(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
        playback_after_the_load: ProjectPlaybackAfterItsLoad,
    ) -> Option<Error> {
        let held_from_its_document = stream
            .why_its_graph_as_held_keeps_this_stream_from_playing()
            .is_some();
        if held_from_its_document
            || playback_after_the_load == ProjectPlaybackAfterItsLoad::HeldStopped
        {
            stream.hold_stopped_without_a_start();
            return None;
        }
        let start_refusal = self
            .start_the_stream_paused_while_its_project_is(stream)
            .err()?;
        hold_a_stream_whose_start_was_refused_stopped(stream);
        Some(start_refusal)
    }

    /// Load `stream_to_load` without starting it.
    pub(super) fn load_a_recorded_stream(
        &self,
        stream_to_load: OneStreamAStreamActionLoads<'_>,
    ) -> Result<Arc<LoadedStreamInThisRuntime>> {
        let OneStreamAStreamActionLoads {
            stream_id,
            display_name,
            stream_location,
            graph,
            stream_environment,
            capture_device_access_prompting,
            why_none_of_its_node_code_is_read,
            is_interrupted,
        } = stream_to_load;
        let load_options = OptionsForLoadingOneStream::at_stream_location(
            stream_id.to_string(),
            stream_location.clone(),
        )
        .in_stream_environment(stream_environment)
        .with_display_name(display_name)
        .with_capture_device_access_prompting(capture_device_access_prompting);
        let load_options = match why_none_of_its_node_code_is_read {
            Some(why_none_of_it_is_read) => {
                load_options.reading_none_of_its_node_code(why_none_of_it_is_read)
            }
            None => load_options,
        };
        let stream = match self.load_stream_held_as_unless_interrupted(
            graph,
            load_options,
            LoadedStreamHolding::RecordedByTheRuntime,
            is_interrupted,
        )? {
            StreamLoadObservingInterrupts::Loaded(stream) => stream,
            StreamLoadObservingInterrupts::AbandonedForAnInterrupt => {
                let why_it_was_abandoned = if is_the_machines_shutdown_requested() {
                    "the machine is shutting every stream down"
                } else {
                    "its stream action was cancelled"
                };
                return Err(Error::Runtime(format!(
                    "the stream `{stream_id}` was not loaded: {why_it_was_abandoned}"
                )));
            }
        };
        Ok(stream)
    }
}

/// The graph the document of `record`'s stream asset holds as it is on disk now, or why
/// there is none: the project is gone from where it was recorded, or no document holds the
/// stream asset, or the document holds no graph.
pub(super) fn the_graph_the_recorded_stream_holds(
    record: &StreamRecord,
) -> std::result::Result<GraphSnapshot, String> {
    let (project_file, stream_document) = the_graph_document_of_the_stream_at(&record.location)?;
    let project_directory = &record.location.project_directory;
    let asset_uuids_the_project_holds =
        every_asset_uuid_the_project_holds(project_directory, project_file.project)
            .map_err(|refusal| format!("its project's assets do not read: {refusal}"))?;
    the_graph_a_stream_document_holds(
        project_directory,
        &stream_document,
        &asset_uuids_the_project_holds,
    )
}

/// The project directory whose `.venv/bin/python` is `interpreter`; `None` when
/// it is not one.
pub(super) fn the_project_directory_whose_venv_holds(interpreter: &Path) -> Option<PathBuf> {
    let venv_bin = interpreter.parent()?;
    let venv = venv_bin.parent()?;
    if interpreter.file_name()? != "python"
        || venv_bin.file_name()? != "bin"
        || venv.file_name()? != ".venv"
    {
        return None;
    }
    venv.parent().map(Path::to_path_buf)
}

/// Record `stream_record` failed for `refusal`, its load or start refused, and
/// answer it failed — unless a machine shutdown abandoned it, when the refusal
/// is returned and the record left as it was.
pub(super) fn record_a_refused_load_failed(
    stream_records: &StreamRecordsInTheStateDirectory,
    stream_record: &mut StreamRecord,
    refusal: Error,
) -> Result<StreamStateAfterItsRun> {
    if is_the_machines_shutdown_requested() {
        return Err(refusal);
    }
    record_a_stream_held_from_its_document_failed(
        stream_records,
        stream_record,
        refusal.to_string(),
    )
}

/// Finish, in order, every pending stream action a crash left on a record
/// of `stream_records`: a removal forgets the record, and a stop records it
/// stopped.
pub(super) fn finish_the_pending_stream_actions_a_crash_left_on_the_records(
    stream_records: &StreamRecordsInTheStateDirectory,
) {
    for read in stream_records.read_every() {
        let Ok(StreamRecordAsRead::Current(record)) = read else {
            continue;
        };
        if record.pending_stream_actions.is_empty() {
            continue;
        }
        let stream_id = record.stream_id;
        let finished = stream_records.update(&stream_id, |recorded| {
            let Some(record) = recorded.as_mut() else {
                return;
            };
            let pending_stream_actions = std::mem::take(&mut record.pending_stream_actions);
            for pending in &pending_stream_actions {
                tracing::info!(
                    "the stream action {} left the stream `{stream_id}` pending its {:?}; \
                     it is finished now",
                    pending.action_id,
                    pending.kind
                );
            }
            if pending_stream_actions
                .iter()
                .any(|pending| pending.kind == PendingStreamActionKind::Remove)
            {
                *recorded = None;
                return;
            }
            for pending in pending_stream_actions {
                match pending.kind {
                    PendingStreamActionKind::Stop => record.stopped = true,
                    PendingStreamActionKind::Close => record.mark_closed(),
                    PendingStreamActionKind::Remove => {}
                }
            }
        });
        if let Err(not_finished) = finished {
            tracing::error!(
                "the pending stream actions of the stream `{stream_id}` were not finished: \
                 {not_finished}"
            );
        }
    }
}

/// Refuse a display name that is empty or only whitespace: a stream is always
/// shown under something.
/// Log what the runtime's previous run crashed on, a crash no stream owns as
/// the runtime bug it is.
pub(super) fn log_the_previous_runtime_runs_crash(crash: &CrashOfThePreviousRuntimeRun) {
    for (stream_id, cause) in &crash.causes_by_implicated_stream {
        tracing::error!(
            "the runtime's previous run crashed, implicating the stream `{stream_id}`: {cause}"
        );
    }
    for cause in &crash.causes_on_threads_no_stream_owns {
        tracing::error!(
            "the runtime's previous run crashed on a thread no stream owns: {cause}; this is a \
             runtime bug to file"
        );
    }
    if crash.left_no_trace() {
        tracing::warn!(
            "the runtime's previous run ended without a clean stop and recorded no crash — a \
             SIGKILL, or an out-of-memory kill; repeated, it is a runtime bug to file"
        );
    }
}
