// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, NodeRunId, ShutdownChannelComponent, StateComponent,
    ThreadHandleComponent,
};
use crate::core::nodes::NodeState;
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};
use crate::core::runtime::TeardownProgressNoteOfOneStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeThreadKind {
    NativeNode,
    /// The host of a helper process, which walks that helper's shutdown ladder
    /// before it returns.
    HelperProcessHost,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeThreadJoinBudgets {
    native_node_thread: Duration,
    /// Sits above the helper ladder's worst case rather than at the native
    /// budget.
    helper_process_host_thread: Duration,
    native_node_thread_once_shutdown_is_forced: Duration,
}

impl NodeThreadJoinBudgets {
    pub(crate) const ENGINE_CHOSEN: Self = Self {
        native_node_thread: Duration::from_secs(5),
        helper_process_host_thread: Duration::from_secs(10),
        native_node_thread_once_shutdown_is_forced: Duration::from_millis(250),
    };

    fn budget_for(&self, kind: NodeThreadKind) -> Duration {
        match kind {
            NodeThreadKind::NativeNode => self.native_node_thread,
            NodeThreadKind::HelperProcessHost => self.helper_process_host_thread,
        }
    }
}

/// How often the bounded wait re-checks which threads have returned.
const NODE_THREAD_JOIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDisplayNameAndId {
    pub node_run_id: NodeRunId,
    pub node_display_name: String,
}

struct SignalledNodeThread {
    node: NodeDisplayNameAndId,
    join_handle: JoinHandle<()>,
    kind: NodeThreadKind,
}

pub(crate) struct AbandonedNodeThreadStillRunning {
    pub(crate) node: NodeDisplayNameAndId,
    pub(crate) join_handle: JoinHandle<()>,
}

/// `'Name' (id), 'Other' (id)`, in the order given.
struct NodeDisplayNamesAndIds<'a>(&'a [NodeDisplayNameAndId]);

impl std::fmt::Display for NodeDisplayNamesAndIds<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, node) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(
                formatter,
                "'{}' ({})",
                node.node_display_name, node.node_run_id
            )?;
        }
        Ok(())
    }
}

pub struct DescriptionOfTheAbandonedNodeThreads<'a>(pub &'a [NodeDisplayNameAndId]);

impl std::fmt::Display for DescriptionOfTheAbandonedNodeThreads<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} node thread(s) ignored shutdown past their budget and were abandoned: {}. \
             The engine stays alive beneath them until this process exits.",
            self.0.len(),
            NodeDisplayNamesAndIds(self.0),
        )
    }
}

pub(crate) fn stop_node_threads_signalling_every_thread_before_joining_any(
    graph_arc: &Arc<RwLock<Graph>>,
    the_stream_the_nodes_belong_to: &LoadedStreamIdentity,
    node_run_ids: &[NodeRunId],
    budgets: NodeThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
    teardown_progress_note: &TeardownProgressNoteOfOneStream,
    abandoned_node_threads: &Mutex<Vec<AbandonedNodeThreadStillRunning>>,
) -> Vec<NodeDisplayNameAndId> {
    let mut signalled_threads = Vec::with_capacity(node_run_ids.len());
    for node_run_id in node_run_ids {
        the_stream_the_nodes_belong_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerWillDestroyNode {
                node_run_id: node_run_id.clone(),
            },
        );
        tracing::info!("[REMOVE] {}", node_run_id);

        let mut graph = graph_arc.write();
        let Some(node) = graph.traversal_mut().v(node_run_id).first_mut() else {
            continue;
        };
        if let Some(state) = node.get::<StateComponent>() {
            state.transition_to(NodeState::Stopping);
        }
        if let Some(channel) = node.get::<ShutdownChannelComponent>() {
            channel.signal_shutdown();
        }
        let node_display_name = node.display_name.clone();
        if let Some(thread) = node.remove::<ThreadHandleComponent>() {
            signalled_threads.push(SignalledNodeThread {
                node: NodeDisplayNameAndId {
                    node_run_id: node_run_id.clone(),
                    node_display_name,
                },
                join_handle: thread.join_handle,
                kind: thread.kind,
            });
        }
    }

    let abandoned_by_this_removal = join_every_signalled_node_thread_within_its_budget(
        signalled_threads,
        budgets,
        is_shutdown_forced,
        teardown_progress_note,
    );
    let abandoned_nodes: Vec<NodeDisplayNameAndId> = abandoned_by_this_removal
        .iter()
        .map(|thread| thread.node.clone())
        .collect();
    abandoned_node_threads
        .lock()
        .extend(abandoned_by_this_removal);
    crate::core::runtime::count_threads_abandoned_in_this_process(
        abandoned_nodes.len(),
        &format!(
            "removing nodes from the stream `{}`",
            the_stream_the_nodes_belong_to.stream_id
        ),
        &the_stream_the_nodes_belong_to.stream_id,
    );
    abandoned_nodes
}

pub(crate) fn remove_nodes_signalling_every_thread_before_joining_any(
    graph_arc: &Arc<RwLock<Graph>>,
    the_stream_the_nodes_belong_to: &LoadedStreamIdentity,
    node_run_ids: &[NodeRunId],
    budgets: NodeThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
    teardown_progress_note: &TeardownProgressNoteOfOneStream,
    abandoned_node_threads: &Mutex<Vec<AbandonedNodeThreadStillRunning>>,
) -> Result<Vec<NodeDisplayNameAndId>> {
    let abandoned_nodes = stop_node_threads_signalling_every_thread_before_joining_any(
        graph_arc,
        the_stream_the_nodes_belong_to,
        node_run_ids,
        budgets,
        is_shutdown_forced,
        teardown_progress_note,
        abandoned_node_threads,
    );

    let mut first_node_left_behind = None;
    for node_run_id in node_run_ids {
        {
            let mut graph = graph_arc.write();
            if let Some(state) = graph
                .traversal_mut()
                .v(node_run_id)
                .first_mut()
                .and_then(|node| node.get::<StateComponent>())
            {
                state.transition_to(NodeState::Stopped);
            }
            if graph.drop_the_node(node_run_id) {
                first_node_left_behind.get_or_insert_with(|| node_run_id.clone());
                continue;
            }
        }
        the_stream_the_nodes_belong_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerDidDestroyNode {
                node_run_id: node_run_id.clone(),
            },
        );
    }
    if let Some(node_run_id) = first_node_left_behind {
        return Err(Error::GraphError(format!(
            "node '{node_run_id}' was not dropped from the graph"
        )));
    }

    Ok(abandoned_nodes)
}

/// Wait on every signalled thread at once, each within its own budget, and hand
/// back the ones abandoned, in the order they were signalled.
fn join_every_signalled_node_thread_within_its_budget(
    signalled_threads: Vec<SignalledNodeThread>,
    budgets: NodeThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
    teardown_progress_note: &TeardownProgressNoteOfOneStream,
) -> Vec<AbandonedNodeThreadStillRunning> {
    let waiting_began = Instant::now();
    let mut shutdown_forced_at: Option<Instant> = None;
    let mut still_waited_on = signalled_threads;
    let mut abandoned = Vec::new();
    let mut last_noted_waiting_count = None;

    while !still_waited_on.is_empty() {
        if shutdown_forced_at.is_none() && is_shutdown_forced() {
            shutdown_forced_at = Some(Instant::now());
        }

        let mut index = 0;
        while index < still_waited_on.len() {
            let thread = &still_waited_on[index];
            if thread.join_handle.is_finished() {
                join_a_returned_node_thread(still_waited_on.remove(index));
            } else if let Some(reason) =
                why_to_abandon(thread, budgets, waiting_began, shutdown_forced_at)
            {
                let thread = still_waited_on.remove(index);
                abandoned.push(abandon_a_node_thread(thread, reason, budgets));
            } else {
                index += 1;
            }
        }

        if still_waited_on.is_empty() {
            break;
        }
        if last_noted_waiting_count != Some(still_waited_on.len()) {
            last_noted_waiting_count = Some(still_waited_on.len());
            let waiting_on: Vec<NodeDisplayNameAndId> = still_waited_on
                .iter()
                .map(|thread| thread.node.clone())
                .collect();
            teardown_progress_note.note_what_the_teardown_is_waiting_on(format!(
                "the node threads of {}",
                NodeDisplayNamesAndIds(&waiting_on)
            ));
        }
        std::thread::sleep(NODE_THREAD_JOIN_POLL_INTERVAL);
    }
    abandoned
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeThreadAbandonedBecause {
    ItOutlivedItsBudget,
    ShutdownWasForcedWhileItWasInsideItsCallback,
}

fn why_to_abandon(
    thread: &SignalledNodeThread,
    budgets: NodeThreadJoinBudgets,
    waiting_began: Instant,
    shutdown_forced_at: Option<Instant>,
) -> Option<NodeThreadAbandonedBecause> {
    if waiting_began.elapsed() >= budgets.budget_for(thread.kind) {
        return Some(NodeThreadAbandonedBecause::ItOutlivedItsBudget);
    }
    let forced_past_its_grace = thread.kind == NodeThreadKind::NativeNode
        && shutdown_forced_at.is_some_and(|forced_at| {
            forced_at.elapsed() >= budgets.native_node_thread_once_shutdown_is_forced
        });
    forced_past_its_grace
        .then_some(NodeThreadAbandonedBecause::ShutdownWasForcedWhileItWasInsideItsCallback)
}

fn join_a_returned_node_thread(thread: SignalledNodeThread) {
    let node_run_id = &thread.node.node_run_id;
    match thread.join_handle.join() {
        Ok(()) => tracing::info!("[{}] Node thread joined successfully", node_run_id),
        Err(panic_payload) => {
            tracing::error!(
                "[{}] Node thread panicked: {:?}",
                node_run_id,
                panic_payload
            )
        }
    }
}

fn abandon_a_node_thread(
    thread: SignalledNodeThread,
    reason: NodeThreadAbandonedBecause,
    budgets: NodeThreadJoinBudgets,
) -> AbandonedNodeThreadStillRunning {
    let node = thread.node;
    match reason {
        NodeThreadAbandonedBecause::ShutdownWasForcedWhileItWasInsideItsCallback => {
            tracing::error!(
                "[{}] node '{}' was still inside its callback when shutdown was forced; \
                 its thread is abandoned",
                node.node_run_id,
                node.node_display_name,
            )
        }
        NodeThreadAbandonedBecause::ItOutlivedItsBudget => tracing::error!(
            "[{}] node '{}' ignored shutdown for {}s; its thread is abandoned",
            node.node_run_id,
            node.node_display_name,
            budgets.budget_for(thread.kind).as_secs_f64(),
        ),
    }
    AbandonedNodeThreadStillRunning {
        node,
        join_handle: thread.join_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{MockNode, ensure_test_mocks_registered};

    const BUDGETS_A_TEST_CAN_OUTLIVE: NodeThreadJoinBudgets = NodeThreadJoinBudgets {
        native_node_thread: Duration::from_millis(400),
        helper_process_host_thread: Duration::from_millis(1200),
        native_node_thread_once_shutdown_is_forced: Duration::from_millis(100),
    };

    fn never_forced() -> bool {
        false
    }

    #[derive(Clone, Copy)]
    enum ThreadOnceToldToStop {
        ReturnsAfter(Duration),
        IgnoresShutdown,
    }

    /// When each test thread was told to stop, in the order it heard.
    type WhenEachThreadWasToldToStop = Arc<Mutex<Vec<Instant>>>;

    fn a_graph_node_running_a_thread(
        graph: &mut Graph,
        display_name: &str,
        behaviour: ThreadOnceToldToStop,
        kind: NodeThreadKind,
        when_each_thread_was_told_to_stop: &WhenEachThreadWasToldToStop,
    ) -> NodeRunId {
        let mut spec = MockNode::Node::node(Default::default());
        spec.display_name = Some(display_name.to_string());
        let node_run_id = graph
            .traversal_mut()
            .add_v(spec)
            .expect("the node is named")
            .first()
            .expect("the node is added")
            .id
            .clone();
        let node = graph
            .traversal_mut()
            .v(&node_run_id)
            .first_mut()
            .expect("the node exists");
        let mut shutdown_channel = ShutdownChannelComponent::new();
        let told_to_stop = shutdown_channel
            .take_receiver()
            .expect("a fresh channel has its receiver");
        node.insert(shutdown_channel);
        let when_each_thread_was_told_to_stop = Arc::clone(when_each_thread_was_told_to_stop);
        let join_handle = std::thread::spawn(move || {
            let _ = told_to_stop.recv();
            when_each_thread_was_told_to_stop
                .lock()
                .push(Instant::now());
            match behaviour {
                ThreadOnceToldToStop::ReturnsAfter(delay) => std::thread::sleep(delay),
                ThreadOnceToldToStop::IgnoresShutdown => {
                    std::thread::sleep(Duration::from_secs(60))
                }
            }
        });
        node.insert(ThreadHandleComponent { join_handle, kind });
        node_run_id
    }

    fn remove(
        graph: Graph,
        node_run_ids: &[NodeRunId],
        budgets: NodeThreadJoinBudgets,
        is_shutdown_forced: impl Fn() -> bool,
    ) -> (Arc<RwLock<Graph>>, Vec<NodeDisplayNameAndId>) {
        let graph_arc = Arc::new(RwLock::new(graph));
        let abandoned_node_threads = Mutex::new(Vec::new());
        let abandoned = remove_nodes_signalling_every_thread_before_joining_any(
            &graph_arc,
            &LoadedStreamIdentity {
                runtime_id: "node-thread-shutdown-test".to_string(),
                stream_id: "main".to_string(),
                stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
            },
            node_run_ids,
            budgets,
            is_shutdown_forced,
            &TeardownProgressNoteOfOneStream::default(),
            &abandoned_node_threads,
        )
        .expect("the removal completes");
        assert_eq!(
            abandoned_node_threads.lock().len(),
            abandoned.len(),
            "every abandoned thread must be kept for teardown to ask about"
        );
        (graph_arc, abandoned)
    }

    /// Fail-without-fix: joining each thread before telling the next to stop
    /// makes every one hear it only after the one ahead has returned, a whole
    /// ladder apart.
    #[test]
    fn every_thread_is_told_to_stop_before_any_is_waited_on() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let ladder = Duration::from_millis(600);
        let node_run_ids: Vec<NodeRunId> = ["First", "Second", "Third"]
            .into_iter()
            .map(|name| {
                a_graph_node_running_a_thread(
                    &mut graph,
                    name,
                    ThreadOnceToldToStop::ReturnsAfter(ladder),
                    NodeThreadKind::HelperProcessHost,
                    &told_to_stop,
                )
            })
            .collect();

        let (graph_arc, abandoned) = remove(
            graph,
            &node_run_ids,
            BUDGETS_A_TEST_CAN_OUTLIVE,
            never_forced,
        );

        assert!(abandoned.is_empty(), "no thread outlived its budget");
        let told_to_stop = told_to_stop.lock();
        assert_eq!(told_to_stop.len(), 3);
        let first_heard = told_to_stop.iter().min().expect("three threads heard");
        let last_heard = told_to_stop.iter().max().expect("three threads heard");
        assert!(
            last_heard.duration_since(*first_heard) < ladder,
            "the last thread heard {:?} after the first — it was told only once another \
             had returned",
            last_heard.duration_since(*first_heard)
        );
        assert!(graph_arc.read().traversal().v(()).ids().is_empty());
    }

    /// A native thread that ignores shutdown is abandoned at its budget and
    /// named, and every node is removed all the same.
    #[test]
    fn a_native_thread_that_ignores_shutdown_is_abandoned_at_its_budget_and_named() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let stuck = a_graph_node_running_a_thread(
            &mut graph,
            "stuckencoder",
            ThreadOnceToldToStop::IgnoresShutdown,
            NodeThreadKind::NativeNode,
            &told_to_stop,
        );
        let cooperative = a_graph_node_running_a_thread(
            &mut graph,
            "TidySink",
            ThreadOnceToldToStop::ReturnsAfter(Duration::ZERO),
            NodeThreadKind::NativeNode,
            &told_to_stop,
        );

        let started = Instant::now();
        let (graph_arc, abandoned) = remove(
            graph,
            &[stuck.clone(), cooperative],
            BUDGETS_A_TEST_CAN_OUTLIVE,
            never_forced,
        );

        let elapsed = started.elapsed();
        assert!(
            elapsed >= BUDGETS_A_TEST_CAN_OUTLIVE.native_node_thread,
            "the stuck thread was let go after {elapsed:?}, before its native budget"
        );
        assert_eq!(
            abandoned,
            vec![NodeDisplayNameAndId {
                node_run_id: stuck,
                node_display_name: "stuckencoder".to_string(),
            }]
        );
        assert!(
            graph_arc.read().traversal().v(()).ids().is_empty(),
            "an abandoned node's node must still leave the graph"
        );

        let description = DescriptionOfTheAbandonedNodeThreads(&abandoned).to_string();
        assert!(
            description.contains("'stuckencoder'")
                && description.contains(abandoned[0].node_run_id.as_str()),
            "the description must name the node by display name and id: {description}"
        );
    }

    /// A helper's host thread gets the ladder's budget, never the native one.
    #[test]
    fn a_helper_host_thread_is_waited_on_past_the_native_budget() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let helper_host = a_graph_node_running_a_thread(
            &mut graph,
            "PythonProbe",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(700)),
            NodeThreadKind::HelperProcessHost,
            &told_to_stop,
        );

        let (_, abandoned) = remove(
            graph,
            &[helper_host],
            BUDGETS_A_TEST_CAN_OUTLIVE,
            never_forced,
        );

        assert!(
            abandoned.is_empty(),
            "a helper walking its ladder was abandoned at the native budget"
        );
    }

    /// A second interrupt abandons a native thread still in its callback soon
    /// after, and keeps waiting on a helper's host thread, whose ladder now skips
    /// to terminating the process group.
    #[test]
    fn a_forced_shutdown_abandons_a_stuck_native_thread_and_still_waits_on_a_helper_host() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let stuck = a_graph_node_running_a_thread(
            &mut graph,
            "stuckencoder",
            ThreadOnceToldToStop::IgnoresShutdown,
            NodeThreadKind::NativeNode,
            &told_to_stop,
        );
        let helper_host = a_graph_node_running_a_thread(
            &mut graph,
            "PythonProbe",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(500)),
            NodeThreadKind::HelperProcessHost,
            &told_to_stop,
        );

        let started = Instant::now();
        let (_, abandoned) = remove(
            graph,
            &[stuck.clone(), helper_host],
            NodeThreadJoinBudgets::ENGINE_CHOSEN,
            || true,
        );

        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a forced shutdown still waited out the native budget: {:?}",
            started.elapsed()
        );
        assert_eq!(
            abandoned
                .iter()
                .map(|node| node.node_run_id.clone())
                .collect::<Vec<_>>(),
            vec![stuck],
            "only the native thread is abandoned; the helper host returned"
        );
    }

    #[test]
    fn a_native_thread_returning_promptly_is_joined_even_when_shutdown_is_forced() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let prompt = a_graph_node_running_a_thread(
            &mut graph,
            "PromptSink",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(20)),
            NodeThreadKind::NativeNode,
            &told_to_stop,
        );

        let (_, abandoned) = remove(
            graph,
            &[prompt],
            NodeThreadJoinBudgets::ENGINE_CHOSEN,
            || true,
        );

        assert!(
            abandoned.is_empty(),
            "a native thread returning at once was abandoned: {abandoned:?}"
        );
    }
}
