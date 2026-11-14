// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The node types each project offered when this runtime last listed it, and the follower that
//! lists a project again after a change that can alter them, or after a stream action when its
//! last listing was refused, so the project's feed announces each node type added, removed or
//! updated.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use streamlib::sdk::runtime::StreamAction;
use streamlib_control_contract::ListProjectNodeTypesRequest;
use streamlib_project_repository::{ProjectChangeFeedAnnouncement, ProjectChangeOnTheFeed};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectNodeTypesToolResult, NodeTypeOfOneProjectSource,
};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationDispatcherHeldWeakly,
};

/// The project files whose change can alter the node types a project offers, by file name:
/// node code, and what uv installs the project's distributions from.
const FILE_NAMES_THAT_CAN_CHANGE_THE_NODE_TYPES: [&str; 2] = ["pyproject.toml", "uv.lock"];
const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

/// Each node type a listing offers, by its `type`; a `type` two sources declare is the first's,
/// as the listing orders its sources.
type NodeTypesOfferedByType = BTreeMap<String, NodeTypeOfOneProjectSource>;

/// What one listing changed from the one before it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct NodeTypesChangedSinceTheLastListing {
    pub(crate) added: Vec<NodeTypeOfOneProjectSource>,
    pub(crate) removed: Vec<NodeTypeOfOneProjectSource>,
    pub(crate) updated: Vec<NodeTypeOfOneProjectSource>,
}

/// When a listing of node types started, among every listing this runtime started: a listing that
/// started later read the project's files as they stood later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NodeTypeListingStarted(u64);

/// One project's last listing, and whether the follower is listing it again.
struct OneProjectsNodeTypesLastListed {
    listing_started: NodeTypeListingStarted,
    project_directory: PathBuf,
    node_types_offered: NodeTypesOfferedByType,
    /// Whether the last listing was refused, as while the project's venv is being made, which
    /// leaves `node_types_offered` as the listing before it read them.
    last_listing_was_refused: bool,
    listing_it_again: bool,
    listing_wanted_again: bool,
}

impl OneProjectsNodeTypesLastListed {
    /// A project's first listing that read, as kept, with no listing of it again running.
    fn of_a_first_listing_that_read(
        listing_started: NodeTypeListingStarted,
        project_directory: PathBuf,
        node_types_offered: NodeTypesOfferedByType,
    ) -> Self {
        Self {
            listing_started,
            project_directory,
            node_types_offered,
            last_listing_was_refused: false,
            listing_it_again: false,
            listing_wanted_again: false,
        }
    }

    /// A project's first listing that was refused, as kept: it offers nothing yet.
    fn of_a_first_listing_refused(
        listing_started: NodeTypeListingStarted,
        project_directory: PathBuf,
    ) -> Self {
        Self {
            last_listing_was_refused: true,
            ..Self::of_a_first_listing_that_read(
                listing_started,
                project_directory,
                NodeTypesOfferedByType::new(),
            )
        }
    }
}

/// The node types each project offered when this runtime last listed it, by project UUID.
#[derive(Default)]
pub(crate) struct ProjectNodeTypesLastListed {
    by_project: Mutex<HashMap<Uuid, OneProjectsNodeTypesLastListed>>,
    listings_started: AtomicU64,
    follower_started: std::sync::OnceLock<()>,
}

impl ProjectNodeTypesLastListed {
    /// Mark a listing starting, before it reads the project's files.
    pub(crate) fn start_a_listing(&self) -> NodeTypeListingStarted {
        NodeTypeListingStarted(self.listings_started.fetch_add(1, Ordering::Relaxed))
    }

    /// Keep `listing`, started at `listing_started`, as `project`'s last and hand
    /// `announce_what_changed` what it changed from the one before, under the one lock, so the
    /// changes are announced in the order the listings are kept; a project never listed before has
    /// nothing to change from, and a listing started before the one kept is dropped unannounced.
    pub(crate) fn take_in_the_listing(
        &self,
        project: Uuid,
        listing_started: NodeTypeListingStarted,
        listing: &ListProjectNodeTypesToolResult,
        announce_what_changed: impl FnOnce(NodeTypesChangedSinceTheLastListing),
    ) {
        let node_types_offered = node_types_offered_by_type_in(listing);
        let mut by_project = self.by_project.lock();
        match by_project.get_mut(&project) {
            Some(last_listed) if last_listed.listing_started > listing_started => {}
            Some(last_listed) => {
                let changed = node_types_changed_between(
                    &last_listed.node_types_offered,
                    &node_types_offered,
                );
                last_listed.listing_started = listing_started;
                last_listed.project_directory = listing.project_directory.clone();
                last_listed.node_types_offered = node_types_offered;
                last_listed.last_listing_was_refused = false;
                announce_what_changed(changed);
            }
            None => {
                by_project.insert(
                    project,
                    OneProjectsNodeTypesLastListed::of_a_first_listing_that_read(
                        listing_started,
                        listing.project_directory.clone(),
                        node_types_offered,
                    ),
                );
            }
        }
    }

    /// Keep that the listing of `project` started at `listing_started` was refused, so the
    /// follower lists it again and the listing that reads announces what it offers; a project
    /// never listed before offered nothing, and one listed before keeps what it offered.
    pub(crate) fn take_in_the_refusal_of_a_listing(
        &self,
        project: Uuid,
        listing_started: NodeTypeListingStarted,
        project_directory: &Path,
    ) {
        let mut by_project = self.by_project.lock();
        match by_project.get_mut(&project) {
            Some(last_listed) if last_listed.listing_started > listing_started => {}
            Some(last_listed) => {
                last_listed.listing_started = listing_started;
                last_listed.project_directory = project_directory.to_path_buf();
                last_listed.last_listing_was_refused = true;
            }
            None => {
                by_project.insert(
                    project,
                    OneProjectsNodeTypesLastListed::of_a_first_listing_refused(
                        listing_started,
                        project_directory.to_path_buf(),
                    ),
                );
            }
        }
    }

    /// Start following the change feed and the stream actions of `dispatcher`'s projects once,
    /// listing again each project already listed after a change that can alter its node types,
    /// and each whose last listing was refused after a stream action of it finishes.
    pub(crate) fn follow_the_change_feed_once(&self, dispatcher: &ControlOperationDispatcher) {
        self.follower_started.get_or_init(|| {
            let mut change_feed = dispatcher
                .project_repositories
                .subscribe_to_the_change_feed();
            let mut stream_action_changes = dispatcher
                .operations_on_the_loaded_streams
                .listen_for_each_stream_action_change();
            let dispatcher = dispatcher.held_weakly();
            tokio::spawn(async move {
                let mut stream_action_changes_still_arrive = true;
                loop {
                    let projects_to_list_again = tokio::select! {
                        announced = change_feed.recv() => {
                            projects_to_list_again_after_the_change_feed(&dispatcher, announced)
                        }
                        changed_action_id = stream_action_changes.recv(),
                            if stream_action_changes_still_arrive =>
                        {
                            let projects_to_list_again =
                                projects_to_list_again_after_a_stream_action_change(
                                    &dispatcher,
                                    changed_action_id,
                                );
                            stream_action_changes_still_arrive = projects_to_list_again.is_some();
                            Some(projects_to_list_again.unwrap_or_default())
                        }
                    };
                    let Some(projects_to_list_again) = projects_to_list_again else {
                        return;
                    };
                    for project in projects_to_list_again {
                        list_the_project_again(&dispatcher, project);
                    }
                }
            });
        });
    }

    fn every_project_listed(&self) -> Vec<Uuid> {
        self.by_project.lock().keys().copied().collect()
    }

    fn every_project_last_refused(&self) -> Vec<Uuid> {
        self.by_project
            .lock()
            .iter()
            .filter(|(_, last_listed)| last_listed.last_listing_was_refused)
            .map(|(project, _)| *project)
            .collect()
    }

    /// Each project at `stream_action`'s directory whose last listing was refused, once the
    /// action has finished: the action can have made the venv the listing reads with. The
    /// action and the listing both spell the directory with its symbolic links resolved.
    fn projects_a_stream_action_can_make_listable(
        &self,
        stream_action: &StreamAction,
    ) -> Vec<Uuid> {
        if !stream_action.state.is_finished() {
            return Vec::new();
        }
        self.by_project
            .lock()
            .iter()
            .filter(|(_, last_listed)| {
                last_listed.last_listing_was_refused
                    && last_listed.project_directory == stream_action.project_directory
            })
            .map(|(project, _)| *project)
            .collect()
    }

    /// The project's directory when it was listed and no listing of it again is running, marking
    /// one running; otherwise marks another wanted once the running one ends.
    fn start_listing_again(&self, project: Uuid) -> Option<PathBuf> {
        let mut by_project = self.by_project.lock();
        let last_listed = by_project.get_mut(&project)?;
        if last_listed.listing_it_again {
            last_listed.listing_wanted_again = true;
            return None;
        }
        last_listed.listing_it_again = true;
        Some(last_listed.project_directory.clone())
    }

    /// End the running listing of `project` again, answering whether another is wanted.
    fn end_listing_again(&self, project: Uuid) -> bool {
        let mut by_project = self.by_project.lock();
        let Some(last_listed) = by_project.get_mut(&project) else {
            return false;
        };
        let wanted_again = std::mem::take(&mut last_listed.listing_wanted_again);
        last_listed.listing_it_again = wanted_again;
        wanted_again
    }
}

/// List `project` again off the follower, once at a time, again after a change that landed while
/// it listed; the listing announces what changed as any listing does.
fn list_the_project_again(dispatcher: &ControlOperationDispatcherHeldWeakly, project: Uuid) {
    let Some(project_directory) = dispatcher.upgrade().and_then(|dispatcher| {
        dispatcher
            .project_node_types_last_listed
            .start_listing_again(project)
    }) else {
        return;
    };
    let dispatcher = dispatcher.clone();
    tokio::spawn(async move {
        loop {
            let Some(dispatcher) = dispatcher.upgrade() else {
                return;
            };
            if let Err(refusal) = dispatcher
                .list_project_node_types(ListProjectNodeTypesRequest {
                    project_directory: project_directory.clone(),
                })
                .await
            {
                tracing::warn!(
                    project = %project_directory.display(),
                    %refusal,
                    "the project's node types were not listed again after a change, so no change \
                     of them is announced until its next listing"
                );
            }
            if !dispatcher
                .project_node_types_last_listed
                .end_listing_again(project)
            {
                return;
            }
        }
    });
}

/// Each project to list again after `announced` on the change feed; `None` once the feed has
/// closed or the dispatcher is gone, which ends the follower.
fn projects_to_list_again_after_the_change_feed(
    dispatcher: &ControlOperationDispatcherHeldWeakly,
    announced: Result<ProjectChangeFeedAnnouncement, RecvError>,
) -> Option<Vec<Uuid>> {
    match announced {
        Ok(announcement) if can_change_the_node_types(&announcement) => {
            Some(vec![announcement.project])
        }
        Ok(_) => Some(Vec::new()),
        Err(RecvError::Lagged(_)) => Some(
            dispatcher
                .upgrade()?
                .project_node_types_last_listed
                .every_project_listed(),
        ),
        Err(RecvError::Closed) => None,
    }
}

/// Each project to list again after the stream action `changed_action_id` names changed; `None`
/// once no further change arrives or the dispatcher is gone.
fn projects_to_list_again_after_a_stream_action_change(
    dispatcher: &ControlOperationDispatcherHeldWeakly,
    changed_action_id: Result<String, RecvError>,
) -> Option<Vec<Uuid>> {
    match changed_action_id {
        Ok(changed_action_id) => {
            let dispatcher = dispatcher.upgrade()?;
            Some(
                dispatcher
                    .operations_on_the_loaded_streams
                    .describe_stream_action(&changed_action_id)
                    .map(|stream_action| {
                        dispatcher
                            .project_node_types_last_listed
                            .projects_a_stream_action_can_make_listable(&stream_action)
                    })
                    .unwrap_or_else(|not_described| {
                        tracing::debug!(
                            action = %changed_action_id,
                            %not_described,
                            "a stream action that changed is not described, so no project is \
                             listed again after it"
                        );
                        Vec::new()
                    }),
            )
        }
        Err(RecvError::Lagged(_)) => Some(
            dispatcher
                .upgrade()?
                .project_node_types_last_listed
                .every_project_last_refused(),
        ),
        Err(RecvError::Closed) => None,
    }
}

fn can_change_the_node_types(announcement: &ProjectChangeFeedAnnouncement) -> bool {
    match &announcement.change {
        ProjectChangeOnTheFeed::VenvMadeCurrent | ProjectChangeOnTheFeed::FilesChangedUnnamed => {
            true
        }
        ProjectChangeOnTheFeed::FilesChanged(files) => files
            .iter()
            .any(|file| can_a_file_change_the_node_types(file)),
        ProjectChangeOnTheFeed::Applied(_)
        | ProjectChangeOnTheFeed::Refused
        | ProjectChangeOnTheFeed::MediaFactsRead
        | ProjectChangeOnTheFeed::NodeTypesAdded(_)
        | ProjectChangeOnTheFeed::NodeTypesRemoved(_)
        | ProjectChangeOnTheFeed::NodeTypesUpdated(_) => false,
    }
}

fn can_a_file_change_the_node_types(path_relative_to_the_project: &str) -> bool {
    let file_name = path_relative_to_the_project
        .rsplit('/')
        .next()
        .unwrap_or(path_relative_to_the_project);
    file_name.ends_with(PYTHON_SOURCE_FILE_SUFFIX)
        || FILE_NAMES_THAT_CAN_CHANGE_THE_NODE_TYPES.contains(&file_name)
}

fn node_types_offered_by_type_in(
    listing: &ListProjectNodeTypesToolResult,
) -> NodeTypesOfferedByType {
    let mut node_types_offered = NodeTypesOfferedByType::new();
    for source in &listing.sources {
        for node_type in &source.node_types {
            node_types_offered
                .entry(node_type.node_type.clone())
                .or_insert_with(|| NodeTypeOfOneProjectSource {
                    origin: source.origin.clone(),
                    node_type: node_type.clone(),
                });
        }
    }
    node_types_offered
}

fn node_types_changed_between(
    offered_before: &NodeTypesOfferedByType,
    offered_now: &NodeTypesOfferedByType,
) -> NodeTypesChangedSinceTheLastListing {
    let mut changed = NodeTypesChangedSinceTheLastListing::default();
    for (node_type, offered) in offered_now {
        match offered_before.get(node_type) {
            None => changed.added.push(offered.clone()),
            Some(offered_before) if offered_before != offered => {
                changed.updated.push(offered.clone());
            }
            Some(_) => {}
        }
    }
    for (node_type, offered_before) in offered_before {
        if !offered_now.contains_key(node_type) {
            changed.removed.push(offered_before.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::runtime::{StreamActionKind, StreamActionState};
    use streamlib_runtime_client_contract::local_api_wire_contract::{
        NodeTypeReadFromSource, NodeTypeSourceOrigin, NodeTypeSourceReadWithoutRunningIt,
    };

    fn a_node_type(node_type: &str, description: &str) -> NodeTypeReadFromSource {
        NodeTypeReadFromSource {
            node_type: node_type.to_owned(),
            kind: Some("process".to_owned()),
            description: description.to_owned(),
            category: Some("camera".to_owned()),
            inputs: None,
            outputs: None,
            path: None,
            line: None,
        }
    }

    fn a_listing_of(node_types: Vec<NodeTypeReadFromSource>) -> ListProjectNodeTypesToolResult {
        ListProjectNodeTypesToolResult {
            project_directory: PathBuf::from("/projects/show"),
            project: Uuid::nil().to_string(),
            project_name: "show".to_owned(),
            sources: vec![NodeTypeSourceReadWithoutRunningIt {
                origin: NodeTypeSourceOrigin::InstalledDistribution {
                    distribution: "tatolab-camera".to_owned(),
                    distribution_version: "0.68.90".to_owned(),
                },
                node_types,
                graph_generators: Vec::new(),
                unreadable_modules: Vec::new(),
            }],
        }
    }

    /// What `last_listed` takes in from `listing`, started now.
    fn what_the_listing_changed(
        last_listed: &ProjectNodeTypesLastListed,
        listing: &ListProjectNodeTypesToolResult,
    ) -> NodeTypesChangedSinceTheLastListing {
        let mut announced = NodeTypesChangedSinceTheLastListing::default();
        last_listed.take_in_the_listing(
            Uuid::nil(),
            last_listed.start_a_listing(),
            listing,
            |changed| announced = changed,
        );
        announced
    }

    fn the_types_of(node_types: &[NodeTypeOfOneProjectSource]) -> Vec<&str> {
        node_types
            .iter()
            .map(|offered| offered.node_type.node_type.as_str())
            .collect()
    }

    #[test]
    fn a_projects_first_listing_changes_nothing() {
        let last_listed = ProjectNodeTypesLastListed::default();
        let changed = what_the_listing_changed(
            &last_listed,
            &a_listing_of(vec![a_node_type("tatolab.camera:CameraSource", "a camera")]),
        );
        assert_eq!(changed, NodeTypesChangedSinceTheLastListing::default());
    }

    #[test]
    fn a_later_listing_names_each_type_added_removed_and_updated_and_no_other() {
        let last_listed = ProjectNodeTypesLastListed::default();
        what_the_listing_changed(
            &last_listed,
            &a_listing_of(vec![
                a_node_type("tatolab.camera:CameraSource", "a camera"),
                a_node_type("tatolab.camera:LatencyProbe", "a probe"),
                a_node_type("tatolab.camera:Unchanged", "the same"),
            ]),
        );

        let changed = what_the_listing_changed(
            &last_listed,
            &a_listing_of(vec![
                a_node_type("tatolab.camera:CameraSource", "a camera, now with HDR"),
                a_node_type("tatolab.camera:Unchanged", "the same"),
                a_node_type("tatolab.camera:FrameCounter", "counts frames"),
            ]),
        );

        assert_eq!(
            the_types_of(&changed.added),
            ["tatolab.camera:FrameCounter"]
        );
        assert_eq!(
            the_types_of(&changed.removed),
            ["tatolab.camera:LatencyProbe"]
        );
        assert_eq!(
            the_types_of(&changed.updated),
            ["tatolab.camera:CameraSource"]
        );
        assert_eq!(
            changed.updated[0].node_type.description,
            "a camera, now with HDR"
        );
        assert_eq!(changed.removed[0].node_type.description, "a probe");
    }

    #[test]
    fn a_listing_the_same_as_the_last_changes_nothing() {
        let last_listed = ProjectNodeTypesLastListed::default();
        let listing = a_listing_of(vec![a_node_type("tatolab.camera:CameraSource", "a camera")]);
        what_the_listing_changed(&last_listed, &listing);

        assert_eq!(
            what_the_listing_changed(&last_listed, &listing),
            NodeTypesChangedSinceTheLastListing::default()
        );
    }

    #[test]
    fn node_code_and_what_uv_installs_from_can_change_the_node_types_and_other_files_cannot() {
        assert!(can_a_file_change_the_node_types("nodes/looks.py"));
        assert!(can_a_file_change_the_node_types(
            "imports/camera/pyproject.toml"
        ));
        assert!(can_a_file_change_the_node_types("uv.lock"));
        assert!(!can_a_file_change_the_node_types("media/intro.mp4"));
        assert!(!can_a_file_change_the_node_types("streams/main.json"));
    }

    #[test]
    fn a_listing_again_asked_for_while_one_runs_runs_once_after_it() {
        let last_listed = ProjectNodeTypesLastListed::default();
        what_the_listing_changed(&last_listed, &a_listing_of(Vec::new()));

        assert_eq!(
            last_listed.start_listing_again(Uuid::nil()),
            Some(PathBuf::from("/projects/show"))
        );
        assert_eq!(last_listed.start_listing_again(Uuid::nil()), None);
        assert_eq!(last_listed.start_listing_again(Uuid::nil()), None);
        assert!(last_listed.end_listing_again(Uuid::nil()));
        assert!(!last_listed.end_listing_again(Uuid::nil()));
        assert_eq!(
            last_listed.start_listing_again(Uuid::nil()),
            Some(PathBuf::from("/projects/show"))
        );
    }

    #[test]
    fn a_listing_started_before_the_one_kept_is_dropped_unannounced() {
        let last_listed = ProjectNodeTypesLastListed::default();
        what_the_listing_changed(&last_listed, &a_listing_of(Vec::new()));
        let started_first = last_listed.start_a_listing();
        let started_second = last_listed.start_a_listing();
        let newer = a_listing_of(vec![a_node_type("tatolab.camera:CameraSource", "a camera")]);
        let mut announced = Vec::new();
        last_listed.take_in_the_listing(Uuid::nil(), started_second, &newer, |changed| {
            announced.push(changed)
        });

        last_listed.take_in_the_listing(
            Uuid::nil(),
            started_first,
            &a_listing_of(Vec::new()),
            |changed| announced.push(changed),
        );

        assert_eq!(announced.len(), 1);
        assert_eq!(
            the_types_of(&announced[0].added),
            ["tatolab.camera:CameraSource"]
        );
        assert_eq!(
            what_the_listing_changed(&last_listed, &newer),
            NodeTypesChangedSinceTheLastListing::default()
        );
    }

    fn a_stream_action_of(project_directory: &str, state: StreamActionState) -> StreamAction {
        StreamAction {
            action_id: "runtime-1".to_owned(),
            project_directory: PathBuf::from(project_directory),
            kind: StreamActionKind::Open,
            state,
            step: None,
            venv_sync: None,
            streams: Vec::new(),
            failed_because: None,
        }
    }

    #[test]
    fn a_project_whose_first_listing_was_refused_announces_what_its_next_listing_offers() {
        let last_listed = ProjectNodeTypesLastListed::default();
        let refused_listing_started = last_listed.start_a_listing();
        last_listed.take_in_the_refusal_of_a_listing(
            Uuid::nil(),
            refused_listing_started,
            Path::new("/projects/show"),
        );

        let changed = what_the_listing_changed(
            &last_listed,
            &a_listing_of(vec![a_node_type("tatolab.camera:CameraSource", "a camera")]),
        );

        assert_eq!(
            the_types_of(&changed.added),
            ["tatolab.camera:CameraSource"]
        );
    }

    #[test]
    fn a_refused_listing_keeps_what_the_listing_before_it_offered() {
        let last_listed = ProjectNodeTypesLastListed::default();
        let offered = a_listing_of(vec![a_node_type("tatolab.camera:CameraSource", "a camera")]);
        what_the_listing_changed(&last_listed, &offered);
        let refused_listing_started = last_listed.start_a_listing();
        last_listed.take_in_the_refusal_of_a_listing(
            Uuid::nil(),
            refused_listing_started,
            Path::new("/projects/show"),
        );

        assert_eq!(
            what_the_listing_changed(&last_listed, &offered),
            NodeTypesChangedSinceTheLastListing::default()
        );
    }

    #[test]
    fn a_refusal_of_a_listing_started_before_the_one_kept_is_dropped() {
        let last_listed = ProjectNodeTypesLastListed::default();
        let refused_listing_started = last_listed.start_a_listing();
        what_the_listing_changed(&last_listed, &a_listing_of(Vec::new()));
        last_listed.take_in_the_refusal_of_a_listing(
            Uuid::nil(),
            refused_listing_started,
            Path::new("/projects/show"),
        );

        assert!(
            last_listed
                .projects_a_stream_action_can_make_listable(&a_stream_action_of(
                    "/projects/show",
                    StreamActionState::Done
                ))
                .is_empty()
        );
    }

    #[test]
    fn a_finished_stream_action_lists_again_only_its_project_last_refused() {
        let last_listed = ProjectNodeTypesLastListed::default();
        let refused_listing_started = last_listed.start_a_listing();
        last_listed.take_in_the_refusal_of_a_listing(
            Uuid::nil(),
            refused_listing_started,
            Path::new("/projects/show"),
        );
        let listable = |project_directory: &str, state: StreamActionState| {
            last_listed.projects_a_stream_action_can_make_listable(&a_stream_action_of(
                project_directory,
                state,
            ))
        };

        assert!(listable("/projects/show", StreamActionState::Queued).is_empty());
        assert!(listable("/projects/show", StreamActionState::InProgress).is_empty());
        assert!(listable("/projects/other", StreamActionState::Done).is_empty());
        assert_eq!(
            listable("/projects/show", StreamActionState::Done),
            [Uuid::nil()]
        );
        assert_eq!(
            listable("/projects/show", StreamActionState::Failed),
            [Uuid::nil()]
        );

        what_the_listing_changed(&last_listed, &a_listing_of(Vec::new()));
        assert!(listable("/projects/show", StreamActionState::Done).is_empty());
    }

    #[test]
    fn every_project_last_refused_leaves_out_one_whose_last_listing_read() {
        let last_listed = ProjectNodeTypesLastListed::default();
        assert!(last_listed.every_project_last_refused().is_empty());
        let refused_listing_started = last_listed.start_a_listing();
        last_listed.take_in_the_refusal_of_a_listing(
            Uuid::nil(),
            refused_listing_started,
            Path::new("/projects/show"),
        );
        assert_eq!(last_listed.every_project_last_refused(), [Uuid::nil()]);

        what_the_listing_changed(&last_listed, &a_listing_of(Vec::new()));
        assert!(last_listed.every_project_last_refused().is_empty());
    }

    #[test]
    fn a_project_never_listed_is_not_listed_again() {
        assert_eq!(
            ProjectNodeTypesLastListed::default().start_listing_again(Uuid::nil()),
            None
        );
    }
}
