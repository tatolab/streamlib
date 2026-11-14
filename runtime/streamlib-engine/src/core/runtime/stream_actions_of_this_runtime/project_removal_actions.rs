// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// Queue the removal of the whole project in `project_directory` on its
    /// queue, and answer the action: it unloads every loaded stream of the
    /// project and forgets every record of it, deleting no file. Each record
    /// carries the removal as a pending mark until it is forgotten. Refused
    /// for a directory not named by its absolute path, and for a project the
    /// runtime holds no stream of.
    pub fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let action_id = self.next_stream_action_id();
        let streams = self
            .every_stream_of_the_project_marked_for_its_removal(&action_id, &project_directory)?;
        if streams.is_empty() {
            return Err(Error::NotFound(format!(
                "this runtime holds no stream of the project in {}, so there is none to remove. \
                 Loaded: {}. Recorded: {}",
                project_directory.display(),
                self.loaded_stream_ids_listed_for_a_refusal(),
                self.recorded_stream_ids_listed_for_a_refusal()
            )));
        }
        Ok(self.queue_the_removal_of_the_project(
            action_id,
            project_directory,
            StreamActionKind::Remove,
            streams,
        ))
    }

    /// Queue the deletion of the whole project in `project_directory` on its
    /// queue, and answer the action: it unloads every loaded stream of the
    /// project and forgets every record of it, as a removal does, then moves the
    /// project's folder to this machine's Trash. Refused for a directory not
    /// named by its absolute path, and for one that holds no project.
    pub fn delete_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        if !project_directory.join(PROJECT_FILE_NAME).is_file() {
            return Err(Error::NotFound(format!(
                "{} holds no project: it has no {PROJECT_FILE_NAME}, so there is none to delete",
                project_directory.display()
            )));
        }
        let action_id = self.next_stream_action_id();
        let streams = self
            .every_stream_of_the_project_marked_for_its_removal(&action_id, &project_directory)?;
        Ok(self.queue_the_removal_of_the_project(
            action_id,
            project_directory,
            StreamActionKind::Delete,
            streams,
        ))
    }

    /// Every stream of the project in `project_directory`, recorded or loaded,
    /// each record marked with the pending removal `action_id`; a record that
    /// cannot be marked refuses the whole, every mark already made taken off.
    pub(super) fn every_stream_of_the_project_marked_for_its_removal(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) -> Result<Vec<StreamOfAStreamAction>> {
        let mut streams: Vec<StreamOfAStreamAction> = Vec::new();
        for stream_id in self.ids_of_every_record_of_the_project(project_directory) {
            let marked = match self.stream_records() {
                Some(stream_records) => {
                    match stream_records.change_the_record(&stream_id, |record| {
                        record.queue_the_pending_stream_action(
                            action_id,
                            PendingStreamActionKind::Remove,
                        );
                    }) {
                        Ok(marked) => marked,
                        Err(not_marked) => {
                            for stream_marked_already in &streams {
                                self.finish_the_pending_stream_action_on_the_record(
                                    &stream_marked_already.stream_id,
                                    action_id,
                                );
                            }
                            return Err(not_marked);
                        }
                    }
                }
                None => None,
            };
            streams.push(StreamOfAStreamAction {
                display_name: marked.as_ref().map_or_else(
                    || stream_id.clone(),
                    |record| the_display_name_of_the_stream_at(&record.location, &record.stream_id),
                ),
                stream_location: marked.map(|record| record.location),
                stream_id,
                state: None,
            });
        }
        for loaded in self.every_loaded_stream() {
            if the_same_directory(loaded.project_directory(), project_directory)
                && !streams
                    .iter()
                    .any(|stream| stream.stream_id == loaded.stream_id())
            {
                streams.push(StreamOfAStreamAction {
                    stream_id: loaded.stream_id().to_string(),
                    display_name: loaded.display_name(),
                    stream_location: Some(loaded.stream_location().clone()),
                    state: None,
                });
            }
        }
        Ok(streams)
    }

    /// Queue the removal `action_id` of the project in `project_directory`, a
    /// `Remove` or a `Delete`, over `streams`.
    pub(super) fn queue_the_removal_of_the_project(
        &self,
        action_id: String,
        project_directory: PathBuf,
        kind: StreamActionKind,
        streams: Vec<StreamOfAStreamAction>,
    ) -> StreamActionQueued {
        self.forget_that_the_project_is_paused(&project_directory);
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind,
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
                Box::new(move |runner: &Runner| match kind {
                    StreamActionKind::Delete => runner
                        .delete_the_project_for_its_stream_action(&action_id, &project_directory),
                    _ => runner
                        .remove_the_project_for_its_stream_action(&action_id, &project_directory),
                }),
                &self.tokio_handle(),
            )
    }

    /// Run the removal the stream action `action_id` queued of the project in
    /// `project_directory`: unload every loaded stream of the project and
    /// forget every record of it, and finish the action.
    pub(super) fn remove_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let not_forgotten = self.unload_and_forget_every_stream_of_the_project(project_directory);
        progress.finish(if not_forgotten.is_empty() {
            Ok(Vec::new())
        } else {
            Err(the_records_not_forgotten(&not_forgotten))
        });
    }

    /// Run the deletion the stream action `action_id` queued of the project in
    /// `project_directory`: unload and forget every stream of it as a removal
    /// does, then move its folder to the Trash, and finish the action. A record
    /// left unforgotten leaves the folder where it is.
    pub(super) fn delete_the_project_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::Stopping);
        let not_forgotten = self.unload_and_forget_every_stream_of_the_project(project_directory);
        if !not_forgotten.is_empty() {
            progress.finish(Err(format!(
                "{}, so the project's folder is left where it is",
                the_records_not_forgotten(&not_forgotten)
            )));
            return;
        }
        progress.finish(
            move_the_project_folder_to_the_trash(project_directory)
                .map(|()| Vec::new())
                .map_err(|not_moved| {
                    format!(
                        "the project's streams are unloaded and forgotten, and its folder {} \
                         was not moved to the Trash: {not_moved}",
                        project_directory.display()
                    )
                }),
        );
    }

    /// Unload every loaded stream of the project in `project_directory` and
    /// forget every record of it, answering why each record that was not
    /// forgotten was not.
    pub(super) fn unload_and_forget_every_stream_of_the_project(
        &self,
        project_directory: &Path,
    ) -> Vec<String> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded_of_the_project: Vec<Arc<LoadedStreamInThisRuntime>> = self
            .every_loaded_stream()
            .into_iter()
            .filter(|loaded| the_same_directory(loaded.project_directory(), project_directory))
            .collect();
        let mut not_forgotten = Vec::new();
        let mut ids_of_the_records_forgotten = Vec::new();
        if let Some(stream_records) = self.stream_records() {
            for stream_id in self.ids_of_every_record_of_the_project(project_directory) {
                match stream_records.remove(&stream_id) {
                    Ok(_) => ids_of_the_records_forgotten.push(stream_id),
                    Err(not_removed) => not_forgotten.push(not_removed.to_string()),
                }
            }
        }
        unload_every_stream_an_action_took_back(
            &loaded_of_the_project,
            "its owner removed its project",
        );
        for stream_id in ids_of_the_records_forgotten {
            self.forget_the_function_node_states_of_the_stream(&stream_id);
        }
        not_forgotten
    }

    /// Unload and forget every record of `project` whose stream asset is not among
    /// `stream_assets_the_project_holds`: its graph document is gone, or another stream of the
    /// project places it, so it runs only there. A record of the project that another copy of it
    /// holds is left as it is; one that cannot be removed is logged.
    pub(super) fn forget_every_record_of_the_project_whose_stream_asset_is_gone(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        project: &ProjectAsALoadReadsIt,
        stream_assets_the_project_holds: &[Uuid],
    ) {
        for read in stream_records.read_every() {
            let Ok(StreamRecordAsRead::Current(record)) = read else {
                continue;
            };
            if record.location.project != project.project_file.project
                || stream_assets_the_project_holds.contains(&record.location.stream_asset)
                || another_copy_of_the_project_holds(&record.location, &project.project_directory)
            {
                continue;
            }
            let why_it_is_forgotten = if project
                .stream_assets_another_stream_places
                .contains(&record.location.stream_asset)
            {
                "another stream of its project places it, so it runs only inside that stream"
                    .to_owned()
            } else {
                format!(
                    "no graph document of {} holds its stream asset {} any more",
                    project.project_directory.display(),
                    record.location.stream_asset
                )
            };
            if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(&record.stream_id) {
                unload_a_stream_an_action_took_back(&loaded, &why_it_is_forgotten);
            }
            match stream_records.remove(&record.stream_id) {
                Ok(_) => {
                    self.forget_the_function_node_states_of_the_stream(&record.stream_id);
                    tracing::info!(
                        "the stream `{}` is forgotten: {why_it_is_forgotten}",
                        record.stream_id,
                    )
                }
                Err(not_removed) => tracing::error!(
                    "the stream `{}` has no graph document any more, and its record was not \
                     removed: {not_removed}",
                    record.stream_id
                ),
            }
        }
    }

    /// Forget every function-node state of `stream_id`, in memory and in its project's
    /// store, as its playback ends, its project closes or its record is forgotten.
    pub(super) fn forget_the_function_node_states_of_the_stream(&self, stream_id: &str) {
        self.engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream
            .forget_every_state_of_the_stream(stream_id);
    }

    /// Unload and forget the stream of `project`'s stream asset `stream_asset`, whose graph
    /// document was deleted, answering its id; `None` when the runtime records none.
    pub fn forget_the_stream_of_a_deleted_stream_asset(
        &self,
        project: Uuid,
        stream_asset: Uuid,
    ) -> Result<Option<String>> {
        let Some(stream_records) = self.stream_records() else {
            return Ok(None);
        };
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let Some(record) = stream_records
            .read_every()
            .into_iter()
            .find_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if record.location.project == project
                        && record.location.stream_asset == stream_asset =>
                {
                    Some(record)
                }
                _ => None,
            })
        else {
            return Ok(None);
        };
        if let Some(loaded) = self.loaded_stream_of_the_id_when_loaded(&record.stream_id) {
            unload_a_stream_an_action_took_back(&loaded, "its stream asset was deleted");
        }
        stream_records.remove(&record.stream_id)?;
        self.forget_the_function_node_states_of_the_stream(&record.stream_id);
        Ok(Some(record.stream_id))
    }
}

/// Why a removal finished failed: the records in `not_forgotten`, each with why.
pub(super) fn the_records_not_forgotten(not_forgotten: &[String]) -> String {
    format!(
        "the project's streams are unloaded, and these records were not forgotten, so \
         they stay listed: {}",
        not_forgotten.join("; ")
    )
}

/// Ask every one of `streams` for its shutdown at once, so their teardowns run
/// side by side within one teardown's budget, then unload each as
/// [`unload_a_stream_an_action_took_back`] does.
pub(super) fn unload_every_stream_an_action_took_back(
    streams: &[Arc<LoadedStreamInThisRuntime>],
    reason: &str,
) {
    for stream in streams {
        stream.ask_for_this_streams_shutdown(reason);
    }
    for stream in streams {
        unload_a_stream_an_action_took_back(stream, reason);
    }
}
