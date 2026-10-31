// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every load and unload of a project is a stream action on that project's
//! serial queue: queued and answered with its action id at once, then run one
//! at a time, in order, on the queue's own worker thread. Projects' queues
//! never wait on each other.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use parking_lot::{Condvar, Mutex};

use super::{Runner, StreamListingState, is_the_machines_shutdown_requested};
use crate::core::logging::ProjectLogRouteEntry;
use crate::core::{Error, Result};

/// How many finished stream actions of one project the runtime keeps; an
/// older one's id is answered as no longer held, and its records stay in the
/// project's log.
pub const FINISHED_STREAM_ACTIONS_KEPT_PER_PROJECT: usize = 64;

const OPEN_TO_A_CANCEL: u8 = 0;
const CANCEL_REQUESTED: u8 = 1;
const PAST_CANCELLING: u8 = 2;

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamActionKind {
    /// Load every `@stream` of the project's `stream.py`.
    Load,
    /// Start a stopped or failed stream from its record.
    Start,
    /// Stop a stream, recording it stopped.
    Stop,
    /// Remove the project: unload every stream of it and forget its records.
    Remove,
    /// The runtime's re-load at its start of the project's records neither
    /// stopped nor failed.
    ReloadAtStart,
}

impl StreamActionKind {
    /// The kind as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Remove => "remove",
            Self::ReloadAtStart => "reload_at_start",
        }
    }

    /// The state a stream this kind of action works on is listed in.
    fn listing_state_of_a_stream_it_works_on(self) -> StreamListingState {
        match self {
            Self::Load | Self::Start | Self::ReloadAtStart => StreamListingState::Preparing,
            Self::Stop | Self::Remove => StreamListingState::Stopping,
        }
    }
}

impl std::fmt::Display for StreamActionKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where a stream action is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamActionState {
    /// Waiting behind the project's earlier actions.
    Queued,
    /// Running one of its steps.
    InProgress,
    /// Finished.
    Done,
    /// Finished, failed for its reason.
    Failed,
    /// Ended early by a cancel.
    Cancelled,
}

impl StreamActionState {
    /// The state as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether the action has ended.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for StreamActionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The step a running stream action is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamActionStep {
    /// Writing the files a new project starts from.
    WritingTheScaffold,
    /// Making the project's `.venv` with the runtime unit's uv.
    MakingTheVenv,
    /// Compiling the project's `@stream` functions.
    Compiling,
    /// Describing, loading and starting streams.
    Starting,
    /// Tearing streams down.
    Stopping,
}

impl StreamActionStep {
    /// The step as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WritingTheScaffold => "writing_the_scaffold",
            Self::MakingTheVenv => "making_the_venv",
            Self::Compiling => "compiling",
            Self::Starting => "starting",
            Self::Stopping => "stopping",
        }
    }
}

impl std::fmt::Display for StreamActionStep {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stream action queued on its project's queue, answered at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamActionQueued {
    /// The action's id, `<runtime_id>-<n>`.
    pub action_id: String,
    /// The project whose queue holds it.
    pub project_directory: PathBuf,
    /// The stream it was queued for; `None` for an action of the whole
    /// project.
    pub stream_id: Option<String>,
}

/// One stream a stream action touched, as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAStreamActionTouched {
    /// The stream's id.
    pub stream_id: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// The state the stream is listed in now; `None` once the runtime holds
    /// it no longer.
    pub state: Option<StreamListingState>,
}

/// A stream action, as `describe_stream_action` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamActionDescription {
    /// The action's id.
    pub action_id: String,
    /// The project whose queue holds it.
    pub project_directory: PathBuf,
    /// What it does.
    pub kind: StreamActionKind,
    /// Where it is in its life.
    pub state: StreamActionState,
    /// The step it is on; `None` while queued and once finished.
    pub step: Option<StreamActionStep>,
    /// The streams it touched so far.
    pub streams: Vec<StreamAStreamActionTouched>,
    /// Why it failed; `None` unless it did.
    pub failed_because: Option<String>,
}

/// What `cancel_stream_action` did with an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamActionCancelOutcome {
    /// The action's id.
    pub action_id: String,
    /// The action's state once the cancel landed: `cancelled` for a queued
    /// one, `in_progress` for a running one that stops at its current step.
    pub state: StreamActionState,
    /// Why the action was not cancelled — it had finished, or its teardown
    /// had started; `None` when it was.
    pub not_cancelled_because: Option<String>,
}

/// What a queued stream action runs once its turn comes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamActionBody {
    /// [`StreamActionKind::Load`]; a created project's scaffold is written
    /// first, and its new streams shown under the folder's name.
    LoadEveryStreamOfTheProject {
        /// Whether the project is being created.
        writing_the_scaffold_first: bool,
    },
    /// [`StreamActionKind::Start`].
    StartTheStream {
        /// The stream to start.
        stream_id: String,
    },
    /// [`StreamActionKind::Stop`].
    StopTheStream {
        /// The stream to stop.
        stream_id: String,
    },
    /// [`StreamActionKind::Remove`].
    RemoveTheProject,
    /// [`StreamActionKind::ReloadAtStart`].
    ReloadTheRecordsAtStart {
        /// The records to re-load, in file-name order.
        stream_ids: Vec<String>,
    },
}

impl StreamActionBody {
    /// What this body does.
    pub(crate) fn kind(&self) -> StreamActionKind {
        match self {
            Self::LoadEveryStreamOfTheProject { .. } => StreamActionKind::Load,
            Self::StartTheStream { .. } => StreamActionKind::Start,
            Self::StopTheStream { .. } => StreamActionKind::Stop,
            Self::RemoveTheProject => StreamActionKind::Remove,
            Self::ReloadTheRecordsAtStart { .. } => StreamActionKind::ReloadAtStart,
        }
    }

    /// The stream this body names; `None` for one of the whole project.
    fn stream_id(&self) -> Option<&str> {
        match self {
            Self::StartTheStream { stream_id } | Self::StopTheStream { stream_id } => {
                Some(stream_id)
            }
            Self::LoadEveryStreamOfTheProject { .. }
            | Self::RemoveTheProject
            | Self::ReloadTheRecordsAtStart { .. } => None,
        }
    }
}

/// Where a stream action is, as its body reports it.
#[derive(Debug)]
struct ProgressOfOneStreamAction {
    state: StreamActionState,
    step: Option<StreamActionStep>,
    streams_touched: Vec<(String, String)>,
    failed_because: Option<String>,
}

/// One stream action of a project: what it runs, where it is, and the log
/// entry its records go through.
pub(crate) struct StreamActionOfAProject {
    action_id: String,
    project_directory: PathBuf,
    kind: StreamActionKind,
    stream_id: Option<String>,
    queued_at: Instant,
    body: Mutex<Option<StreamActionBody>>,
    progress: Mutex<ProgressOfOneStreamAction>,
    /// One of `OPEN_TO_A_CANCEL`, `CANCEL_REQUESTED` or `PAST_CANCELLING`; a
    /// cancel and a teardown race for it, and the first to land wins.
    cancelling: AtomicU8,
    /// `None` while the directory is no project, so the action writes into
    /// the runtime's own log and creates nothing there.
    log_entry: Option<Arc<ProjectLogRouteEntry>>,
}

impl StreamActionOfAProject {
    /// The action `action_id` of the project in `project_directory`, queued
    /// to run `body`, its records going into the project's log — or into the
    /// runtime's own log when the directory holds no `pyproject.toml` and the
    /// action creates no project there.
    pub(crate) fn queued(
        action_id: String,
        project_directory: PathBuf,
        runtime_id: &str,
        body: StreamActionBody,
    ) -> Arc<Self> {
        let it_writes_a_project_log = project_directory.join("pyproject.toml").is_file()
            || matches!(
                body,
                StreamActionBody::LoadEveryStreamOfTheProject {
                    writing_the_scaffold_first: true
                }
            );
        let log_entry = it_writes_a_project_log.then(|| {
            ProjectLogRouteEntry::for_a_stream_action_of_the_project_in(
                &project_directory,
                runtime_id,
                &action_id,
            )
        });
        Arc::new(Self {
            kind: body.kind(),
            stream_id: body.stream_id().map(str::to_string),
            action_id,
            project_directory,
            queued_at: Instant::now(),
            body: Mutex::new(Some(body)),
            progress: Mutex::new(ProgressOfOneStreamAction {
                state: StreamActionState::Queued,
                step: None,
                streams_touched: Vec::new(),
                failed_because: None,
            }),
            cancelling: AtomicU8::new(OPEN_TO_A_CANCEL),
            log_entry,
        })
    }

    /// The action's id.
    pub(crate) fn action_id(&self) -> &str {
        &self.action_id
    }

    /// The project whose queue holds it.
    pub(crate) fn project_directory(&self) -> &Path {
        &self.project_directory
    }

    /// What it does.
    pub(crate) fn kind(&self) -> StreamActionKind {
        self.kind
    }

    /// Run `work` with this action's records going into its log.
    pub(crate) fn run_in_its_log<R>(&self, work: impl FnOnce() -> R) -> R {
        match &self.log_entry {
            Some(log_entry) => log_entry.run_entered(work),
            None => work(),
        }
    }

    /// Note that the action is on `step` now.
    pub(crate) fn enter_step(&self, step: StreamActionStep) {
        self.progress.lock().step = Some(step);
        tracing::info!("the stream action `{}` is {step}", self.action_id);
    }

    /// Note that the action touched the stream `stream_id`, shown under
    /// `display_name`.
    pub(crate) fn touched_the_stream(&self, stream_id: &str, display_name: &str) {
        let mut progress = self.progress.lock();
        match progress
            .streams_touched
            .iter_mut()
            .find(|(touched_id, _)| touched_id == stream_id)
        {
            Some((_, touched_display_name)) => *touched_display_name = display_name.to_string(),
            None => progress
                .streams_touched
                .push((stream_id.to_string(), display_name.to_string())),
        }
    }

    /// Whether a cancel of the action was asked for.
    pub(crate) fn is_cancel_requested(&self) -> bool {
        self.cancelling.load(Ordering::SeqCst) == CANCEL_REQUESTED
    }

    /// Ask the action to stop at its current step; `false` when its teardown
    /// began first.
    fn request_a_cancel(&self) -> bool {
        match self.cancelling.compare_exchange(
            OPEN_TO_A_CANCEL,
            CANCEL_REQUESTED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => true,
            Err(already) => already == CANCEL_REQUESTED,
        }
    }

    /// Mark the action past cancelling — a stop's or a remove's teardown
    /// begins — unless a cancel was asked for first; `false` then.
    pub(crate) fn go_past_cancelling(&self) -> bool {
        match self.cancelling.compare_exchange(
            OPEN_TO_A_CANCEL,
            PAST_CANCELLING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => true,
            Err(already) => already == PAST_CANCELLING,
        }
    }

    /// Whether the action touches the stream `stream_id` of its project.
    fn works_on_the_stream(&self, stream_id: &str, its_project_holds_it: bool) -> bool {
        match &self.stream_id {
            Some(named) => named == stream_id,
            None => {
                its_project_holds_it
                    || self
                        .progress
                        .lock()
                        .streams_touched
                        .iter()
                        .any(|(touched, _)| touched == stream_id)
            }
        }
    }

    /// The state and step the action is at.
    fn state_and_step(&self) -> (StreamActionState, Option<StreamActionStep>) {
        let progress = self.progress.lock();
        (progress.state, progress.step)
    }

    fn mark(&self, state: StreamActionState, failed_because: Option<String>) {
        let mut progress = self.progress.lock();
        progress.state = state;
        if state.is_finished() {
            progress.step = None;
        }
        progress.failed_because = failed_because;
    }

    /// The action as `describe_stream_action` reads it, each stream it
    /// touched in the state `listed_state_of` lists it in.
    fn description(
        &self,
        listed_state_of: &dyn Fn(&str) -> Option<StreamListingState>,
    ) -> StreamActionDescription {
        let progress = self.progress.lock();
        StreamActionDescription {
            action_id: self.action_id.clone(),
            project_directory: self.project_directory.clone(),
            kind: self.kind,
            state: progress.state,
            step: progress.step,
            streams: progress
                .streams_touched
                .iter()
                .map(|(stream_id, display_name)| StreamAStreamActionTouched {
                    stream_id: stream_id.clone(),
                    display_name: display_name.clone(),
                    state: listed_state_of(stream_id),
                })
                .collect(),
            failed_because: progress.failed_because.clone(),
        }
    }
}

/// One project's queue: the actions waiting, the one running, and the most
/// recent finished ones.
#[derive(Default)]
struct ProjectStreamActionQueue {
    queued: VecDeque<Arc<StreamActionOfAProject>>,
    running: Option<Arc<StreamActionOfAProject>>,
    finished: VecDeque<Arc<StreamActionOfAProject>>,
    a_worker_runs_it: bool,
}

impl ProjectStreamActionQueue {
    fn every_action(&self) -> impl Iterator<Item = &Arc<StreamActionOfAProject>> {
        self.running
            .iter()
            .chain(self.queued.iter())
            .chain(self.finished.iter())
    }

    fn keep_finished(&mut self, action: Arc<StreamActionOfAProject>) {
        self.finished.push_back(action);
        while self.finished.len() > FINISHED_STREAM_ACTIONS_KEPT_PER_PROJECT {
            self.finished.pop_front();
        }
    }
}

/// Every project's queue of a runtime, and the count its action ids are
/// numbered by.
pub(crate) struct StreamActionQueuesOfTheEngine {
    runner: Weak<Runner>,
    queue_of_each_project: Mutex<HashMap<PathBuf, ProjectStreamActionQueue>>,
    actions_queued_so_far: AtomicU64,
    a_project_queue_went_idle: Condvar,
}

impl StreamActionQueuesOfTheEngine {
    /// The queues of the runtime `runner`, none queued yet.
    pub(crate) fn of_the_runner(runner: Weak<Runner>) -> Self {
        Self {
            runner,
            queue_of_each_project: Mutex::default(),
            actions_queued_so_far: AtomicU64::new(0),
            a_project_queue_went_idle: Condvar::new(),
        }
    }
}

thread_local! {
    static STREAM_ACTION_RUNNING_ON_THIS_THREAD: RefCell<Option<Arc<StreamActionOfAProject>>> =
        const { RefCell::new(None) };
}

/// Whether the work on the calling thread should stop: the machine is
/// shutting down, or the stream action running on this thread was cancelled.
pub fn is_this_threads_stream_work_interrupted() -> bool {
    is_the_machines_shutdown_requested()
        || STREAM_ACTION_RUNNING_ON_THIS_THREAD
            .try_with(|running| {
                running
                    .borrow()
                    .as_ref()
                    .is_some_and(|action| action.is_cancel_requested())
            })
            .unwrap_or(false)
}

/// The stream action running on this thread until it drops.
struct StreamActionRunningOnThisThread;

impl StreamActionRunningOnThisThread {
    fn enter(action: &Arc<StreamActionOfAProject>) -> Self {
        let _ = STREAM_ACTION_RUNNING_ON_THIS_THREAD
            .try_with(|running| *running.borrow_mut() = Some(Arc::clone(action)));
        Self
    }
}

impl Drop for StreamActionRunningOnThisThread {
    fn drop(&mut self) {
        let _ =
            STREAM_ACTION_RUNNING_ON_THIS_THREAD.try_with(|running| *running.borrow_mut() = None);
    }
}

impl Runner {
    /// Queue `body` on the queue of the project in `project_directory`,
    /// after `before_it_is_queued` writes what queuing it records — its
    /// pending marks — under the queues' lock, so marks land in queue order.
    /// A worker of the project's queue runs it once every earlier action of
    /// the project has finished.
    pub(crate) fn queue_a_stream_action(
        &self,
        project_directory: PathBuf,
        body: StreamActionBody,
        before_it_is_queued: impl FnOnce(&str) -> Result<()>,
    ) -> Result<StreamActionQueued> {
        let queues = &self.stream_actions.stream_action_queues;
        let mut queue_of_each_project = queues.queue_of_each_project.lock();
        let action_number = queues.actions_queued_so_far.fetch_add(1, Ordering::SeqCst) + 1;
        let action_id = format!("{}-{action_number}", self.runtime_id());
        before_it_is_queued(&action_id)?;
        let action = StreamActionOfAProject::queued(
            action_id,
            project_directory.clone(),
            self.runtime_id().as_str(),
            body,
        );
        action.run_in_its_log(|| {
            tracing::info!(
                "the stream action `{}` is queued: {} of {}",
                action.action_id(),
                action.kind(),
                project_directory.display()
            )
        });
        let queued = StreamActionQueued {
            action_id: action.action_id().to_string(),
            project_directory: project_directory.clone(),
            stream_id: action.stream_id.clone(),
        };
        let project_queue = queue_of_each_project
            .entry(project_directory.clone())
            .or_default();
        project_queue.queued.push_back(action);
        if !project_queue.a_worker_runs_it {
            project_queue.a_worker_runs_it = true;
            let runner = queues.runner.clone();
            let spawned = std::thread::Builder::new()
                .name("streamlib-project-stream-actions".to_string())
                .spawn(move || run_the_projects_stream_actions_in_turn(runner, project_directory));
            if let Err(not_spawned) = spawned {
                project_queue.a_worker_runs_it = false;
                project_queue.queued.pop_back();
                return Err(Error::Runtime(format!(
                    "the stream action `{}` was not queued: its project's worker thread could \
                     not be started: {not_spawned}",
                    queued.action_id
                )));
            }
        }
        Ok(queued)
    }

    /// Wait until no project's queue holds an action queued or running.
    #[cfg(test)]
    pub(crate) fn wait_until_every_queued_stream_action_has_finished(&self) {
        let queues = &self.stream_actions.stream_action_queues;
        let mut queue_of_each_project = queues.queue_of_each_project.lock();
        while queue_of_each_project
            .values()
            .any(|project_queue| project_queue.a_worker_runs_it)
        {
            queues
                .a_project_queue_went_idle
                .wait(&mut queue_of_each_project);
        }
    }

    /// The stream action `action_id`, as it is now. Refused naming why for
    /// an id this runtime never queued, and for one it no longer holds.
    pub fn describe_stream_action(&self, action_id: &str) -> Result<StreamActionDescription> {
        let action = self.the_stream_action_of_the_id(action_id)?;
        let listings = self.list_streams();
        Ok(action.description(&|stream_id| {
            listings
                .iter()
                .find(|listing| listing.stream_id == stream_id)
                .map(|listing| listing.state)
        }))
    }

    /// End the stream action `action_id` early: a queued one is dropped and
    /// its pending marks cleared; a running load or start stops at its
    /// current step, leaving every stream it had not reached as recorded. A
    /// stop or remove whose teardown has started, and a finished action, are
    /// past cancelling, and the outcome says so.
    pub fn cancel_stream_action(&self, action_id: &str) -> Result<StreamActionCancelOutcome> {
        let action = self.the_stream_action_of_the_id(action_id)?;
        let dropped_from_its_queue = {
            let mut queue_of_each_project = self
                .stream_actions
                .stream_action_queues
                .queue_of_each_project
                .lock();
            let project_queue = queue_of_each_project.get_mut(action.project_directory());
            project_queue.and_then(|project_queue| {
                let position = project_queue
                    .queued
                    .iter()
                    .position(|queued| Arc::ptr_eq(queued, &action))?;
                let dropped = project_queue.queued.remove(position)?;
                dropped.mark(StreamActionState::Cancelled, None);
                project_queue.keep_finished(Arc::clone(&dropped));
                Some(dropped)
            })
        };
        if let Some(dropped) = dropped_from_its_queue {
            dropped.run_in_its_log(|| {
                tracing::info!("the stream action `{action_id}` was cancelled before its turn came")
            });
            self.clear_the_pending_marks_of_a_stream_action(&dropped);
            return Ok(StreamActionCancelOutcome {
                action_id: action_id.to_string(),
                state: StreamActionState::Cancelled,
                not_cancelled_because: None,
            });
        }
        let (state, _) = action.state_and_step();
        if state.is_finished() {
            return Ok(StreamActionCancelOutcome {
                action_id: action_id.to_string(),
                state,
                not_cancelled_because: Some(format!("it has already ended {state}")),
            });
        }
        if !action.request_a_cancel() {
            return Ok(StreamActionCancelOutcome {
                action_id: action_id.to_string(),
                state,
                not_cancelled_because: Some(format!(
                    "its {} has begun tearing streams down, which runs to its end",
                    action.kind()
                )),
            });
        }
        action.run_in_its_log(|| {
            tracing::info!(
                "the stream action `{action_id}` is cancelled, and stops at its current step"
            )
        });
        Ok(StreamActionCancelOutcome {
            action_id: action_id.to_string(),
            state,
            not_cancelled_because: None,
        })
    }

    /// The action running on the queue of `project_directory` that works on
    /// the stream `stream_id`, with the state the stream is listed in for it
    /// and the step it is on.
    pub(crate) fn the_stream_action_working_on_the_stream(
        &self,
        project_directory: Option<&Path>,
        stream_id: &str,
    ) -> Option<(String, StreamListingState, Option<StreamActionStep>)> {
        let queue_of_each_project = self
            .stream_actions
            .stream_action_queues
            .queue_of_each_project
            .lock();
        queue_of_each_project
            .iter()
            .filter_map(|(queued_project, project_queue)| {
                let running = project_queue.running.as_ref()?;
                let its_project_holds_it = project_directory == Some(queued_project.as_path());
                running
                    .works_on_the_stream(stream_id, its_project_holds_it)
                    .then(|| {
                        (
                            running.action_id().to_string(),
                            running.kind().listing_state_of_a_stream_it_works_on(),
                            running.state_and_step().1,
                        )
                    })
            })
            .next()
    }

    /// Each project with a load queued or running, and the load's id and
    /// step.
    pub(crate) fn projects_with_a_load_queued_or_running(
        &self,
    ) -> Vec<(PathBuf, String, Option<StreamActionStep>)> {
        let queue_of_each_project = self
            .stream_actions
            .stream_action_queues
            .queue_of_each_project
            .lock();
        queue_of_each_project
            .iter()
            .filter_map(|(project_directory, project_queue)| {
                project_queue
                    .running
                    .iter()
                    .chain(project_queue.queued.iter())
                    .find(|action| {
                        matches!(
                            action.kind(),
                            StreamActionKind::Load | StreamActionKind::ReloadAtStart
                        )
                    })
                    .map(|load| {
                        (
                            project_directory.clone(),
                            load.action_id().to_string(),
                            load.state_and_step().1,
                        )
                    })
            })
            .collect()
    }

    fn the_stream_action_of_the_id(&self, action_id: &str) -> Result<Arc<StreamActionOfAProject>> {
        let queue_of_each_project = self
            .stream_actions
            .stream_action_queues
            .queue_of_each_project
            .lock();
        if let Some(action) = queue_of_each_project
            .values()
            .flat_map(ProjectStreamActionQueue::every_action)
            .find(|action| action.action_id() == action_id)
        {
            return Ok(Arc::clone(action));
        }
        let runtime_id = self.runtime_id().to_string();
        let numbered_by_this_runtime = action_id
            .strip_prefix(&format!("{runtime_id}-"))
            .and_then(|number| number.parse::<u64>().ok())
            .is_some_and(|number| {
                number
                    <= self
                        .stream_actions
                        .stream_action_queues
                        .actions_queued_so_far
                        .load(Ordering::SeqCst)
            });
        Err(Error::NotFound(if numbered_by_this_runtime {
            format!(
                "the stream action `{action_id}` is no longer held: the runtime keeps the last \
                 {FINISHED_STREAM_ACTIONS_KEPT_PER_PROJECT} finished actions of each project, and \
                 its records stay in the project's log (`tatolab logs --action {action_id}`)"
            )
        } else {
            format!(
                "this runtime (`{runtime_id}`) queued no stream action `{action_id}`; an action \
                 queued before the runtime last started is no longer held, and its records stay \
                 in its project's log"
            )
        }))
    }

    /// Run `action` on the calling thread, its records going into its
    /// project's log, and mark how it ended.
    fn run_one_stream_action(&self, action: &Arc<StreamActionOfAProject>) {
        let _running_on_this_thread = StreamActionRunningOnThisThread::enter(action);
        let _entered = action
            .log_entry
            .as_ref()
            .map(|log_entry| log_entry.enter_on_this_thread());
        let Some(body) = action.body.lock().take() else {
            return;
        };
        action.mark(StreamActionState::InProgress, None);
        tracing::info!(
            "the stream action `{}` began, {:.1}s after it was queued: {} of {}",
            action.action_id(),
            action.queued_at.elapsed().as_secs_f64(),
            action.kind(),
            action.project_directory().display()
        );
        let ran = self.run_the_body_of_a_stream_action(action, body);
        let (state, failed_because) = match ran {
            Ok(()) => (StreamActionState::Done, None),
            Err(_) if action.is_cancel_requested() => (StreamActionState::Cancelled, None),
            Err(failed_because) => (StreamActionState::Failed, Some(failed_because.to_string())),
        };
        match &failed_because {
            Some(failed_because) => tracing::error!(
                "the stream action `{}` failed: {failed_because}",
                action.action_id()
            ),
            None => tracing::info!("the stream action `{}` is {state}", action.action_id()),
        }
        if state == StreamActionState::Cancelled {
            self.clear_the_pending_marks_of_a_stream_action(action);
        }
        action.mark(state, failed_because);
        if let Some(log_entry) = &action.log_entry {
            log_entry.write_every_record_it_emitted_and_sync();
        }
    }
}

/// The worker of the queue of `project_directory`: run its actions one at a
/// time, in order, until none is queued, holding the runtime only while one
/// runs, so the last hold of it never drops on a worker of its tokio runtime.
fn run_the_projects_stream_actions_in_turn(runner: Weak<Runner>, project_directory: PathBuf) {
    loop {
        let Some(runner) = runner.upgrade() else {
            return;
        };
        let next = {
            let mut queue_of_each_project = runner
                .stream_actions
                .stream_action_queues
                .queue_of_each_project
                .lock();
            let Some(project_queue) = queue_of_each_project.get_mut(&project_directory) else {
                return;
            };
            match project_queue.queued.pop_front() {
                Some(next) => {
                    project_queue.running = Some(Arc::clone(&next));
                    next
                }
                None => {
                    project_queue.a_worker_runs_it = false;
                    runner
                        .stream_actions
                        .stream_action_queues
                        .a_project_queue_went_idle
                        .notify_all();
                    return;
                }
            }
        };
        runner.run_one_stream_action(&next);
        {
            let mut queue_of_each_project = runner
                .stream_actions
                .stream_action_queues
                .queue_of_each_project
                .lock();
            if let Some(project_queue) = queue_of_each_project.get_mut(&project_directory) {
                project_queue.running = None;
                project_queue.keep_finished(next);
            }
        }
        drop(runner);
    }
}
