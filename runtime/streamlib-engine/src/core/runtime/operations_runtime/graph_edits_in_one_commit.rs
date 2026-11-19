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

use super::{BoxFuture, LiveGraphChangeOnOneStream, commit_live_graph_change};
use crate::core::Result;
use crate::core::compiler::PendingOperation;
use crate::core::graph::graph_snapshot::GraphOnlyNodesAsDrawn;
use crate::core::graph::{GraphEdgeWithComponents, LinkUniqueId, PendingDeletionComponent};
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::runtime::operations::GraphEditBatch;

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
/// announced, beside the link it announces, and the stream as drawn before them.
pub(in crate::core::runtime) struct GraphEditsHeldForOneCommit {
    events_held: Mutex<Vec<(Option<LinkUniqueId>, RuntimeEvent)>>,
    /// Whether an edit held announced the graph changed, announced once as the edits close.
    the_graph_changed: AtomicBool,
    drawn_before: GraphOnlyNodesAsDrawn,
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
                    .await?;
                Ok(made)
            }
            Err(refusal) => {
                self.take_back_the_graph_edits_held_for_one_commit(&held);
                Err(refusal)
            }
        }
    }

    /// Run `edit` on the engine's runtime from synchronous code, joined to `held`.
    pub(in crate::core::runtime) fn block_on_holding<T: Send + 'static>(
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
    pub(in crate::core::runtime) fn hold_the_graph_edits_for_one_commit(
        &self,
    ) -> Arc<GraphEditsHeldForOneCommit> {
        self.compiler.hold_the_transaction_for_one_commit();
        Arc::new(GraphEditsHeldForOneCommit {
            events_held: Mutex::new(Vec::new()),
            the_graph_changed: AtomicBool::new(false),
            drawn_before: self.graph_only_nodes_as_drawn.lock().clone(),
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
        edit: impl FnOnce(&Arc<GraphEditsHeldForOneCommit>) -> Result<T>,
    ) -> Result<T> {
        let held = self.hold_the_graph_edits_for_one_commit();
        match THE_GRAPH_EDITS_THIS_TASK_HOLDS.sync_scope(Arc::clone(&held), || edit(&held)) {
            Ok(made) => {
                self.close_the_graph_edits_held_for_one_commit_from_synchronous_code(&held)?;
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
    /// refusal takes the edits back as far as nothing built or wired holds them.
    pub(in crate::core::runtime) async fn close_the_graph_edits_held_for_one_commit(
        &self,
        held: &GraphEditsHeldForOneCommit,
    ) -> Result<()> {
        let logged = self.release_the_graph_edits_held_for_one_commit(held)?;
        let committed = if logged.is_empty() {
            Ok(())
        } else {
            commit_live_graph_change(&self.compiler, self.live_commit_context()).await
        };
        self.announce_the_graph_edits_one_commit_made(held, &logged, committed)
    }

    /// Close `held` from synchronous code, as `close_the_graph_edits_held_for_one_commit` does.
    pub(in crate::core::runtime) fn close_the_graph_edits_held_for_one_commit_from_synchronous_code(
        &self,
        held: &GraphEditsHeldForOneCommit,
    ) -> Result<()> {
        let logged = self.release_the_graph_edits_held_for_one_commit(held)?;
        let compiler = Arc::clone(&self.compiler);
        let live = self.live_commit_context();
        let committed = if logged.is_empty() {
            Ok(())
        } else {
            self.block_on_the_engines_tokio_runtime(async move {
                commit_live_graph_change(&compiler, live).await
            })
        };
        self.announce_the_graph_edits_one_commit_made(held, &logged, committed)
    }

    /// Release `held`'s hold on the compiler transaction and answer the edits logged under it,
    /// or take them back when they leave a reactive reader fed only by islands.
    fn release_the_graph_edits_held_for_one_commit(
        &self,
        held: &GraphEditsHeldForOneCommit,
    ) -> Result<Vec<PendingOperation>> {
        if let Err(refusal) = self.refuse_a_reactive_reader_only_islands_feed() {
            self.take_back_the_graph_edits_held_for_one_commit(held);
            return Err(refusal);
        }
        Ok(self.compiler.release_the_transaction_held_for_one_commit())
    }

    /// Announce each event `held` kept of what stands once `committed` compiled the edits
    /// `logged`, or take those edits back when the commit refused them.
    fn announce_the_graph_edits_one_commit_made(
        &self,
        held: &GraphEditsHeldForOneCommit,
        logged: &[PendingOperation],
        committed: Result<()>,
    ) -> Result<()> {
        if let Err(refusal) = committed {
            self.compiler
                .take_back_what_operations_did_to_the_graph(logged);
            *self.graph_only_nodes_as_drawn.lock() = held.drawn_before.clone();
            held.events_held.lock().clear();
            return Err(refusal);
        }
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
                    .is_some_and(|link| !link.has::<PendingDeletionComponent>())
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
        Ok(())
    }

    /// Take `held` back: each edit logged under the hold, as far as nothing built or wired
    /// holds it, and the stream as drawn before it; announce none of its events.
    pub(in crate::core::runtime) fn take_back_the_graph_edits_held_for_one_commit(
        &self,
        held: &GraphEditsHeldForOneCommit,
    ) {
        self.compiler
            .take_back_the_transaction_held_for_one_commit();
        *self.graph_only_nodes_as_drawn.lock() = held.drawn_before.clone();
        held.events_held.lock().clear();
    }

    /// Open a batch of graph edits on this stream for one writer, once no other writer's are
    /// open on it: held for one commit until it closes, and taken back when it is dropped open.
    pub(in crate::core::runtime) fn open_a_graph_edit_batch_of_this_stream(
        self: Arc<Self>,
    ) -> GraphEditBatchOfALoadedStream {
        let no_other_writer =
            Arc::clone(&self.one_writer_of_the_graph_at_a_time).blocking_lock_owned();
        let held = self.hold_the_graph_edits_for_one_commit();
        GraphEditBatchOfALoadedStream {
            stream: self,
            held,
            _no_other_writer: no_other_writer,
            open: true,
        }
    }
}

/// One writer's batch of graph edits on one loaded stream, holding the stream's other writers
/// off until it closes or is dropped.
#[must_use = "a batch of graph edits is closed, or taken back when it is dropped"]
pub(crate) struct GraphEditBatchOfALoadedStream {
    stream: Arc<LoadedStreamInThisRuntime>,
    held: Arc<GraphEditsHeldForOneCommit>,
    _no_other_writer: tokio::sync::OwnedMutexGuard<()>,
    open: bool,
}

impl GraphEditBatch for GraphEditBatchOfALoadedStream {
    fn apply_a_stream_graph_operation_live(
        &self,
        graph_operation: StreamGraphOperation,
    ) -> BoxFuture<'_, Result<StreamGraphOperation>> {
        self.stream.in_this_streams_log_route(
            THE_GRAPH_EDITS_THIS_TASK_HOLDS.scope(
                Arc::clone(&self.held),
                self.stream
                    .apply_a_stream_graph_operation_to_the_live_graph(graph_operation),
            ),
        )
    }

    fn close(mut self: Box<Self>) -> Result<()> {
        self.open = false;
        self.stream
            .close_the_graph_edits_held_for_one_commit_from_synchronous_code(&self.held)
    }
}

impl Drop for GraphEditBatchOfALoadedStream {
    fn drop(&mut self) {
        if self.open {
            self.stream
                .take_back_the_graph_edits_held_for_one_commit(&self.held);
        }
    }
}
