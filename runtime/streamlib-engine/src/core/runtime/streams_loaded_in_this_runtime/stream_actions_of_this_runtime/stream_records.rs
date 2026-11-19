// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// Record this runtime's streams in `streams_directory`, created
    /// owner-only when absent. Refused a second time; a runtime never given
    /// one records no stream, and so runs none.
    pub fn record_streams_in_the_state_directory(&self, streams_directory: &Path) -> Result<()> {
        if let Some(stream_records) = self.stream_records() {
            return Err(Error::Configuration(format!(
                "this runtime already records its streams in {}; it records them in one \
                 directory for its life",
                stream_records.streams_directory().display()
            )));
        }
        let stream_records = Arc::new(StreamRecordsInTheStateDirectory::open(streams_directory)?);
        self.stream_actions
            .stream_records
            .set(stream_records)
            .map_err(|_| {
                Error::Configuration(format!(
                    "this runtime was handed a streams directory twice at once; it records its \
                     streams in {} only",
                    streams_directory.display()
                ))
            })?;
        Ok(())
    }

    /// Every loaded stream of the project in `project_directory`.
    pub(super) fn loaded_streams_of_the_project(
        &self,
        project_directory: &Path,
    ) -> Vec<Arc<LoadedStreamInThisRuntime>> {
        self.every_loaded_stream()
            .into_iter()
            .filter(|loaded| the_same_directory(loaded.project_directory(), project_directory))
            .collect()
    }

    /// The ids of every record of the project in `project_directory`, those an
    /// earlier runtime wrote that name it included.
    pub(super) fn ids_of_every_record_of_the_project(
        &self,
        project_directory: &Path,
    ) -> Vec<String> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        stream_records
            .read_every()
            .into_iter()
            .filter_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if the_same_directory(
                        &record.location.project_directory,
                        project_directory,
                    ) =>
                {
                    Some(record.stream_id)
                }
                Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier))
                    if earlier
                        .project_directory
                        .as_deref()
                        .is_some_and(|recorded| {
                            the_same_directory(recorded, project_directory)
                        }) =>
                {
                    Some(earlier.stream_id)
                }
                _ => None,
            })
            .collect()
    }

    /// Take the pending mark of the action `action_id` off the record of
    /// `stream_id`, when it has one.
    pub(super) fn finish_the_pending_stream_action_on_the_record(
        &self,
        stream_id: &str,
        action_id: &str,
    ) {
        let Some(stream_records) = self.stream_records() else {
            return;
        };
        if let Err(not_finished) = stream_records.change_the_record(stream_id, |record| {
            record.finish_the_pending_stream_action(action_id);
        }) {
            tracing::error!(
                "the stream action {action_id} stays pending on the record of the stream \
                 `{stream_id}`: {not_finished}"
            );
        }
    }

    /// Why the record of `stream`, a stream the runtime records, holds it failed; `None` when
    /// it records it not failed, or records none.
    pub(super) fn why_the_record_of_a_loaded_stream_holds_it_failed(
        &self,
        stream: &LoadedStreamInThisRuntime,
    ) -> Option<StreamFailure> {
        if stream.holding() != LoadedStreamHolding::RecordedByTheRuntime {
            return None;
        }
        match self.stream_record_of_the_id(stream.stream_id()) {
            Ok(Some(StreamRecordAsRead::Current(record))) => record
                .failure()
                .map(|(kind, words)| StreamFailure::of_the_stream(kind, words.to_owned())),
            _ => None,
        }
    }

    /// Every stream the runtime holds, by id: each loaded stream, then each
    /// record not loaded — one an earlier runtime wrote listed failed. A record
    /// that cannot be read is logged and left out.
    pub fn list_streams(&self) -> Vec<StreamListing> {
        let mut listings: Vec<StreamListing> = self
            .every_loaded_stream()
            .iter()
            .map(|stream| {
                let why_it_cannot_play = stream
                    .why_its_graph_as_held_keeps_this_stream_from_playing()
                    .or_else(|| self.why_the_record_of_a_loaded_stream_holds_it_failed(stream));
                let playback = if is_paused(stream.status()) {
                    StreamPlaybackOnTheWire::Paused
                } else if why_it_cannot_play.is_some() || stream.is_held_stopped() {
                    StreamPlaybackOnTheWire::Stopped
                } else {
                    StreamPlaybackOnTheWire::Playing
                };
                let condition = the_condition_of_a_stream(
                    StreamLoadOnTheWire::Loaded,
                    playback,
                    why_it_cannot_play.as_ref().map(StreamFailure::diagnostic),
                    stream.diagnostics_of_its_nodes(),
                );
                StreamListing {
                    stream_id: stream.stream_id().to_string(),
                    display_name: stream.display_name(),
                    stream_location: Some(stream.stream_location().clone()),
                    state: if why_it_cannot_play.is_some() {
                        StreamListingState::Failed
                    } else if is_paused(stream.status()) {
                        StreamListingState::Paused
                    } else if stream.is_held_stopped() {
                        StreamListingState::Stopped
                    } else {
                        StreamListingState::Running
                    },
                    node_count: Some(stream.node_count()),
                    failed_because: why_it_cannot_play.map(|failure| failure.words),
                    condition,
                    capture_devices_whose_access_was_never_asked_for: stream
                        .capture_devices_whose_access_was_never_asked_for(),
                    stream_action_under_way: None,
                }
            })
            .collect();
        if let Some(stream_records) = self.stream_records() {
            for read in stream_records.read_every() {
                let listing = match read {
                    Ok(StreamRecordAsRead::Current(record)) => StreamListing {
                        state: record.listing_state(),
                        condition: the_condition_of_a_stream_recorded_not_loaded(&record),
                        display_name: the_display_name_of_the_stream_at(
                            &record.location,
                            &record.stream_id,
                        ),
                        stream_id: record.stream_id,
                        stream_location: Some(record.location),
                        node_count: None,
                        failed_because: record.failed_because,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        stream_action_under_way: None,
                    },
                    Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => StreamListing {
                        state: StreamListingState::Failed,
                        condition: the_condition_of_a_stream(
                            StreamLoadOnTheWire::NotLoaded,
                            StreamPlaybackOnTheWire::Stopped,
                            Some(DiagnosticOnTheWire::of(
                                DiagnosticKindOnTheWire::RecordedByAnEarlierRuntime,
                                DiagnosticPlaceOnTheWire::Stream,
                                earlier.failed_because(),
                            )),
                            Vec::new(),
                        ),
                        failed_because: Some(earlier.failed_because()),
                        display_name: earlier.stream_id.clone(),
                        stream_id: earlier.stream_id,
                        stream_location: None,
                        node_count: None,
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        stream_action_under_way: None,
                    },
                    Err(unreadable) => {
                        tracing::warn!("{unreadable}; it is left out of the stream listing");
                        continue;
                    }
                };
                if listings
                    .iter()
                    .any(|listed| listed.stream_id == listing.stream_id)
                {
                    continue;
                }
                listings.push(listing);
            }
        }
        self.show_the_streams_actions_under_way_prepare(&mut listings);
        listings.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
        listings
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    pub(super) fn stream_records(&self) -> Option<&StreamRecordsInTheStateDirectory> {
        self.stream_actions.stream_records.get().map(Arc::as_ref)
    }

    pub(super) fn stream_record_of_the_id(
        &self,
        stream_id: &str,
    ) -> Result<Option<StreamRecordAsRead>> {
        match self.stream_records() {
            Some(stream_records) => Ok(stream_records.read(stream_id)?),
            None => Ok(None),
        }
    }

    /// The record of `stream_id` this runtime writes, refused naming why a
    /// record an earlier runtime wrote is failed.
    pub(super) fn current_stream_record_of_the_id(
        &self,
        stream_id: &str,
    ) -> Result<Option<StreamRecord>> {
        match self.stream_record_of_the_id(stream_id)? {
            Some(StreamRecordAsRead::Current(record)) => Ok(Some(record)),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier)) => {
                Err(Error::Runtime(format!(
                    "the stream `{stream_id}` is failed: {}",
                    earlier.failed_because()
                )))
            }
            None => Ok(None),
        }
    }

    /// Change the record of the stream `stream_id` with `edit` when there is
    /// one, refused when the runtime records no stream.
    pub(super) fn change_the_record_of_the_stream(
        &self,
        stream_id: &str,
        edit: impl FnOnce(&mut StreamRecord),
    ) -> Result<()> {
        match self.stream_records() {
            Some(stream_records) => stream_records.change_the_record(stream_id, edit).map(drop),
            None => Err(Error::Configuration(format!(
                "the stream `{stream_id}` was not recorded: this runtime records no stream"
            ))),
        }
    }

    pub(super) fn a_stream_this_runtime_does_not_hold(&self, stream_id: &str) -> Error {
        Error::NotFound(format!(
            "no stream with the id `{stream_id}` is loaded or recorded in this runtime. Loaded: \
             {}. Recorded: {}",
            self.loaded_stream_ids_listed_for_a_refusal(),
            self.recorded_stream_ids_listed_for_a_refusal()
        ))
    }

    pub(super) fn recorded_stream_ids_listed_for_a_refusal(&self) -> String {
        let recorded_ids: Vec<String> = self
            .stream_records()
            .map(|stream_records| {
                stream_records
                    .read_every()
                    .into_iter()
                    .filter_map(|read| read.ok())
                    .map(|read| match read {
                        StreamRecordAsRead::Current(record) => match record.listing_state() {
                            StreamListingState::Running => record.stream_id,
                            state => format!("{} ({state})", record.stream_id),
                        },
                        StreamRecordAsRead::RecordedByAnEarlierRuntime(earlier) => format!(
                            "{} ({}, recorded by an earlier runtime)",
                            earlier.stream_id,
                            StreamListingState::Failed
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();
        names_listed_for_a_refusal(recorded_ids, "none")
    }

    /// The record of the stream at `stream_location`, its project and stream asset, when one
    /// is recorded, wherever the project sat when it was.
    pub(super) fn the_record_of_the_stream_at(
        &self,
        stream_records: &StreamRecordsInTheStateDirectory,
        stream_location: &StreamLocation,
    ) -> Option<StreamRecord> {
        stream_records
            .read_every()
            .into_iter()
            .find_map(|read| match read {
                Ok(StreamRecordAsRead::Current(record))
                    if record.location.is_the_same_stream_as(stream_location) =>
                {
                    Some(record)
                }
                _ => None,
            })
    }

    /// Every record of the project in `project_directory` that reads.
    pub(super) fn current_records_of_the_project(
        &self,
        project_directory: &Path,
    ) -> Vec<StreamRecord> {
        self.ids_of_every_record_of_the_project(project_directory)
            .iter()
            .filter_map(|stream_id| {
                self.current_stream_record_of_the_id(stream_id)
                    .ok()
                    .flatten()
            })
            .collect()
    }
}

/// The condition of the stream `record` records, not loaded: paused, stopped or playing as it is
/// recorded, a stream closed or failed stopped, and failed as its record says.
fn the_condition_of_a_stream_recorded_not_loaded(
    record: &StreamRecord,
) -> StreamConditionOnTheWire {
    let failure = record.failure();
    let playback = if record.paused {
        StreamPlaybackOnTheWire::Paused
    } else if record.stopped || record.closed || failure.is_some() {
        StreamPlaybackOnTheWire::Stopped
    } else {
        StreamPlaybackOnTheWire::Playing
    };
    the_condition_of_a_stream(
        StreamLoadOnTheWire::NotLoaded,
        playback,
        failure.map(|(kind, words)| {
            DiagnosticOnTheWire::of(
                kind.on_the_wire(),
                DiagnosticPlaceOnTheWire::Stream,
                words.to_owned(),
            )
        }),
        Vec::new(),
    )
}
