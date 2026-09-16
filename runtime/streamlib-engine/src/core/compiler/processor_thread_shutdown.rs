// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Removing processors: every thread told to stop before any is waited on, each
//! wait bounded, and a thread that outlives its budget abandoned and named.
//!
//! `docs/plan/ARCHITECTURE.md` §Processor model and §Language SDKs: the engine
//! stops every helper at once, never one after another, and a native processor
//! thread that ignores shutdown past its budget is abandoned rather than
//! joined. A second interrupt abandons a native thread still inside its callback,
//! while a helper's host thread is still waited on — its ladder skips to
//! terminating the helper's process group, which is what that wait now bounds.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
    ThreadHandleComponent,
};
use crate::core::processors::ProcessorState;
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};

/// What a processor thread runs, which decides how long its join may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorThreadKind {
    /// A processor whose callbacks run on this thread.
    NativeProcessor,
    /// The host of a helper process, which walks that helper's shutdown ladder
    /// before it returns.
    HelperProcessHost,
}

/// How long each kind of processor thread has to return once told to stop.
///
/// Engine-chosen and not authorable: the plan makes every shutdown budget the
/// engine's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessorThreadJoinBudgets {
    native_processor_thread: Duration,
    /// Sits above the helper ladder's worst case rather than at the native
    /// budget.
    helper_process_host_thread: Duration,
    /// How long a native thread has once shutdown is forced. Not zero: a thread
    /// already returning when the second interrupt lands is not inside its
    /// callback, and abandoning it would leak the engine over a processor that
    /// was never stuck.
    native_processor_thread_once_shutdown_is_forced: Duration,
}

impl ProcessorThreadJoinBudgets {
    pub(crate) const ENGINE_CHOSEN: Self = Self {
        native_processor_thread: Duration::from_secs(5),
        helper_process_host_thread: Duration::from_secs(10),
        native_processor_thread_once_shutdown_is_forced: Duration::from_millis(250),
    };

    fn budget_for(&self, kind: ProcessorThreadKind) -> Duration {
        match kind {
            ProcessorThreadKind::NativeProcessor => self.native_processor_thread,
            ProcessorThreadKind::HelperProcessHost => self.helper_process_host_thread,
        }
    }
}

/// How often the bounded wait re-checks which threads have returned.
const PROCESSOR_THREAD_JOIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The processor a thread runs, by the two names a person reads it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorDisplayNameAndId {
    pub processor_id: ProcessorUniqueId,
    pub processor_display_name: String,
}

/// A processor thread already told to stop, waiting to be joined.
struct SignalledProcessorThread {
    processor: ProcessorDisplayNameAndId,
    join_handle: JoinHandle<()>,
    kind: ProcessorThreadKind,
}

/// An abandoned processor thread and the handle that says whether it has since
/// returned.
pub(crate) struct AbandonedProcessorThreadStillRunning {
    pub(crate) processor: ProcessorDisplayNameAndId,
    pub(crate) join_handle: JoinHandle<()>,
}

/// `'Name' (id), 'Other' (id)`, in the order given.
struct DisplayNamesAndIds<'a>(&'a [ProcessorDisplayNameAndId]);

impl std::fmt::Display for DisplayNamesAndIds<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, processor) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(
                formatter,
                "'{}' ({})",
                processor.processor_display_name, processor.processor_id
            )?;
        }
        Ok(())
    }
}

/// What to say about processors whose threads were abandoned.
pub struct DescriptionOfTheAbandonedProcessorThreads<'a>(pub &'a [ProcessorDisplayNameAndId]);

impl std::fmt::Display for DescriptionOfTheAbandonedProcessorThreads<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} processor thread(s) ignored shutdown past their budget and were abandoned: {}. \
             The engine stays alive beneath them until this process exits.",
            self.0.len(),
            DisplayNamesAndIds(self.0),
        )
    }
}

/// Remove `processor_ids` from the graph, telling every one of their threads to
/// stop before waiting on any.
///
/// Every node is removed whether its thread returned or not. The threads
/// abandoned are added to `abandoned_processor_threads` before any node is
/// dropped, and named in the return value.
pub(crate) fn remove_processors_signalling_every_thread_before_joining_any(
    graph_arc: &Arc<RwLock<Graph>>,
    processor_ids: &[ProcessorUniqueId],
    budgets: ProcessorThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
    abandoned_processor_threads: &Mutex<Vec<AbandonedProcessorThreadStillRunning>>,
) -> Result<Vec<ProcessorDisplayNameAndId>> {
    let mut signalled_threads = Vec::with_capacity(processor_ids.len());
    for processor_id in processor_ids {
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::CompilerWillDestroyProcessor {
                processor_id: processor_id.clone(),
            }),
        );
        tracing::info!("[REMOVE] {}", processor_id);

        // The lock is released before any wait: a processor on its way out may
        // need it to finish the runtime operation it is inside.
        let mut graph = graph_arc.write();
        let Some(node) = graph.traversal_mut().v(processor_id).first_mut() else {
            continue;
        };
        if let Some(state) = node.get::<StateComponent>() {
            state.transition_to(ProcessorState::Stopping);
        }
        if let Some(channel) = node.get::<ShutdownChannelComponent>() {
            channel.signal_shutdown();
        }
        let processor_display_name = node.display_name.clone();
        if let Some(thread) = node.remove::<ThreadHandleComponent>() {
            signalled_threads.push(SignalledProcessorThread {
                processor: ProcessorDisplayNameAndId {
                    processor_id: processor_id.clone(),
                    processor_display_name,
                },
                join_handle: thread.join_handle,
                kind: thread.kind,
            });
        }
    }

    let abandoned_by_this_removal = join_every_signalled_processor_thread_within_its_budget(
        signalled_threads,
        budgets,
        is_shutdown_forced,
    );
    let abandoned_processors: Vec<ProcessorDisplayNameAndId> = abandoned_by_this_removal
        .iter()
        .map(|thread| thread.processor.clone())
        .collect();
    abandoned_processor_threads
        .lock()
        .extend(abandoned_by_this_removal);

    let mut first_node_left_behind = None;
    for processor_id in processor_ids {
        {
            let mut graph = graph_arc.write();
            if let Some(node) = graph.traversal_mut().v(processor_id).first_mut() {
                if let Some(state) = node.get::<StateComponent>() {
                    state.transition_to(ProcessorState::Stopped);
                }
            }
            if graph.traversal_mut().v(processor_id).drop().exists() {
                first_node_left_behind.get_or_insert_with(|| processor_id.clone());
                continue;
            }
        }
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::CompilerDidDestroyProcessor {
                processor_id: processor_id.clone(),
            }),
        );
    }
    if let Some(processor_id) = first_node_left_behind {
        return Err(Error::GraphError(format!(
            "processor '{processor_id}' was not dropped from the graph"
        )));
    }

    Ok(abandoned_processors)
}

/// Wait on every signalled thread at once, each within its own budget, and hand
/// back the ones abandoned, in the order they were signalled.
fn join_every_signalled_processor_thread_within_its_budget(
    signalled_threads: Vec<SignalledProcessorThread>,
    budgets: ProcessorThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
) -> Vec<AbandonedProcessorThreadStillRunning> {
    let waiting_began = Instant::now();
    let mut shutdown_forced_at: Option<Instant> = None;
    let mut still_waited_on = signalled_threads;
    let mut abandoned = Vec::new();
    let mut last_noted_waiting_count = None;

    while !still_waited_on.is_empty() {
        if shutdown_forced_at.is_none() && is_shutdown_forced() {
            shutdown_forced_at = Some(Instant::now());
        }

        // `remove`, never `swap_remove`: the refusal names processors in the
        // order they were removed.
        let mut index = 0;
        while index < still_waited_on.len() {
            let thread = &still_waited_on[index];
            if thread.join_handle.is_finished() {
                join_a_returned_processor_thread(still_waited_on.remove(index));
            } else if let Some(reason) =
                why_to_abandon(thread, budgets, waiting_began, shutdown_forced_at)
            {
                let thread = still_waited_on.remove(index);
                abandoned.push(abandon_a_processor_thread(thread, reason, budgets));
            } else {
                index += 1;
            }
        }

        if still_waited_on.is_empty() {
            break;
        }
        if last_noted_waiting_count != Some(still_waited_on.len()) {
            last_noted_waiting_count = Some(still_waited_on.len());
            let waiting_on: Vec<ProcessorDisplayNameAndId> = still_waited_on
                .iter()
                .map(|thread| thread.processor.clone())
                .collect();
            crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(format!(
                "the processor threads of {}",
                DisplayNamesAndIds(&waiting_on)
            ));
        }
        std::thread::sleep(PROCESSOR_THREAD_JOIN_POLL_INTERVAL);
    }
    abandoned
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessorThreadAbandonedBecause {
    ItOutlivedItsBudget,
    ShutdownWasForcedWhileItWasInsideItsCallback,
}

fn why_to_abandon(
    thread: &SignalledProcessorThread,
    budgets: ProcessorThreadJoinBudgets,
    waiting_began: Instant,
    shutdown_forced_at: Option<Instant>,
) -> Option<ProcessorThreadAbandonedBecause> {
    if waiting_began.elapsed() >= budgets.budget_for(thread.kind) {
        return Some(ProcessorThreadAbandonedBecause::ItOutlivedItsBudget);
    }
    let forced_past_its_grace = thread.kind == ProcessorThreadKind::NativeProcessor
        && shutdown_forced_at.is_some_and(|forced_at| {
            forced_at.elapsed() >= budgets.native_processor_thread_once_shutdown_is_forced
        });
    forced_past_its_grace
        .then_some(ProcessorThreadAbandonedBecause::ShutdownWasForcedWhileItWasInsideItsCallback)
}

fn join_a_returned_processor_thread(thread: SignalledProcessorThread) {
    let processor_id = &thread.processor.processor_id;
    match thread.join_handle.join() {
        Ok(()) => tracing::info!("[{}] Processor thread joined successfully", processor_id),
        Err(panic_payload) => {
            tracing::error!(
                "[{}] Processor thread panicked: {:?}",
                processor_id,
                panic_payload
            )
        }
    }
}

fn abandon_a_processor_thread(
    thread: SignalledProcessorThread,
    reason: ProcessorThreadAbandonedBecause,
    budgets: ProcessorThreadJoinBudgets,
) -> AbandonedProcessorThreadStillRunning {
    let processor = thread.processor;
    match reason {
        ProcessorThreadAbandonedBecause::ShutdownWasForcedWhileItWasInsideItsCallback => {
            tracing::error!(
                "[{}] processor '{}' was still inside its callback when shutdown was forced; \
                 its thread is abandoned",
                processor.processor_id,
                processor.processor_display_name,
            )
        }
        ProcessorThreadAbandonedBecause::ItOutlivedItsBudget => tracing::error!(
            "[{}] processor '{}' ignored shutdown for {}s; its thread is abandoned",
            processor.processor_id,
            processor.processor_display_name,
            budgets.budget_for(thread.kind).as_secs_f64(),
        ),
    }
    AbandonedProcessorThreadStillRunning {
        processor,
        join_handle: thread.join_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{MockProcessor, ensure_test_mocks_registered};

    const BUDGETS_A_TEST_CAN_OUTLIVE: ProcessorThreadJoinBudgets = ProcessorThreadJoinBudgets {
        native_processor_thread: Duration::from_millis(400),
        helper_process_host_thread: Duration::from_millis(1200),
        native_processor_thread_once_shutdown_is_forced: Duration::from_millis(100),
    };

    fn never_forced() -> bool {
        false
    }

    /// How a processor's thread behaves once it is told to stop.
    #[derive(Clone, Copy)]
    enum ThreadOnceToldToStop {
        ReturnsAfter(Duration),
        IgnoresShutdown,
    }

    /// When each test thread was told to stop, in the order it heard.
    type WhenEachThreadWasToldToStop = Arc<Mutex<Vec<Instant>>>;

    /// A processor node in `graph` carrying a shutdown channel and a thread
    /// that behaves as `behaviour` says, the way the spawn op leaves one.
    fn a_processor_node_running_a_thread(
        graph: &mut Graph,
        display_name: &str,
        behaviour: ThreadOnceToldToStop,
        kind: ProcessorThreadKind,
        when_each_thread_was_told_to_stop: &WhenEachThreadWasToldToStop,
    ) -> ProcessorUniqueId {
        let mut spec = MockProcessor::Processor::node(Default::default());
        spec.display_name = Some(display_name.to_string());
        let processor_id = graph
            .traversal_mut()
            .add_v(spec)
            .first()
            .expect("the node is added")
            .id
            .clone();
        let node = graph
            .traversal_mut()
            .v(&processor_id)
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
        processor_id
    }

    fn remove(
        graph: Graph,
        processor_ids: &[ProcessorUniqueId],
        budgets: ProcessorThreadJoinBudgets,
        is_shutdown_forced: impl Fn() -> bool,
    ) -> (Arc<RwLock<Graph>>, Vec<ProcessorDisplayNameAndId>) {
        let graph_arc = Arc::new(RwLock::new(graph));
        let abandoned_processor_threads = Mutex::new(Vec::new());
        let abandoned = remove_processors_signalling_every_thread_before_joining_any(
            &graph_arc,
            processor_ids,
            budgets,
            is_shutdown_forced,
            &abandoned_processor_threads,
        )
        .expect("the removal completes");
        assert_eq!(
            abandoned_processor_threads.lock().len(),
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
        let processor_ids: Vec<ProcessorUniqueId> = ["First", "Second", "Third"]
            .into_iter()
            .map(|name| {
                a_processor_node_running_a_thread(
                    &mut graph,
                    name,
                    ThreadOnceToldToStop::ReturnsAfter(ladder),
                    ProcessorThreadKind::HelperProcessHost,
                    &told_to_stop,
                )
            })
            .collect();

        let (graph_arc, abandoned) = remove(
            graph,
            &processor_ids,
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
        let stuck = a_processor_node_running_a_thread(
            &mut graph,
            "StuckEncoder",
            ThreadOnceToldToStop::IgnoresShutdown,
            ProcessorThreadKind::NativeProcessor,
            &told_to_stop,
        );
        let cooperative = a_processor_node_running_a_thread(
            &mut graph,
            "TidySink",
            ThreadOnceToldToStop::ReturnsAfter(Duration::ZERO),
            ProcessorThreadKind::NativeProcessor,
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
            elapsed >= BUDGETS_A_TEST_CAN_OUTLIVE.native_processor_thread,
            "the stuck thread was let go after {elapsed:?}, before its native budget"
        );
        assert_eq!(
            abandoned,
            vec![ProcessorDisplayNameAndId {
                processor_id: stuck,
                processor_display_name: "StuckEncoder".to_string(),
            }]
        );
        assert!(
            graph_arc.read().traversal().v(()).ids().is_empty(),
            "an abandoned processor's node must still leave the graph"
        );

        let description = DescriptionOfTheAbandonedProcessorThreads(&abandoned).to_string();
        assert!(
            description.contains("'StuckEncoder'")
                && description.contains(abandoned[0].processor_id.as_str()),
            "the description must name the processor by display name and id: {description}"
        );
    }

    /// A helper's host thread gets the ladder's budget, never the native one.
    #[test]
    fn a_helper_host_thread_is_waited_on_past_the_native_budget() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let helper_host = a_processor_node_running_a_thread(
            &mut graph,
            "PythonProbe",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(700)),
            ProcessorThreadKind::HelperProcessHost,
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
        let stuck = a_processor_node_running_a_thread(
            &mut graph,
            "StuckEncoder",
            ThreadOnceToldToStop::IgnoresShutdown,
            ProcessorThreadKind::NativeProcessor,
            &told_to_stop,
        );
        let helper_host = a_processor_node_running_a_thread(
            &mut graph,
            "PythonProbe",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(500)),
            ProcessorThreadKind::HelperProcessHost,
            &told_to_stop,
        );

        let started = Instant::now();
        let (_, abandoned) = remove(
            graph,
            &[stuck.clone(), helper_host],
            ProcessorThreadJoinBudgets::ENGINE_CHOSEN,
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
                .map(|processor| processor.processor_id.clone())
                .collect::<Vec<_>>(),
            vec![stuck],
            "only the native thread is abandoned; the helper host returned"
        );
    }

    /// A native thread already on its way out when shutdown is forced is not
    /// inside its callback, so it is joined rather than abandoned.
    ///
    /// Fail-without-fix: abandoning every native thread not yet finished at the
    /// first check after the force catches this one microseconds after its
    /// signal, and `run()` raises naming a processor that was never stuck.
    #[test]
    fn a_native_thread_returning_promptly_is_joined_even_when_shutdown_is_forced() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let told_to_stop = WhenEachThreadWasToldToStop::default();
        let prompt = a_processor_node_running_a_thread(
            &mut graph,
            "PromptSink",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(20)),
            ProcessorThreadKind::NativeProcessor,
            &told_to_stop,
        );

        let (_, abandoned) = remove(
            graph,
            &[prompt],
            ProcessorThreadJoinBudgets::ENGINE_CHOSEN,
            || true,
        );

        assert!(
            abandoned.is_empty(),
            "a native thread returning at once was abandoned: {abandoned:?}"
        );
    }
}
