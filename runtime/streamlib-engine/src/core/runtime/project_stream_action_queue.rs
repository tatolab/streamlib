// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Each project's serial queue of stream actions: an action runs after the
//! project's earlier ones, one at a time, on a worker the engine's tokio
//! runtime spawns, while other projects' queues run beside it; and the actions
//! the engine holds, every queued and running one and the last finished ones
//! of each project.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use super::{Runner, StreamListingState, StreamLocation};

/// How many finished stream actions of one project the engine holds; an
/// older one is answered as no longer held.
pub const FINISHED_STREAM_ACTIONS_HELD_PER_PROJECT: usize = 64;

/// What a stream action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamActionKind {
    /// Loads every `@stream` of a project.
    Load,
}

impl StreamActionKind {
    /// The kind as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
        }
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
}

impl StreamActionState {
    /// The state as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Whether the action has finished, done or failed.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
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
}

impl StreamActionStep {
    /// The step as the local API spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MakingTheVenv => "making_the_venv",
            Self::Compiling => "compiling",
            Self::Starting => "starting",
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
    /// Where the stream's code lives.
    pub stream_location: StreamLocation,
    /// `None` until the action took the stream in; then running or failed.
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

/// The actions the engine holds by id, and each project's finished ones
/// oldest first, so the oldest past the limit is let go.
#[derive(Default)]
struct StreamActionsHeld {
    by_action_id: HashMap<String, StreamAction>,
    finished_action_ids_of_each_project: HashMap<PathBuf, VecDeque<String>>,
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
        } = &mut *held;
        let Some(action) = by_action_id.get_mut(action_id) else {
            return;
        };
        let it_had_finished = action.state.is_finished();
        change(action);
        if it_had_finished || !action.state.is_finished() {
            return;
        }
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

/// The `<n>` of an action id `<runtime_id>-<n>`, so actions sort in the order
/// they were queued.
fn the_number_of_the_action(action_id: &str) -> u64 {
    action_id
        .rsplit_once('-')
        .and_then(|(_, action_number)| action_number.parse().ok())
        .unwrap_or(u64::MAX)
}

/// How a running stream action reports its step and the streams its compile
/// named, so a listing shows them while it runs.
pub(crate) struct StreamActionProgress<'runner> {
    pub(crate) queue_of_each_project: &'runner StreamActionQueueOfEachProject,
    pub(crate) action_id: String,
}

impl StreamActionProgress<'_> {
    /// The action is on `step`.
    pub(crate) fn is_on(&self, step: StreamActionStep) {
        self.queue_of_each_project
            .change_the_held_stream_action(&self.action_id, |action| {
                action.step = Some(step);
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

    #[test]
    fn action_ids_count_up_from_one_under_the_runtimes_id() {
        let queue_of_each_project = StreamActionQueueOfEachProject::default();
        assert_eq!(queue_of_each_project.next_action_id("Rabc"), "Rabc-1");
        assert_eq!(queue_of_each_project.next_action_id("Rabc"), "Rabc-2");
        assert_eq!(the_number_of_the_action("Rabc-12"), 12);
    }
}
