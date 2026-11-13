// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What one open control connection subscribed to: projects' change feeds, loaded streams' live
//! graphs, read from `PUBSUB` on each stream's topic, and the stream actions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use streamlib::sdk::error::Result as StreamlibResult;
use streamlib::sdk::processors::ProcessorState;
use streamlib::sdk::pubsub::{
    Event, EventListener, LoadedStreamIdentity, PUBSUB, RuntimeEvent, topics,
};
use streamlib::sdk::runtime::OperationsOnTheStreamsLoadedInThisRuntime;
use streamlib_control_contract::{
    NodeStateOnTheWire, StreamGraphChangeKindOnTheWire, StreamGraphChangedNotificationParams,
};
use streamlib_project_repository::ProjectFilesWatchedWhileHeld;
use tokio::sync::mpsc;
use uuid::Uuid;

/// The subscriptions of one connection that stays open to be sent notifications.
pub(crate) struct ControlSubscriptionsOfOneConnection {
    subscribed_projects_with_their_folders_watched:
        Mutex<BTreeMap<Uuid, ProjectFilesWatchedWhileHeld>>,
    subscribed_project_logs_by_directory_with_symlinks_resolved:
        Mutex<BTreeMap<PathBuf, ProjectLogSubscribed>>,
    followed_stream_graphs: Mutex<BTreeMap<String, Option<StreamGraphFollowedOnOneLoad>>>,
    stream_graph_changes_to_the_connection: mpsc::Sender<StreamGraphChangedNotificationParams>,
    subscribed_to_stream_actions: AtomicBool,
}

impl std::fmt::Debug for ControlSubscriptionsOfOneConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlSubscriptionsOfOneConnection")
            .field(
                "subscribed_projects_with_their_folders_watched",
                &*self.subscribed_projects_with_their_folders_watched.lock(),
            )
            .field(
                "subscribed_project_logs_by_directory_with_symlinks_resolved",
                &*self
                    .subscribed_project_logs_by_directory_with_symlinks_resolved
                    .lock(),
            )
            .field(
                "followed_stream_graphs",
                &self
                    .followed_stream_graphs
                    .lock()
                    .keys()
                    .collect::<Vec<_>>(),
            )
            .field(
                "subscribed_to_stream_actions",
                &self.subscribed_to_stream_actions.load(Ordering::Relaxed),
            )
            .finish()
    }
}

/// A project log a subscription names: its directory as the connection spelled it and with
/// symlinks resolved, and the cursor at its end as the subscription arrived.
pub(crate) struct ProjectLogAsSubscribedWithItsEnd {
    pub(crate) project_directory_as_subscribed: PathBuf,
    pub(crate) project_directory_with_symlinks_resolved: PathBuf,
    pub(crate) cursor_at_the_logs_end: String,
}

/// One project log the connection subscribed to.
#[derive(Debug)]
struct ProjectLogSubscribed {
    directories_as_subscribed: BTreeSet<std::ffi::OsString>,
    cursor_the_next_push_follows: String,
}

/// Where a subscribed project log's next push starts, and each directory it is sent as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectLogNextPush {
    /// Each directory the connection subscribed to the project's log as.
    pub(crate) directories_as_subscribed: Vec<PathBuf>,
    /// The cursor the next push's records follow.
    pub(crate) cursor_the_next_push_follows: String,
}

/// One followed stream as it is loaded now, and the listener that keeps its topic subscribed.
struct StreamGraphFollowedOnOneLoad {
    loaded_stream: LoadedStreamIdentity,
    _listener_kept_subscribed: Arc<Mutex<dyn EventListener>>,
}

impl ControlSubscriptionsOfOneConnection {
    /// No subscription yet; a followed stream's graph changes go to
    /// `stream_graph_changes_to_the_connection`.
    pub(crate) fn new(
        stream_graph_changes_to_the_connection: mpsc::Sender<StreamGraphChangedNotificationParams>,
    ) -> Self {
        Self {
            subscribed_projects_with_their_folders_watched: Mutex::new(BTreeMap::new()),
            subscribed_project_logs_by_directory_with_symlinks_resolved: Mutex::new(BTreeMap::new()),
            followed_stream_graphs: Mutex::new(BTreeMap::new()),
            stream_graph_changes_to_the_connection,
            subscribed_to_stream_actions: AtomicBool::new(false),
        }
    }

    /// Whether the connection subscribed to `project`'s change feed.
    pub(crate) fn holds_the_project(&self, project: Uuid) -> bool {
        self.subscribed_projects_with_their_folders_watched
            .lock()
            .contains_key(&project)
    }

    /// Every project whose change feed the connection subscribed to.
    pub(crate) fn every_subscribed_project(&self) -> Vec<Uuid> {
        self.subscribed_projects_with_their_folders_watched
            .lock()
            .keys()
            .copied()
            .collect()
    }

    /// Subscribe to `projects`' change feeds, holding the watch `watch_the_files_of` gives on
    /// each one's folder for as long as the connection stays open, and answering every project
    /// subscribed to.
    pub(crate) fn subscribe_to_the_projects(
        &self,
        projects: Vec<Uuid>,
        watch_the_files_of: impl Fn(Uuid) -> ProjectFilesWatchedWhileHeld,
    ) -> Vec<Uuid> {
        let mut subscribed_projects = self.subscribed_projects_with_their_folders_watched.lock();
        for project in projects {
            subscribed_projects
                .entry(project)
                .or_insert_with(|| watch_the_files_of(project));
        }
        subscribed_projects.keys().copied().collect()
    }

    /// Where the next push of the log of the project in `project_directory_with_symlinks_resolved`
    /// starts; none when the connection did not subscribe to it.
    pub(crate) fn the_next_push_of_the_project_log(
        &self,
        project_directory_with_symlinks_resolved: &Path,
    ) -> Option<ProjectLogNextPush> {
        self.subscribed_project_logs_by_directory_with_symlinks_resolved
            .lock()
            .get(project_directory_with_symlinks_resolved)
            .map(|subscribed| ProjectLogNextPush {
                directories_as_subscribed: subscribed
                    .directories_as_subscribed
                    .iter()
                    .cloned()
                    .map(PathBuf::from)
                    .collect(),
                cursor_the_next_push_follows: subscribed.cursor_the_next_push_follows.clone(),
            })
    }

    /// Every project whose log the connection subscribed to, by its directory with symlinks
    /// resolved.
    pub(crate) fn every_subscribed_project_log_with_symlinks_resolved(&self) -> Vec<PathBuf> {
        self.subscribed_project_logs_by_directory_with_symlinks_resolved
            .lock()
            .keys()
            .cloned()
            .collect()
    }

    /// Start the next push of the log of the project in `project_directory_with_symlinks_resolved`
    /// at `cursor_after_the_push`, if a push from `cursor_the_push_followed` was the last one.
    pub(crate) fn move_the_project_log_cursor_past_a_push(
        &self,
        project_directory_with_symlinks_resolved: &Path,
        cursor_the_push_followed: &str,
        cursor_after_the_push: String,
    ) {
        if let Some(subscribed) = self
            .subscribed_project_logs_by_directory_with_symlinks_resolved
            .lock()
            .get_mut(project_directory_with_symlinks_resolved)
            .filter(|subscribed| {
                subscribed.cursor_the_next_push_follows == cursor_the_push_followed
            })
        {
            subscribed.cursor_the_next_push_follows = cursor_after_the_push;
        }
    }

    /// Subscribe to the log of each project, keyed by its directory as subscribed, spelled exactly
    /// as the connection spelled it, and with symlinks resolved, its first push starting at the
    /// cursor at its log's end, answering every directory a project log is subscribed to as. A
    /// project already subscribed keeps the cursor its next push follows.
    pub(crate) fn subscribe_to_the_project_logs(
        &self,
        project_logs_as_subscribed_with_symlinks_resolved_and_their_end: Vec<
            ProjectLogAsSubscribedWithItsEnd,
        >,
    ) -> Vec<PathBuf> {
        let mut subscribed_project_logs = self
            .subscribed_project_logs_by_directory_with_symlinks_resolved
            .lock();
        for ProjectLogAsSubscribedWithItsEnd {
            project_directory_as_subscribed,
            project_directory_with_symlinks_resolved,
            cursor_at_the_logs_end,
        } in project_logs_as_subscribed_with_symlinks_resolved_and_their_end
        {
            subscribed_project_logs
                .entry(project_directory_with_symlinks_resolved)
                .or_insert_with(|| ProjectLogSubscribed {
                    directories_as_subscribed: BTreeSet::new(),
                    cursor_the_next_push_follows: cursor_at_the_logs_end,
                })
                .directories_as_subscribed
                .insert(project_directory_as_subscribed.into_os_string());
        }
        subscribed_project_logs
            .values()
            .flat_map(|subscribed| subscribed.directories_as_subscribed.iter().cloned())
            .map(PathBuf::from)
            .collect()
    }

    /// Follow the live graph of each stream, as it is loaded now, answering every stream
    /// followed.
    pub(crate) fn follow_the_stream_graphs(
        &self,
        streams_as_loaded_now: Vec<LoadedStreamIdentity>,
    ) -> StreamlibResult<Vec<String>> {
        let mut followed_stream_graphs = self.followed_stream_graphs.lock();
        for loaded_stream in streams_as_loaded_now {
            let already_followed_on_this_load = followed_stream_graphs
                .get(&loaded_stream.stream_id)
                .and_then(Option::as_ref)
                .is_some_and(|followed| followed.loaded_stream == loaded_stream);
            if already_followed_on_this_load {
                continue;
            }
            let stream_id = loaded_stream.stream_id.clone();
            let followed = self.follow_one_load(loaded_stream)?;
            followed_stream_graphs.insert(stream_id, Some(followed));
        }
        Ok(followed_stream_graphs.keys().cloned().collect())
    }

    /// Every followed stream that is loaded now, by stream id.
    pub(crate) fn every_followed_stream_graph_loaded_now(&self) -> Vec<String> {
        self.followed_stream_graphs
            .lock()
            .iter()
            .filter(|(_, followed)| followed.is_some())
            .map(|(stream_id, _)| stream_id.clone())
            .collect()
    }

    /// Move each followed stream's subscription to the load it has now, answering each stream
    /// whose load changed: loaded again, unloaded, or loaded once more after it was unloaded.
    pub(crate) fn follow_each_stream_graph_to_its_current_load(
        &self,
        operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
    ) -> Vec<String> {
        let mut followed_stream_graphs = self.followed_stream_graphs.lock();
        let mut streams_whose_load_changed = Vec::new();
        for (stream_id, followed) in followed_stream_graphs.iter_mut() {
            let loaded_now = operations_on_the_loaded_streams
                .loaded_stream_identity_of_the_stream_a_call_names(stream_id)
                .ok();
            if followed.as_ref().map(|followed| &followed.loaded_stream) == loaded_now.as_ref() {
                continue;
            }
            *followed = match loaded_now.map(|loaded_now| self.follow_one_load(loaded_now)) {
                Some(Ok(followed_on_the_new_load)) => Some(followed_on_the_new_load),
                Some(Err(not_subscribed)) => {
                    tracing::warn!(
                        %not_subscribed,
                        stream_id,
                        "a control connection could not follow a stream's graph to its new load"
                    );
                    None
                }
                None => None,
            };
            streams_whose_load_changed.push(stream_id.clone());
        }
        streams_whose_load_changed
    }

    /// Subscribe the connection to every stream action's changes.
    pub(crate) fn subscribe_to_stream_actions(&self) {
        self.subscribed_to_stream_actions
            .store(true, Ordering::Relaxed);
    }

    /// Whether the connection subscribed to the stream actions' changes.
    pub(crate) fn is_subscribed_to_stream_actions(&self) -> bool {
        self.subscribed_to_stream_actions.load(Ordering::Relaxed)
    }

    fn follow_one_load(
        &self,
        loaded_stream: LoadedStreamIdentity,
    ) -> StreamlibResult<StreamGraphFollowedOnOneLoad> {
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(StreamGraphChangesForwardedToOneConnection {
                stream_id: loaded_stream.stream_id.clone(),
                stream_graph_changes_to_the_connection: self
                    .stream_graph_changes_to_the_connection
                    .clone(),
            }));
        PUBSUB.subscribe(
            &topics::loaded_stream(&loaded_stream),
            Arc::clone(&listener),
        )?;
        Ok(StreamGraphFollowedOnOneLoad {
            loaded_stream,
            _listener_kept_subscribed: listener,
        })
    }
}

/// Forwards each change of one loaded stream's graph heard on its topic to one connection,
/// waiting while the connection's queue is full, so a connection that falls behind loses events
/// at `PUBSUB` and is told `missed`.
struct StreamGraphChangesForwardedToOneConnection {
    stream_id: String,
    stream_graph_changes_to_the_connection: mpsc::Sender<StreamGraphChangedNotificationParams>,
}

impl StreamGraphChangesForwardedToOneConnection {
    fn forward(
        &self,
        change: StreamGraphChangeKindOnTheWire,
        node_state: Option<(String, NodeStateOnTheWire)>,
    ) {
        let (node, node_state) = node_state.unzip();
        let forwarded = self.stream_graph_changes_to_the_connection.blocking_send(
            StreamGraphChangedNotificationParams {
                stream: self.stream_id.clone(),
                change,
                node,
                node_state,
            },
        );
        if forwarded.is_err() {
            tracing::debug!(
                stream_id = self.stream_id,
                "a stream graph change was heard after its control connection ended"
            );
        }
    }
}

impl EventListener for StreamGraphChangesForwardedToOneConnection {
    fn on_event(&mut self, event: &Event) -> StreamlibResult<()> {
        let Event::OfALoadedStream { event, .. } = event else {
            return Ok(());
        };
        match event {
            RuntimeEvent::GraphDidChange => {
                self.forward(StreamGraphChangeKindOnTheWire::Edited, None);
            }
            RuntimeEvent::ProcessorStateDidChange {
                processor_id,
                new_state,
                ..
            } => self.forward(
                StreamGraphChangeKindOnTheWire::NodeStateChanged,
                Some((processor_id.to_string(), node_state_on_the_wire(*new_state))),
            ),
            RuntimeEvent::RuntimeStarted
            | RuntimeEvent::RuntimeStartFailed { .. }
            | RuntimeEvent::RuntimeStopped
            | RuntimeEvent::RuntimeStopFailed { .. }
            | RuntimeEvent::RuntimePaused
            | RuntimeEvent::RuntimePauseFailed { .. }
            | RuntimeEvent::RuntimeResumed
            | RuntimeEvent::RuntimeResumeFailed { .. } => {
                self.forward(StreamGraphChangeKindOnTheWire::PlaybackStateChanged, None);
            }
            _ => {}
        }
        Ok(())
    }

    fn on_events_lost(&mut self, _lost_event_count: u64) -> StreamlibResult<()> {
        self.forward(StreamGraphChangeKindOnTheWire::Missed, None);
        Ok(())
    }
}

fn node_state_on_the_wire(processor_state: ProcessorState) -> NodeStateOnTheWire {
    match processor_state {
        ProcessorState::Pending => NodeStateOnTheWire::Pending,
        ProcessorState::Idle => NodeStateOnTheWire::Idle,
        ProcessorState::Running => NodeStateOnTheWire::Running,
        ProcessorState::Paused => NodeStateOnTheWire::Paused,
        ProcessorState::Stopping => NodeStateOnTheWire::Stopping,
        ProcessorState::Stopped => NodeStateOnTheWire::Stopped,
        ProcessorState::Error => NodeStateOnTheWire::Error,
        ProcessorState::Restarting => NodeStateOnTheWire::Restarting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names a node's state as `graph` renders it.
    ///
    /// Mental-revert: a wire spelling other than the engine's `Display` leaves a client reading
    /// one name from `graph` and another from `stream_graph_changed`.
    #[test]
    fn a_node_state_on_the_wire_is_spelled_as_graph_renders_it() {
        for processor_state in [
            ProcessorState::Pending,
            ProcessorState::Idle,
            ProcessorState::Running,
            ProcessorState::Paused,
            ProcessorState::Stopping,
            ProcessorState::Stopped,
            ProcessorState::Error,
            ProcessorState::Restarting,
        ] {
            assert_eq!(
                serde_json::to_value(node_state_on_the_wire(processor_state))
                    .expect("a node state serializes"),
                serde_json::json!(processor_state.to_string())
            );
        }
    }
}
