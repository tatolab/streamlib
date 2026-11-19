// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Graph edits held for one commit: the edits of one writer on one stream — a transaction's
//! batch, an added node with its links, a reconcile, a load — logged into the compiler's
//! transaction while it is held, judged by the graph they leave, compiled in one commit, and
//! announced once they stand; or taken back whole.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use streamlib_project_repository::StreamGraphOperation;

use tokio::sync::OwnedMutexGuard;

use super::{BoxFuture, LiveGraphChangeOnOneStream, commit_the_graph_edits_held_for_one_commit};
use crate::core::compiler::WhatTheHeldCommitFound;
use crate::core::graph::graph_snapshot::GraphOnlyNodesAsDrawn;
use crate::core::graph::{LinkUniqueId, NodeRunId};
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::operations::{
    GraphEditBatch, GraphEditBatchRefused, HowFarARefusedBatchWasTakenBack,
};
use crate::core::{Error, Result};

/// How long a lifecycle change waiting for the stream's one writer waits between looks at it,
/// and at the stream's shutdown escalation.
const WAIT_BETWEEN_LOOKS_AT_THE_ONE_WRITER: std::time::Duration =
    std::time::Duration::from_millis(5);

/// A change of a stream's lifecycle that waits for the batch of graph edits open on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::runtime) enum ALifecycleChange {
    Start,
    Stop,
    Resume,
}

impl std::fmt::Display for ALifecycleChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Start => "started",
            Self::Stop => "stopped",
            Self::Resume => "resumed",
        })
    }
}

tokio::task_local! {
    static THE_GRAPH_EDITS_THIS_TASK_HOLDS: Arc<GraphEditsHeldForOneCommit>;
}

/// Whether `event` announces a connect: such an event is announced only for a link that stands
/// once the edits close, and a disconnect only for a link they did not connect.
fn announces_a_connect(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::RuntimeWillConnect { .. } | RuntimeEvent::RuntimeDidConnect { .. }
    )
}

/// What graph edits held for one commit keep until they close: each event they would have
/// announced, beside the link it announces, the stream as drawn before them, the names the
/// graph-only nodes held then, and the name each node they renamed held before.
pub(in crate::core::runtime) struct GraphEditsHeldForOneCommit {
    events_held: Mutex<Vec<(Option<LinkUniqueId>, RuntimeEvent)>>,
    /// Whether an edit held announced the graph changed, announced once as the edits close.
    the_graph_changed: AtomicBool,
    drawn_before: GraphOnlyNodesAsDrawn,
    node_names_the_graph_only_nodes_held_before: HashSet<String>,
    names_before_each_rename: Mutex<Vec<(NodeRunId, String)>>,
}

impl GraphEditsHeldForOneCommit {
    /// The graph edits the running task holds for one commit, if it holds some.
    pub(in crate::core::runtime) fn the_ones_this_task_holds() -> Option<Arc<Self>> {
        THE_GRAPH_EDITS_THIS_TASK_HOLDS.try_with(Arc::clone).ok()
    }

    /// Keep `event`, which announces `of_the_link` when it names one, until the edits close.
    pub(in crate::core::runtime) fn hold_the_event(
        &self,
        of_the_link: Option<&LinkUniqueId>,
        event: RuntimeEvent,
    ) {
        if matches!(event, RuntimeEvent::GraphDidChange) {
            self.the_graph_changed.store(true, Ordering::SeqCst);
            return;
        }
        self.events_held.lock().push((of_the_link.cloned(), event));
    }

    /// Keep that the node `node_run_id` was named `name_before` before an edit held renamed
    /// it, so a take-back names it so again.
    pub(in crate::core::runtime) fn hold_the_name_before_a_rename(
        &self,
        node_run_id: &NodeRunId,
        name_before: String,
    ) {
        self.names_before_each_rename
            .lock()
            .push((node_run_id.clone(), name_before));
    }
}

impl LoadedStreamInThisRuntime {
    /// Announce this stream's graph changed, or hold that with the graph edits the running
    /// task holds for one commit, which announce it once as they close.
    pub(in crate::core::runtime) fn announce_the_graph_changed(&self) {
        match GraphEditsHeldForOneCommit::the_ones_this_task_holds() {
            Some(held) => held.hold_the_event(None, RuntimeEvent::GraphDidChange),
            None => self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange),
        }
    }

    /// Run `edit` as graph edits held for one commit, so the order its edits are made in never
    /// matters: joined to those the task already holds, or else, once no other writer's are
    /// open on this stream, held, closed when `edit` stands and taken back when it is refused.
    pub(in crate::core::runtime) async fn in_one_commit<T>(
        &self,
        edit: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        if GraphEditsHeldForOneCommit::the_ones_this_task_holds().is_some() {
            return edit.await;
        }
        let _no_other_writer = Arc::clone(&self.one_writer_of_the_graph_at_a_time)
            .lock_owned()
            .await;
        let held = self.hold_the_graph_edits_for_one_commit();
        match THE_GRAPH_EDITS_THIS_TASK_HOLDS
            .scope(Arc::clone(&held), edit)
            .await
        {
            Ok(made) => {
                self.close_the_graph_edits_held_for_one_commit(&held)
                    .await
                    .map_err(|refused| refused.refusal)?;
                Ok(made)
            }
            Err(refusal) => {
                self.take_back_the_graph_edits_held_for_one_commit(&held);
                Err(refusal)
            }
        }
    }

    /// Run `edit` on the engine's runtime from synchronous code, joined to `held`.
    fn block_on_holding<T: Send + 'static>(
        &self,
        held: &Arc<GraphEditsHeldForOneCommit>,
        edit: impl Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        self.block_on_the_engines_tokio_runtime(
            THE_GRAPH_EDITS_THIS_TASK_HOLDS.scope(Arc::clone(held), edit),
        )
    }

    /// Hold this stream's compiler transaction, so the edits logged from now are compiled in
    /// one commit, and answer what the edits keep until they close.
    fn hold_the_graph_edits_for_one_commit(&self) -> Arc<GraphEditsHeldForOneCommit> {
        self.compiler.hold_the_transaction_for_one_commit();
        Arc::new(GraphEditsHeldForOneCommit {
            events_held: Mutex::new(Vec::new()),
            the_graph_changed: AtomicBool::new(false),
            drawn_before: self.graph_only_nodes_as_drawn.lock().clone(),
            node_names_the_graph_only_nodes_held_before: self
                .compiler
                .scope_reading(|graph| graph.node_names_the_graph_only_nodes_hold().clone()),
            names_before_each_rename: Mutex::new(Vec::new()),
        })
    }

    /// Run `edit`, handed this stream's graph change, as graph edits held for one commit from
    /// synchronous code, as `in_one_commit` runs an edit from asynchronous code.
    pub(in crate::core::runtime) fn in_one_commit_from_synchronous_code<T, Edit>(
        &self,
        edit: impl FnOnce(LiveGraphChangeOnOneStream) -> Edit + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
        Edit: Future<Output = Result<T>> + Send + 'static,
    {
        if let Some(held) = GraphEditsHeldForOneCommit::the_ones_this_task_holds() {
            return self.block_on_holding(&held, edit(self.live_graph_change()));
        }
        let stream = self.this_stream_held_for_a_task()?;
        self.block_on_the_engines_tokio_runtime(async move {
            stream
                .in_one_commit(async { edit(stream.live_graph_change()).await })
                .await
        })
    }

    /// Run `edit` on this thread as graph edits held for one commit, held, closed when `edit`
    /// stands and taken back when it is refused, with no other writer to wait for: the caller
    /// holds the stream's one writer, or no other writer reaches the stream yet.
    pub(in crate::core::runtime) fn in_one_commit_on_this_thread<T>(
        &self,
        edit: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let held = self.hold_the_graph_edits_for_one_commit();
        match THE_GRAPH_EDITS_THIS_TASK_HOLDS.sync_scope(Arc::clone(&held), edit) {
            Ok(made) => {
                self.close_the_graph_edits_held_for_one_commit_from_synchronous_code(&held)
                    .map_err(|refused| refused.refusal)?;
                Ok(made)
            }
            Err(refusal) => {
                self.take_back_the_graph_edits_held_for_one_commit(&held);
                Err(refusal)
            }
        }
    }

    /// Close `held`: refuse the graph it leaves when a reactive reader is left fed only by
    /// islands, compile every edit in one commit, then announce each event of what stands. A
    /// refusal takes the edits back as far as nothing built or wired holds them, and says
    /// whether that is all of them.
    async fn close_the_graph_edits_held_for_one_commit(
        &self,
        held: &GraphEditsHeldForOneCommit,
    ) -> std::result::Result<(), GraphEditBatchRefused> {
        if !self.compiler.the_transaction_is_held_for_one_commit() {
            return Err(self.the_refusal_of_edits_a_forced_stop_took_back(held));
        }
        if let Err(refusal) = self.refuse_a_reactive_reader_only_islands_feed() {
            return Err(self.take_back_whole(held, refusal));
        }
        let Some(logged) = self
            .compiler
            .the_operations_logged_under_the_hold_for_one_commit()
        else {
            return Err(self.the_refusal_of_edits_a_forced_stop_took_back(held));
        };
        let found = if logged.is_empty() {
            if self.compiler.release_the_transaction_held_for_one_commit() {
                WhatTheHeldCommitFound::TheHold
            } else {
                WhatTheHeldCommitFound::TheHoldTakenBack
            }
        } else {
            match commit_the_graph_edits_held_for_one_commit(
                &self.compiler,
                self.live_commit_context(),
            )
            .await
            {
                Ok(found) => found,
                Err(failed) => {
                    self.compiler
                        .take_back_what_operations_did_to_the_graph(&logged);
                    self.restore_what_was_drawn_and_drop_the_events(held);
                    return Err(GraphEditBatchRefused {
                        refusal: failed.refusal,
                        taken_back: if failed.after_changing_what_runs {
                            HowFarARefusedBatchWasTakenBack::AsFarAsNothingBuiltOrWiredHoldsIt
                        } else {
                            HowFarARefusedBatchWasTakenBack::Whole
                        },
                    });
                }
            }
        };
        // A stop that took the edits back as the stream's shutdown was forced, between the
        // look at the hold above and the commit, leaves nothing made to answer for.
        if found == WhatTheHeldCommitFound::TheHoldTakenBack {
            return Err(self.the_refusal_of_edits_a_forced_stop_took_back(held));
        }
        self.announce_each_event_of_what_stands(held);
        Ok(())
    }

    /// Close `held` from synchronous code, as `close_the_graph_edits_held_for_one_commit` does.
    fn close_the_graph_edits_held_for_one_commit_from_synchronous_code(
        &self,
        held: &Arc<GraphEditsHeldForOneCommit>,
    ) -> std::result::Result<(), GraphEditBatchRefused> {
        let stream = self
            .this_stream_held_for_a_task()
            .map_err(|refusal| self.take_back_whole(held, refusal))?;
        let held_by_the_close = Arc::clone(held);
        self.block_on_the_engines_tokio_runtime(async move {
            Ok(stream
                .close_the_graph_edits_held_for_one_commit(&held_by_the_close)
                .await)
        })
        .map_err(|refusal| self.take_back_whole(held, refusal))?
    }

    /// Take `held` back whole, refused for `refusal`.
    fn take_back_whole(
        &self,
        held: &GraphEditsHeldForOneCommit,
        refusal: Error,
    ) -> GraphEditBatchRefused {
        self.take_back_the_graph_edits_held_for_one_commit(held);
        GraphEditBatchRefused {
            refusal,
            taken_back: HowFarARefusedBatchWasTakenBack::Whole,
        }
    }

    /// The refusal of closing `held`, whose edits a stop of this stream took back as its
    /// shutdown was forced: its events are dropped, and the stopped stream is left as it is.
    fn the_refusal_of_edits_a_forced_stop_took_back(
        &self,
        held: &GraphEditsHeldForOneCommit,
    ) -> GraphEditBatchRefused {
        held.events_held.lock().clear();
        GraphEditBatchRefused {
            refusal: Error::Runtime(format!(
                "the stream `{}` stopped while these graph edits were open, as its shutdown was \
                 forced, and took them back: none of them was made",
                self.stream_id()
            )),
            taken_back: HowFarARefusedBatchWasTakenBack::Whole,
        }
    }

    /// Announce each event `held` kept of what stands once its edits compiled.
    fn announce_each_event_of_what_stands(&self, held: &GraphEditsHeldForOneCommit) {
        let events_held = std::mem::take(&mut *held.events_held.lock());
        let links_connected_within: HashSet<LinkUniqueId> = events_held
            .iter()
            .filter(|(_, event)| announces_a_connect(event))
            .filter_map(|(of_the_link, _)| of_the_link.clone())
            .collect();
        let the_link_stands = |link_id: &LinkUniqueId| {
            self.compiler.scope_reading(|graph| {
                graph
                    .traversal()
                    .e(link_id)
                    .first()
                    .is_some_and(|link| graph.stands(link))
            })
        };
        for (of_the_link, event) in events_held {
            let announced = match &of_the_link {
                None => true,
                Some(link_id) if announces_a_connect(&event) => the_link_stands(link_id),
                Some(link_id) => !links_connected_within.contains(link_id),
            };
            if announced {
                self.publish_on_this_streams_topic(event);
            }
        }
        if held.the_graph_changed.load(Ordering::SeqCst) {
            self.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);
        }
    }

    /// Take `held` back: each edit logged under the hold, as far as nothing built or wired
    /// holds it, and the stream as drawn before it; announce none of its events. Once a stop
    /// took the hold back as the stream's shutdown was forced, only its events are dropped.
    fn take_back_the_graph_edits_held_for_one_commit(&self, held: &GraphEditsHeldForOneCommit) {
        if self
            .compiler
            .take_back_the_transaction_held_for_one_commit()
        {
            self.restore_what_was_drawn_and_drop_the_events(held);
        } else {
            held.events_held.lock().clear();
        }
    }

    /// Put the stream as drawn back as it was before `held`, the names the graph-only nodes held
    /// and each node `held` renamed under the name it held before, and drop each event it kept.
    fn restore_what_was_drawn_and_drop_the_events(&self, held: &GraphEditsHeldForOneCommit) {
        *self.graph_only_nodes_as_drawn.lock() = held.drawn_before.clone();
        let names_before_each_rename = std::mem::take(&mut *held.names_before_each_rename.lock());
        self.compiler.scope(|graph, _tx| {
            graph.set_node_names_the_graph_only_nodes_hold(
                held.node_names_the_graph_only_nodes_held_before.clone(),
            );
            for (node_run_id, name_before) in names_before_each_rename.into_iter().rev() {
                if let Err(not_renamed) = graph.traversal_mut().rename_v(&node_run_id, &name_before)
                {
                    tracing::warn!(
                        "`{node_run_id}`, renamed by graph edits taken back, keeps its new name: \
                         {not_renamed}"
                    );
                }
            }
        });
        held.events_held.lock().clear();
    }

    /// Hold this stream's one writer of the graph from synchronous code: at once when no
    /// writer holds it, or else once the writer that does lets go, waited for on the engine's
    /// runtime.
    ///
    /// # Panics
    ///
    /// When it waits from inside an asynchronous task, as every other synchronous entry point
    /// that waits on the engine's runtime does.
    fn hold_the_one_writer_from_synchronous_code(&self) -> Result<OwnedMutexGuard<()>> {
        let one_writer = Arc::clone(&self.one_writer_of_the_graph_at_a_time);
        if let Ok(held) = Arc::clone(&one_writer).try_lock_owned() {
            return Ok(held);
        }
        self.block_on_the_engines_tokio_runtime(async move { Ok(one_writer.lock_owned().await) })
    }

    /// Hold this stream's one writer of the graph for `change`, so it waits for a batch of
    /// graph edits open on another thread to close or be taken back; refused on the thread
    /// that holds the batch, or inside graph edits held for one commit, which would wait for
    /// themselves. A stop notes it waits on an open graph edit, and stops waiting once the
    /// stream's shutdown is forced: it takes the open edits back, so its teardown compiles,
    /// and answers no writer held.
    pub(in crate::core::runtime) fn hold_the_one_writer_for_a_lifecycle_change(
        &self,
        change: ALifecycleChange,
    ) -> Result<Option<OwnedMutexGuard<()>>> {
        let this_thread_holds_a_batch =
            *self.thread_holding_a_graph_edit_batch.lock() == Some(std::thread::current().id());
        if this_thread_holds_a_batch
            || GraphEditsHeldForOneCommit::the_ones_this_task_holds().is_some()
        {
            return Err(Error::Runtime(format!(
                "the stream `{}` was not {change}: its caller holds graph edits of it open, and \
                 the change waits for them to close. To fix: close the batch of graph edits \
                 first",
                self.stream_id()
            )));
        }
        let one_writer = &self.one_writer_of_the_graph_at_a_time;
        if let Ok(held) = Arc::clone(one_writer).try_lock_owned() {
            return Ok(Some(held));
        }
        if change == ALifecycleChange::Stop {
            self.teardown_progress_note()
                .note_what_the_teardown_is_waiting_on("an open graph edit");
        }
        loop {
            if change == ALifecycleChange::Stop
                && self.this_streams_shutdown_escalation().is_forced()
            {
                tracing::warn!(
                    "the stream `{}` stops with a graph edit still open, taking it back: its \
                     shutdown was forced",
                    self.stream_id()
                );
                // So the stop's teardown compiles past the hold the open edits keep, and their
                // later close or drop finds it gone and leaves the stopped stream as it is.
                self.compiler
                    .take_back_the_transaction_held_for_one_commit();
                return Ok(None);
            }
            match Arc::clone(one_writer).try_lock_owned() {
                Ok(held) => return Ok(Some(held)),
                Err(_) => std::thread::sleep(WAIT_BETWEEN_LOOKS_AT_THE_ONE_WRITER),
            }
        }
    }

    /// Open a batch of graph edits on this stream for one writer, once no other writer's are
    /// open on it: held for one commit until it closes, and taken back when it is dropped open.
    ///
    /// # Panics
    ///
    /// When it waits for another writer from inside an asynchronous task.
    pub(in crate::core::runtime) fn open_a_graph_edit_batch_of_this_stream(
        self: Arc<Self>,
    ) -> Result<GraphEditBatchOfALoadedStream> {
        let this_thread_holds_a_batch =
            *self.thread_holding_a_graph_edit_batch.lock() == Some(std::thread::current().id());
        if this_thread_holds_a_batch
            || GraphEditsHeldForOneCommit::the_ones_this_task_holds().is_some()
        {
            return Err(Error::Runtime(format!(
                "no batch of graph edits was opened on the stream `{}`: its caller holds graph \
                 edits of it open, and a second batch waits for them to close. To fix: close \
                 the batch of graph edits first",
                self.stream_id()
            )));
        }
        let no_other_writer = self.hold_the_one_writer_from_synchronous_code()?;
        *self.thread_holding_a_graph_edit_batch.lock() = Some(std::thread::current().id());
        let held = self.hold_the_graph_edits_for_one_commit();
        Ok(GraphEditBatchOfALoadedStream {
            stream: self,
            held,
            _no_other_writer: no_other_writer,
            open: true,
        })
    }
}

/// One writer's batch of graph edits on one loaded stream, holding the stream's other writers
/// off until it closes or is dropped.
#[must_use = "a batch of graph edits is closed, or taken back when it is dropped"]
pub(crate) struct GraphEditBatchOfALoadedStream {
    stream: Arc<LoadedStreamInThisRuntime>,
    held: Arc<GraphEditsHeldForOneCommit>,
    _no_other_writer: OwnedMutexGuard<()>,
    open: bool,
}

impl GraphEditBatch for GraphEditBatchOfALoadedStream {
    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: StreamGraphOperation,
    ) -> BoxFuture<'_, Result<StreamGraphOperation>> {
        let edit = THE_GRAPH_EDITS_THIS_TASK_HOLDS.scope(
            Arc::clone(&self.held),
            self.stream
                .apply_a_stream_graph_operation_to_the_live_graph(graph_operation),
        );
        self.stream.in_this_streams_log_route(async move {
            // Looked at as the edit runs, not as it is asked for: a stop that took the edits
            // back meanwhile leaves none to join.
            if !self
                .stream
                .compiler
                .the_transaction_is_held_for_one_commit()
            {
                return Err(self
                    .stream
                    .the_refusal_of_edits_a_forced_stop_took_back(&self.held)
                    .refusal);
            }
            edit.await
        })
    }

    fn close(mut self: Box<Self>) -> std::result::Result<(), GraphEditBatchRefused> {
        self.open = false;
        self.stream
            .close_the_graph_edits_held_for_one_commit_from_synchronous_code(&self.held)
    }
}

impl Drop for GraphEditBatchOfALoadedStream {
    fn drop(&mut self) {
        *self.stream.thread_holding_a_graph_edit_batch.lock() = None;
        if self.open {
            self.stream
                .take_back_the_graph_edits_held_for_one_commit(&self.held);
        }
    }
}
