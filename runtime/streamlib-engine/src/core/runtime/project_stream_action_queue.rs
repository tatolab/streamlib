// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Each project's serial queue of stream actions: an action runs after the
//! project's earlier ones, one at a time, on a worker the engine's tokio
//! runtime spawns, while other projects' queues run beside it; and the actions
//! the engine holds, every queued and running one and the last finished ones
//! of each project.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use super::{Runner, StreamListingState, StreamLocation, is_the_machines_shutdown_requested};
use crate::core::{Error, Result};

/// How many finished stream actions of one project the engine holds; an
/// older one is answered as no longer held.
pub const FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT: usize = 64;

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamActionKind {
    /// Loads every `@stream` of a project.
    Load,
    /// Loads and starts one stopped or failed stream.
    Start,
    /// Unloads one stream and records it stopped.
    Stop,
    /// Unloads every stream of a project and forgets every record of it.
    Remove,
    /// Re-loads, at the runtime's start, a project's streams neither stopped
    /// nor failed.
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

    /// Whether the action unloads the streams it works on, so a listing shows
    /// them stopping rather than preparing.
    pub fn unloads_its_streams(self) -> bool {
        matches!(self, Self::Stop | Self::Remove)
    }
}

/// Where a stream action is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamActionState {
    /// Waiting behind the project's earlier actions.
    Queued,
    /// Running one of its steps.
    InProgress,
    /// Finished; each stream it took in is running or failed.
    Done,
    /// Ended without taking in the project, for `failed_because`.
    Failed,
    /// Ended early by `cancel_stream_action`: dropped from its queue, or stopped
    /// at the step it was on.
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

    /// Whether the action has finished, done, failed or cancelled.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// The step a running stream action is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamActionStep {
    /// Making the project's `.venv` with the runtime's uv.
    MakingTheVenv,
    /// Compiling the project's `@stream` functions in its interpreter.
    Compiling,
    /// Loading and starting the streams the compile gave.
    Starting,
    /// Unloading the streams it works on.
    Stopping,
}

impl StreamActionStep {
    /// The step as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MakingTheVenv => "making_the_venv",
            Self::Compiling => "compiling",
            Self::Starting => "starting",
            Self::Stopping => "stopping",
        }
    }
}

/// One stream a stream action works on: each its compile named, then how
/// each ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOfAStreamAction {
    /// The stream's id.
    pub stream_id: String,
    /// The label the stream is shown under.
    pub display_name: String,
    /// Where the stream's code lives; `None` for a stream a library host
    /// loaded.
    pub stream_location: Option<StreamLocation>,
    /// `None` until the action took the stream in; then the state it left it in.
    pub state: Option<StreamListingState>,
}

/// One stream action of a project, as [`Runner::describe_stream_action`]
/// answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAction {
    /// The action's id, `<runtime_id>-<n>`, never repeated across the
    /// runtime's restarts.
    pub action_id: String,
    /// The project the action works on, symlinks resolved when it exists.
    pub project_directory: PathBuf,
    /// What the action does.
    pub kind: StreamActionKind,
    /// Where it is in its life.
    pub state: StreamActionState,
    /// The step it is on while it runs.
    pub step: Option<StreamActionStep>,
    /// The streams it works on, once its compile named them.
    pub streams: Vec<StreamOfAStreamAction>,
    /// Why it failed; `None` unless it did.
    pub failed_because: Option<String>,
}

/// A stream action queued on its project's queue: its id, which
/// [`Runner::describe_stream_action`] follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamActionQueued {
    /// The action's id.
    pub action_id: String,
    /// The project whose queue holds it.
    pub project_directory: PathBuf,
}

/// The stream action working on a stream a listing shows `preparing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamActionWorkingOnAStream {
    /// The action's id.
    pub action_id: String,
    /// The step it is on; `None` while it waits in its queue.
    pub step: Option<StreamActionStep>,
}

/// What the last every-stream compile of a project gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatTheLastCompileOfTheProjectGave {
    /// Every `@stream` its `stream.py` defines, in definition order.
    Streams(Vec<StreamTheLastCompileOfTheProjectNamed>),
    /// Its `stream.py` did not import, with the traceback.
    EntryDidNotImport(String),
    /// Its `stream.py` imported and defines no `@stream`, with why.
    EntryDefinesNoStream(String),
}

/// One `@stream` the last compile of a project named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTheLastCompileOfTheProjectNamed {
    /// Where the stream's code lives.
    pub stream_location: StreamLocation,
    /// The function's docstring; `None` when it has none.
    pub description: Option<String>,
}

/// A project's streams as [`Runner::list_project_streams`] answers them:
/// what its last compile gave, and whether an action is preparing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStreamsListing {
    /// The project, symlinks resolved.
    pub project_directory: PathBuf,
    /// The action under way on the project, when one is queued or running.
    pub stream_action_under_way: Option<StreamActionWorkingOnAStream>,
    /// What the project's last compile gave; `None` before its first.
    pub what_the_last_compile_gave: Option<WhatTheLastCompileOfTheProjectGave>,
}

/// An action's body, run by its project's worker once the actions ahead of it
/// have finished.
type QueuedStreamActionBody = Box<dyn FnOnce(&Runner) + Send + 'static>;

/// An action's id and the body its project's worker runs for it.
struct QueuedStreamActionAndItsBody {
    action_id: String,
    body: QueuedStreamActionBody,
}

/// One project's actions waiting their turn; a project holds one only while
/// its worker runs.
#[derive(Default)]
struct ProjectStreamActionQueue {
    actions_waiting: VecDeque<QueuedStreamActionAndItsBody>,
}

/// The actions the engine holds by id, each project's finished ones oldest
/// first, so the oldest past the limit is let go, and the running ones whose
/// cancel was asked for.
#[derive(Default)]
struct StreamActionsHeld {
    by_action_id: HashMap<String, StreamAction>,
    finished_action_ids_of_each_project: HashMap<PathBuf, VecDeque<String>>,
    running_action_ids_whose_cancel_is_asked: HashSet<String>,
}

/// What a cancel did to the action it named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamActionCancelOutcome {
    /// The action was waiting in its queue: it is dropped and finished
    /// cancelled, never having run.
    DroppedFromItsQueue(StreamAction),
    /// The action is running: it stops at its current step, finishing
    /// cancelled.
    AskedOfTheRunningAction(StreamAction),
}

/// Every project's stream action queue, the actions the engine holds and
/// each project's last compile.
#[derive(Default)]
pub(crate) struct StreamActionQueueOfEachProject {
    stream_action_queue_of_each_project: Mutex<HashMap<PathBuf, ProjectStreamActionQueue>>,
    stream_actions_held: Mutex<StreamActionsHeld>,
    last_compile_of_each_project: Mutex<HashMap<PathBuf, WhatTheLastCompileOfTheProjectGave>>,
    stream_actions_queued_by_this_runtime: AtomicU64,
    runner_the_workers_run_actions_on: OnceLock<Weak<Runner>>,
}

impl StreamActionQueueOfEachProject {
    /// Hand the queues the runner their workers run each action's body on.
    pub(crate) fn run_every_action_on(&self, runner: Weak<Runner>) {
        let _ = self.runner_the_workers_run_actions_on.set(runner);
    }

    /// The next action id of the runtime `runtime_id`.
    pub(crate) fn next_action_id(&self, runtime_id: &str) -> String {
        let action_number = self
            .stream_actions_queued_by_this_runtime
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        format!("{runtime_id}-{action_number}")
    }

    /// Hold `action` as queued, and run `body` once the actions of its
    /// project queued before it have finished; the project's worker is
    /// spawned on `tokio_handle` when none runs.
    pub(crate) fn queue_the_stream_action(
        &self,
        action: StreamAction,
        body: QueuedStreamActionBody,
        tokio_handle: &tokio::runtime::Handle,
    ) -> StreamActionQueued {
        let queued = StreamActionQueued {
            action_id: action.action_id.clone(),
            project_directory: action.project_directory.clone(),
        };
        self.stream_actions_held
            .lock()
            .by_action_id
            .insert(action.action_id.clone(), action);
        let a_worker_is_to_be_spawned = {
            let mut queues = self.stream_action_queue_of_each_project.lock();
            let no_worker_runs = !queues.contains_key(&queued.project_directory);
            queues
                .entry(queued.project_directory.clone())
                .or_default()
                .actions_waiting
                .push_back(QueuedStreamActionAndItsBody {
                    action_id: queued.action_id.clone(),
                    body,
                });
            no_worker_runs
        };
        if a_worker_is_to_be_spawned {
            self.spawn_the_worker_of(queued.project_directory.clone(), tokio_handle);
        }
        queued
    }

    /// Spawn the worker that runs `project_directory`'s queued actions one at
    /// a time until its queue is empty, then lets the queue go. The runner is
    /// held only while an action's body runs, so the engine is never let go on
    /// the async worker. An action whose body panics is recorded failed.
    fn spawn_the_worker_of(
        &self,
        project_directory: PathBuf,
        tokio_handle: &tokio::runtime::Handle,
    ) {
        let Some(runner_the_workers_run_actions_on) =
            self.runner_the_workers_run_actions_on.get().cloned()
        else {
            let never_run = self
                .stream_action_queue_of_each_project
                .lock()
                .remove(&project_directory)
                .map(|queue| queue.actions_waiting)
                .unwrap_or_default();
            for queued in never_run {
                self.record_failed(
                    &queued.action_id,
                    "it was queued on a runtime that runs no stream action".to_string(),
                );
            }
            return;
        };
        tokio_handle.spawn(async move {
            loop {
                let Some(runner) = runner_the_workers_run_actions_on.upgrade() else {
                    return;
                };
                let next_body = {
                    let queue_of_each_project =
                        &runner.stream_actions.stream_action_queue_of_each_project;
                    let mut queues = queue_of_each_project
                        .stream_action_queue_of_each_project
                        .lock();
                    let next_body = queues
                        .get_mut(&project_directory)
                        .and_then(|queue| queue.actions_waiting.pop_front());
                    if next_body.is_none() {
                        queues.remove(&project_directory);
                    }
                    next_body
                };
                let Some(QueuedStreamActionAndItsBody { action_id, body }) = next_body else {
                    tokio::task::spawn_blocking(move || drop(runner));
                    return;
                };
                let runner_of_the_body = Arc::clone(&runner);
                if tokio::task::spawn_blocking(move || body(&runner_of_the_body))
                    .await
                    .is_err()
                {
                    tracing::error!(
                        "the stream action {action_id} of {} panicked; the project's next action runs",
                        project_directory.display()
                    );
                    runner
                        .stream_actions
                        .stream_action_queue_of_each_project
                        .record_failed(&action_id, "the action panicked".to_string());
                }
                tokio::task::spawn_blocking(move || drop(runner));
            }
        });
    }

    /// Change the held action `action_id` with `change`; an action that
    /// finishes takes its place among its project's finished ones, the oldest
    /// past [`FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT`] let go.
    pub(crate) fn change_the_held_stream_action(
        &self,
        action_id: &str,
        change: impl FnOnce(&mut StreamAction),
    ) {
        let mut held = self.stream_actions_held.lock();
        let StreamActionsHeld {
            by_action_id,
            finished_action_ids_of_each_project,
            running_action_ids_whose_cancel_is_asked,
        } = &mut *held;
        let Some(action) = by_action_id.get_mut(action_id) else {
            return;
        };
        let it_had_finished = action.state.is_finished();
        change(action);
        if it_had_finished || !action.state.is_finished() {
            return;
        }
        running_action_ids_whose_cancel_is_asked.remove(action_id);
        let finished_action_ids = finished_action_ids_of_each_project
            .entry(action.project_directory.clone())
            .or_default();
        finished_action_ids.push_back(action_id.to_string());
        while finished_action_ids.len() > FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT {
            if let Some(let_go) = finished_action_ids.pop_front() {
                by_action_id.remove(&let_go);
            }
        }
    }

    /// Cancel the action `action_id`: one waiting in its queue is dropped and
    /// finished cancelled; a running one is asked to stop at its current step
    /// once `refuse_the_cancel_of_a_running_action` lets it through. Refused
    /// for an action the engine does not hold or that has finished.
    pub(crate) fn cancel_the_stream_action(
        &self,
        action_id: &str,
        refuse_the_cancel_of_a_running_action: impl FnOnce(&StreamAction) -> Result<()>,
    ) -> Result<StreamActionCancelOutcome> {
        let mut queues = self.stream_action_queue_of_each_project.lock();
        let dropped_from_its_queue = queues.values_mut().any(|queue| {
            let waiting_before = queue.actions_waiting.len();
            queue
                .actions_waiting
                .retain(|waiting| waiting.action_id != action_id);
            queue.actions_waiting.len() != waiting_before
        });
        if dropped_from_its_queue {
            drop(queues);
            self.change_the_held_stream_action(action_id, |action| {
                action.state = StreamActionState::Cancelled;
                action.step = None;
            });
            return self
                .held_stream_action(action_id)
                .map(StreamActionCancelOutcome::DroppedFromItsQueue)
                .ok_or_else(|| a_stream_action_not_held(action_id));
        }
        let mut held = self.stream_actions_held.lock();
        drop(queues);
        let action = held
            .by_action_id
            .get_mut(action_id)
            .ok_or_else(|| a_stream_action_not_held(action_id))?;
        if action.state.is_finished() {
            return Err(Error::Configuration(format!(
                "the stream action `{action_id}` is already {}, so there is nothing to cancel",
                action.state.as_str()
            )));
        }
        // Out of its queue, the worker has taken it, whether or not its body has marked it begun.
        action.state = StreamActionState::InProgress;
        let action = action.clone();
        refuse_the_cancel_of_a_running_action(&action)?;
        held.running_action_ids_whose_cancel_is_asked
            .insert(action_id.to_string());
        Ok(StreamActionCancelOutcome::AskedOfTheRunningAction(action))
    }

    /// Whether the cancel of the running action `action_id` was asked for.
    fn is_the_cancel_of_the_running_action_asked(&self, action_id: &str) -> bool {
        self.stream_actions_held
            .lock()
            .running_action_ids_whose_cancel_is_asked
            .contains(action_id)
    }

    /// Record the held action `action_id` failed for `failed_because`.
    fn record_failed(&self, action_id: &str, failed_because: String) {
        self.change_the_held_stream_action(action_id, |action| {
            action.state = StreamActionState::Failed;
            action.step = None;
            action.failed_because = Some(failed_because);
        });
    }

    /// The held action `action_id`, `None` when the engine never held it or
    /// let it go.
    pub(crate) fn held_stream_action(&self, action_id: &str) -> Option<StreamAction> {
        self.stream_actions_held
            .lock()
            .by_action_id
            .get(action_id)
            .cloned()
    }

    /// Every queued or running action, the earliest queued first.
    pub(crate) fn actions_under_way(&self) -> Vec<StreamAction> {
        let mut under_way: Vec<StreamAction> = self
            .stream_actions_held
            .lock()
            .by_action_id
            .values()
            .filter(|action| !action.state.is_finished())
            .cloned()
            .collect();
        under_way.sort_by_key(|action| the_number_of_the_action(&action.action_id));
        under_way
    }

    /// Remember what the every-stream compile of the project in
    /// `project_directory` gave.
    pub(crate) fn remember_the_last_compile_of(
        &self,
        project_directory: &Path,
        what_it_gave: WhatTheLastCompileOfTheProjectGave,
    ) {
        self.last_compile_of_each_project
            .lock()
            .insert(project_directory.to_path_buf(), what_it_gave);
    }

    /// What the last every-stream compile of the project in
    /// `project_directory` gave; `None` before its first.
    pub(crate) fn the_last_compile_of(
        &self,
        project_directory: &Path,
    ) -> Option<WhatTheLastCompileOfTheProjectGave> {
        self.last_compile_of_each_project
            .lock()
            .get(project_directory)
            .cloned()
    }
}

/// The refusal of a call naming the action `action_id`, which the engine never
/// held or has let go.
pub(crate) fn a_stream_action_not_held(action_id: &str) -> Error {
    Error::Configuration(format!(
        "the stream action `{action_id}` is not held: this runtime never queued it, or it is \
         older than the last {FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT} finished actions of its \
         project; its records stay in the project's log"
    ))
}

/// The `<n>` of an action id `<runtime_id>-<n>`, so actions sort in the order
/// they were queued.
fn the_number_of_the_action(action_id: &str) -> u64 {
    action_id
        .rsplit_once('-')
        .and_then(|(_, action_number)| action_number.parse().ok())
        .unwrap_or(u64::MAX)
}

/// How a running stream action reports its step and the streams its compile
/// named, so a listing shows them while it runs, and learns that its cancel was
/// asked for.
pub(crate) struct StreamActionProgress<'runner> {
    pub(crate) queue_of_each_project: &'runner StreamActionQueueOfEachProject,
    pub(crate) action_id: String,
    its_cancel_was_seen: AtomicBool,
}

impl<'runner> StreamActionProgress<'runner> {
    /// The progress of the action `action_id`, held by `queue_of_each_project`.
    pub(crate) fn of_the_stream_action(
        queue_of_each_project: &'runner StreamActionQueueOfEachProject,
        action_id: &str,
    ) -> Self {
        Self {
            queue_of_each_project,
            action_id: action_id.to_string(),
            its_cancel_was_seen: AtomicBool::new(false),
        }
    }

    /// Whether the action's cancel was asked for; once it was, the action
    /// finishes cancelled.
    pub(crate) fn is_cancelled(&self) -> bool {
        let its_cancel_is_asked = self
            .queue_of_each_project
            .is_the_cancel_of_the_running_action_asked(&self.action_id);
        if its_cancel_is_asked {
            self.its_cancel_was_seen.store(true, Ordering::Relaxed);
        }
        its_cancel_is_asked
    }

    /// Whether the step the action is on is to stop now: the machine is
    /// shutting down, or the action's cancel was asked for.
    pub(crate) fn is_interrupted(&self) -> bool {
        is_the_machines_shutdown_requested() || self.is_cancelled()
    }

    /// Whether the action saw its cancel, so it finishes cancelled.
    pub(crate) fn saw_its_cancel(&self) -> bool {
        self.its_cancel_was_seen.load(Ordering::Relaxed)
    }

    /// The action is on `step`.
    pub(crate) fn is_on(&self, step: StreamActionStep) {
        self.queue_of_each_project
            .change_the_held_stream_action(&self.action_id, |action| {
                action.step = Some(step);
            });
    }

    /// The action has left its queue and runs.
    pub(crate) fn has_begun(&self) {
        self.queue_of_each_project
            .change_the_held_stream_action(&self.action_id, |action| {
                action.state = StreamActionState::InProgress;
            });
    }

    /// Finish the action: done, each of its streams named in `streams_once_done`
    /// given the state it ended in; or failed for why; or cancelled, once it
    /// saw its cancel, with the states of the streams it reached.
    pub(crate) fn finish(
        &self,
        streams_once_done: std::result::Result<Vec<(String, StreamListingState)>, String>,
    ) {
        let saw_its_cancel = self.saw_its_cancel();
        self.queue_of_each_project
            .change_the_held_stream_action(&self.action_id, |action| {
                action.step = None;
                match (streams_once_done, saw_its_cancel) {
                    (Ok(streams_reached), true) => {
                        action.state = StreamActionState::Cancelled;
                        give_each_stream_the_state_it_ended_in(action, streams_reached);
                    }
                    (Err(_), true) => action.state = StreamActionState::Cancelled,
                    (Ok(streams_once_done), false) => {
                        action.state = StreamActionState::Done;
                        give_each_stream_the_state_it_ended_in(action, streams_once_done);
                    }
                    (Err(failed_because), false) => {
                        action.state = StreamActionState::Failed;
                        action.failed_because = Some(failed_because);
                    }
                }
            });
    }

    /// The action's compile named `streams`, which it takes in next.
    pub(crate) fn works_on(&self, streams: Vec<StreamOfAStreamAction>) {
        self.queue_of_each_project
            .change_the_held_stream_action(&self.action_id, |action| {
                action.streams = streams;
            });
    }
}

/// Give each stream of `action` named in `streams_once_ended` the state it
/// ended in.
fn give_each_stream_the_state_it_ended_in(
    action: &mut StreamAction,
    streams_once_ended: Vec<(String, StreamListingState)>,
) {
    for (stream_id, state) in streams_once_ended {
        if let Some(stream) = action
            .streams
            .iter_mut()
            .find(|stream| stream.stream_id == stream_id)
        {
            stream.state = Some(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_action_of(project_directory: &str, action_id: &str) -> StreamAction {
        StreamAction {
            action_id: action_id.to_string(),
            project_directory: PathBuf::from(project_directory),
            kind: StreamActionKind::Load,
            state: StreamActionState::Queued,
            step: None,
            streams: Vec::new(),
            failed_because: None,
        }
    }

    #[test]
    fn the_oldest_finished_action_past_the_limit_is_let_go_and_one_under_way_never_is() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        let mut held = queue_of_each_project.stream_actions_held.lock();
        for action_number in 1..=FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT + 2 {
            let action_id = format!("R-{action_number}");
            held.by_action_id
                .insert(action_id.clone(), an_action_of("/srv/desk", &action_id));
        }
        drop(held);

        for action_number in 1..=FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT + 1 {
            queue_of_each_project.change_the_held_stream_action(
                &format!("R-{action_number}"),
                |action| {
                    action.state = StreamActionState::Done;
                },
            );
        }

        assert_eq!(queue_of_each_project.held_stream_action("R-1"), None);
        assert!(queue_of_each_project.held_stream_action("R-2").is_some());
        assert!(
            queue_of_each_project
                .held_stream_action(&format!(
                    "R-{}",
                    FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT + 2
                ))
                .is_some()
        );
        assert_eq!(
            queue_of_each_project
                .actions_under_way()
                .iter()
                .map(|action| action.action_id.as_str())
                .collect::<Vec<_>>(),
            [format!(
                "R-{}",
                FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT + 2
            )]
        );
    }

    /// `queue_of_each_project` holding `action` waiting in its project's queue.
    fn hold_waiting(queue_of_each_project: &StreamActionQueueOfEachProject, action: StreamAction) {
        queue_of_each_project
            .stream_action_queue_of_each_project
            .lock()
            .entry(action.project_directory.clone())
            .or_default()
            .actions_waiting
            .push_back(QueuedStreamActionAndItsBody {
                action_id: action.action_id.clone(),
                body: Box::new(|_runner: &Runner| {}),
            });
        queue_of_each_project
            .stream_actions_held
            .lock()
            .by_action_id
            .insert(action.action_id.clone(), action);
    }

    #[test]
    fn a_waiting_action_cancelled_is_dropped_from_its_queue_and_finished_cancelled() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        hold_waiting(&queue_of_each_project, an_action_of("/srv/desk", "R-1"));
        hold_waiting(&queue_of_each_project, an_action_of("/srv/desk", "R-2"));

        let cancelled = queue_of_each_project
            .cancel_the_stream_action("R-1", |_running| panic!("R-1 never ran"))
            .expect("a waiting action is cancelled");

        let StreamActionCancelOutcome::DroppedFromItsQueue(dropped) = cancelled else {
            panic!("a waiting action is dropped: {cancelled:?}");
        };
        assert_eq!(dropped.state, StreamActionState::Cancelled);
        assert_eq!(
            queue_of_each_project
                .stream_action_queue_of_each_project
                .lock()[Path::new("/srv/desk")]
            .actions_waiting
            .iter()
            .map(|waiting| waiting.action_id.as_str())
            .collect::<Vec<_>>(),
            ["R-2"]
        );
        let refusal = queue_of_each_project
            .cancel_the_stream_action("R-1", |_running| Ok(()))
            .expect_err("a finished action has nothing to cancel")
            .to_string();
        assert!(refusal.contains("already cancelled"), "{refusal}");
        let refusal = queue_of_each_project
            .cancel_the_stream_action("R-9", |_running| Ok(()))
            .expect_err("an action never held is refused")
            .to_string();
        assert!(refusal.contains("is not held"), "{refusal}");
    }

    #[test]
    fn a_running_action_cancelled_finishes_cancelled_with_the_streams_it_reached_once_it_sees_it() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        let mut running = an_action_of("/srv/desk", "R-1");
        running.state = StreamActionState::InProgress;
        running.streams = ["camera", "preview"]
            .into_iter()
            .map(|stream_id| StreamOfAStreamAction {
                stream_id: stream_id.to_string(),
                display_name: stream_id.to_string(),
                stream_location: None,
                state: None,
            })
            .collect();
        queue_of_each_project
            .stream_actions_held
            .lock()
            .by_action_id
            .insert("R-1".to_string(), running);
        let progress = StreamActionProgress::of_the_stream_action(&queue_of_each_project, "R-1");
        assert!(!progress.is_cancelled());

        let cancelled = queue_of_each_project
            .cancel_the_stream_action("R-1", |_running| Ok(()))
            .expect("a running action is asked to cancel");

        assert!(matches!(
            cancelled,
            StreamActionCancelOutcome::AskedOfTheRunningAction(StreamAction {
                state: StreamActionState::InProgress,
                ..
            })
        ));
        assert!(!progress.saw_its_cancel());
        assert!(progress.is_cancelled());
        progress.finish(Ok(vec![(
            "camera".to_string(),
            StreamListingState::Running,
        )]));
        let finished = queue_of_each_project.held_stream_action("R-1").unwrap();
        assert_eq!(finished.state, StreamActionState::Cancelled);
        assert_eq!(
            finished
                .streams
                .iter()
                .map(|stream| stream.state)
                .collect::<Vec<_>>(),
            [Some(StreamListingState::Running), None]
        );
        assert!(
            queue_of_each_project
                .stream_actions_held
                .lock()
                .running_action_ids_whose_cancel_is_asked
                .is_empty()
        );
    }

    #[test]
    fn an_action_its_worker_took_before_marking_it_begun_answers_its_cancel_in_progress() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        queue_of_each_project
            .stream_actions_held
            .lock()
            .by_action_id
            .insert("R-1".to_string(), an_action_of("/srv/desk", "R-1"));

        let cancelled = queue_of_each_project
            .cancel_the_stream_action("R-1", |_running| Ok(()))
            .expect("a taken action is asked to cancel");

        let StreamActionCancelOutcome::AskedOfTheRunningAction(asked) = cancelled else {
            panic!("a taken action is running: {cancelled:?}");
        };
        assert_eq!(asked.state, StreamActionState::InProgress);
        assert_eq!(
            queue_of_each_project
                .held_stream_action("R-1")
                .unwrap()
                .state,
            StreamActionState::InProgress
        );
    }

    #[test]
    fn a_running_action_whose_cancel_is_refused_runs_on_unasked() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        let mut running = an_action_of("/srv/desk", "R-1");
        running.state = StreamActionState::InProgress;
        queue_of_each_project
            .stream_actions_held
            .lock()
            .by_action_id
            .insert("R-1".to_string(), running);

        let refusal = queue_of_each_project
            .cancel_the_stream_action("R-1", |_running| {
                Err(Error::Configuration("it cannot be cut short".to_string()))
            })
            .expect_err("the refusal is the cancel's answer");

        assert!(refusal.to_string().contains("cannot be cut short"));
        assert!(
            !StreamActionProgress::of_the_stream_action(&queue_of_each_project, "R-1")
                .is_cancelled()
        );
    }

    #[test]
    fn action_ids_count_up_from_one_under_the_runtimes_id() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        assert_eq!(queue_of_each_project.next_action_id("Rabc"), "Rabc-1");
        assert_eq!(queue_of_each_project.next_action_id("Rabc"), "Rabc-2");
        assert_eq!(the_number_of_the_action("Rabc-12"), 12);
    }
}
