// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// The next stream action id of this runtime.
    pub(super) fn next_stream_action_id(&self) -> String {
        self.stream_actions
            .stream_action_queue_of_each_project
            .next_action_id(
                self.engine_resources_shared_by_every_stream
                    .runtime_id
                    .as_str(),
            )
    }

    /// How the running stream action `action_id` reports its progress, it
    /// marked in progress.
    pub(super) fn progress_of_the_stream_action(
        &self,
        action_id: &str,
    ) -> StreamActionProgress<'_> {
        let progress = StreamActionProgress::of_the_stream_action(
            &self.stream_actions.stream_action_queue_of_each_project,
            action_id,
        );
        progress.has_begun();
        progress
    }

    /// The stream action `action_id`, refused when the engine never held it or
    /// let it go among its project's older finished actions; its records stay
    /// in the project's log.
    pub fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .held_stream_action(action_id)
            .ok_or_else(|| a_stream_action_not_held(action_id))
    }

    /// Every queued or running stream action, the earliest queued first.
    pub fn stream_actions_under_way(&self) -> Vec<StreamAction> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
    }

    /// Receive the id of each stream action as it is queued or changes — its state, its step,
    /// its streams or its venv sync — from now on.
    pub fn listen_for_each_stream_action_change(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.stream_actions
            .stream_action_queue_of_each_project
            .listen_for_each_stream_action_change()
    }

    /// Cancel the stream action `action_id`. One still queued is dropped,
    /// its pending marks taken off its streams' records, and finished
    /// cancelled. A running load stops at the step it is on and
    /// finishes cancelled once it sees the cancel: a stream it was loading is
    /// recorded failed, and the streams it had not reached stay as recorded.
    /// Refused for a running stop, close or removal, whose unload cannot be cut
    /// short, and for an action already finished or not held.
    pub fn cancel_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        let cancelled = self
            .stream_actions
            .stream_action_queue_of_each_project
            .cancel_the_stream_action(action_id, refuse_the_cancel_of_a_running_unload)?;
        let action = match cancelled {
            StreamActionCancelOutcome::DroppedFromItsQueue(action) => {
                for stream in &action.streams {
                    self.finish_the_pending_stream_action_on_the_record(
                        &stream.stream_id,
                        action_id,
                    );
                }
                action
            }
            StreamActionCancelOutcome::AskedOfTheRunningAction(action) => action,
        };
        ProjectLogRouteEntry::for_the_project_in(
            &action.project_directory,
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
            Some(action_id),
        )
        .run_entered(|| {
            tracing::info!(
                "the stream action {action_id} ({}) was asked to cancel while {}",
                action.kind.as_str(),
                action.state.as_str()
            )
        });
        Ok(action)
    }

    /// Show each stream an action under way works on `preparing` — or
    /// `stopping`, for an action that unloads it — with the action and its
    /// step, no `failed_because` and its condition as the action changes it, the earliest
    /// action on a stream winning:
    /// the streams the action names, else the project's listed ones.
    pub(super) fn show_the_streams_actions_under_way_prepare(
        &self,
        listings: &mut Vec<StreamListing>,
    ) {
        let mut streams_an_earlier_action_works_on: Vec<String> = Vec::new();
        for action in self
            .stream_actions
            .stream_action_queue_of_each_project
            .actions_under_way()
        {
            if !action.kind.works_on_its_projects_streams() {
                continue;
            }
            let working_on_it = StreamActionWorkingOnAStream {
                action_id: action.action_id.clone(),
                step: action.step,
            };
            let state_it_shows = if action.kind.unloads_its_streams() {
                StreamListingState::Stopping
            } else {
                StreamListingState::Preparing
            };
            let streams_it_works_on: Vec<(String, String, Option<StreamLocation>)> = if action
                .streams
                .is_empty()
            {
                let listed_of_the_project: Vec<(String, String, Option<StreamLocation>)> = listings
                    .iter()
                    .filter_map(|listing| {
                        let location = listing.stream_location.as_ref()?;
                        the_same_directory(&location.project_directory, &action.project_directory)
                            .then(|| {
                                (
                                    listing.stream_id.clone(),
                                    listing.display_name.clone(),
                                    Some(location.clone()),
                                )
                            })
                    })
                    .collect();
                if listed_of_the_project
                    .iter()
                    .any(|(stream_id, _, _)| streams_an_earlier_action_works_on.contains(stream_id))
                {
                    continue;
                }
                listed_of_the_project
            } else {
                action
                    .streams
                    .iter()
                    .filter(|stream| stream.state.is_none())
                    .map(|stream| {
                        (
                            stream.stream_id.clone(),
                            stream.display_name.clone(),
                            stream.stream_location.clone(),
                        )
                    })
                    .collect()
            };
            for (stream_id, display_name, stream_location) in streams_it_works_on {
                if streams_an_earlier_action_works_on.contains(&stream_id) {
                    continue;
                }
                streams_an_earlier_action_works_on.push(stream_id.clone());
                match listings
                    .iter_mut()
                    .find(|listing| listing.stream_id == stream_id)
                {
                    Some(listing) => {
                        listing.state = state_it_shows;
                        listing.failed_because = None;
                        listing.condition = the_condition_while_an_action_works_on_it(
                            action.kind,
                            listing.condition.clone(),
                        );
                        listing.stream_action_under_way = Some(working_on_it.clone());
                    }
                    None => listings.push(StreamListing {
                        stream_id,
                        display_name,
                        stream_location,
                        state: state_it_shows,
                        node_count: None,
                        failed_because: None,
                        condition: the_condition_while_an_action_works_on_it(
                            action.kind,
                            StreamConditionOnTheWire::healthy(
                                StreamLoadOnTheWire::NotLoaded,
                                StreamPlaybackOnTheWire::Stopped,
                            ),
                        ),
                        capture_devices_whose_access_was_never_asked_for: Vec::new(),
                        stream_action_under_way: Some(working_on_it.clone()),
                    }),
                }
            }
        }
    }
}
