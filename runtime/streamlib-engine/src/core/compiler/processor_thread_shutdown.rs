// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Removing processors: every thread told to stop before any is waited on, each
//! wait bounded, and a thread that outlives its budget abandoned and named.
//!
//! `docs/plan/ARCHITECTURE.md` §Processor model and §Language SDKs: the engine
//! stops every helper at once, never one after another, and a native processor
//! thread that ignores shutdown past its budget is abandoned rather than
//! joined. A second interrupt abandons a native thread still inside its callback
//! at once, while a helper's host thread is still waited on — its ladder skips to
//! terminating the helper's process group, which is what that wait now bounds.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, ProcessorUniqueId, ShutdownChannelComponent, StateComponent,
    ThreadHandleComponent,
};
use crate::core::processors::ProcessorState;
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};

/// How long each kind of processor thread has to return once told to stop.
///
/// Engine-chosen and not authorable: the plan makes every shutdown budget the
/// engine's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessorThreadJoinBudgets {
    pub(crate) native_processor_thread: Duration,
    /// A helper's host thread walks that helper's whole shutdown ladder, so its
    /// budget sits above the ladder's worst case rather than at the native one.
    pub(crate) helper_process_host_thread: Duration,
}

impl ProcessorThreadJoinBudgets {
    pub(crate) const ENGINE_CHOSEN: Self = Self {
        native_processor_thread: Duration::from_secs(5),
        helper_process_host_thread: Duration::from_secs(10),
    };
}

/// How often the bounded wait re-checks which threads have returned.
const PROCESSOR_THREAD_JOIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A processor thread already told to stop, waiting to be joined.
pub(crate) struct SignalledProcessorThread {
    pub(crate) abandoned_if_it_outlives_its_budget: AbandonedProcessorThread,
    pub(crate) join_handle: JoinHandle<()>,
    pub(crate) hosts_a_helper_process: bool,
}

/// A processor whose thread ignored shutdown past its budget and was let go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedProcessorThread {
    pub processor_id: ProcessorUniqueId,
    pub processor_display_name: String,
}

/// An abandoned processor thread and the handle that says whether it has since
/// returned.
pub(crate) struct AbandonedProcessorThreadStillRunning {
    pub(crate) abandoned: AbandonedProcessorThread,
    pub(crate) join_handle: JoinHandle<()>,
}

/// The refusal that names every abandoned processor by display name and id.
pub fn refusal_naming_the_abandoned_processor_threads(
    abandoned: &[AbandonedProcessorThread],
) -> Error {
    Error::Runtime(format!(
        "{} processor thread(s) ignored shutdown past their budget and were abandoned: {}. \
         The engine stays alive beneath them until this process exits.",
        abandoned.len(),
        names_of(abandoned),
    ))
}

fn names_of(processor_threads: &[AbandonedProcessorThread]) -> String {
    processor_threads
        .iter()
        .map(|thread| {
            format!(
                "'{}' ({})",
                thread.processor_display_name, thread.processor_id
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Remove `processor_ids` from the graph, telling every one of their threads to
/// stop before waiting on any, and hand back the threads that were abandoned.
///
/// Every node is removed whether its thread returned or not.
pub(crate) fn remove_processors_signalling_every_thread_before_joining_any(
    graph_arc: &Arc<RwLock<Graph>>,
    processor_ids: &[ProcessorUniqueId],
    budgets: ProcessorThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
) -> Result<Vec<AbandonedProcessorThreadStillRunning>> {
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
                abandoned_if_it_outlives_its_budget: AbandonedProcessorThread {
                    processor_id: processor_id.clone(),
                    processor_display_name,
                },
                join_handle: thread.join_handle,
                hosts_a_helper_process: thread.hosts_a_helper_process,
            });
        }
    }

    let abandoned = join_every_signalled_processor_thread_within_its_budget(
        signalled_threads,
        budgets,
        is_shutdown_forced,
    );

    for processor_id in processor_ids {
        {
            let mut graph = graph_arc.write();
            if let Some(node) = graph.traversal_mut().v(processor_id).first_mut() {
                if let Some(state) = node.get::<StateComponent>() {
                    state.transition_to(ProcessorState::Stopped);
                }
            }
            if graph.traversal_mut().v(processor_id).drop().exists() {
                return Err(Error::GraphError("value was not dropped".into()));
            }
        }
        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::CompilerDidDestroyProcessor {
                processor_id: processor_id.clone(),
            }),
        );
    }

    Ok(abandoned)
}

/// Wait on every signalled thread at once, each within its own budget, and hand
/// back the ones abandoned.
pub(crate) fn join_every_signalled_processor_thread_within_its_budget(
    signalled_threads: Vec<SignalledProcessorThread>,
    budgets: ProcessorThreadJoinBudgets,
    is_shutdown_forced: impl Fn() -> bool,
) -> Vec<AbandonedProcessorThreadStillRunning> {
    let waiting_began = Instant::now();
    let mut still_waited_on = signalled_threads;
    let mut abandoned = Vec::new();
    let mut last_noted_waiting_count = None;

    while !still_waited_on.is_empty() {
        let forced = is_shutdown_forced();
        let mut index = 0;
        while index < still_waited_on.len() {
            let thread = &still_waited_on[index];
            let budget = if thread.hosts_a_helper_process {
                budgets.helper_process_host_thread
            } else {
                budgets.native_processor_thread
            };
            if thread.join_handle.is_finished() {
                let thread = still_waited_on.swap_remove(index);
                join_a_returned_processor_thread(thread);
                continue;
            }
            let abandoned_by_force = forced && !thread.hosts_a_helper_process;
            if abandoned_by_force || waiting_began.elapsed() >= budget {
                let thread = still_waited_on.swap_remove(index);
                abandoned.push(abandon_a_processor_thread(
                    thread,
                    budget,
                    abandoned_by_force,
                ));
                continue;
            }
            index += 1;
        }

        if last_noted_waiting_count != Some(still_waited_on.len()) && !still_waited_on.is_empty() {
            last_noted_waiting_count = Some(still_waited_on.len());
            let waiting_on: Vec<AbandonedProcessorThread> = still_waited_on
                .iter()
                .map(|thread| thread.abandoned_if_it_outlives_its_budget.clone())
                .collect();
            crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(format!(
                "the processor threads of {}",
                names_of(&waiting_on)
            ));
        }
        if !still_waited_on.is_empty() {
            std::thread::sleep(PROCESSOR_THREAD_JOIN_POLL_INTERVAL);
        }
    }
    abandoned
}

fn join_a_returned_processor_thread(thread: SignalledProcessorThread) {
    let named = &thread.abandoned_if_it_outlives_its_budget;
    match thread.join_handle.join() {
        Ok(()) => tracing::info!(
            "[{}] Processor thread joined successfully",
            named.processor_id
        ),
        Err(panic_payload) => tracing::error!(
            "[{}] Processor thread panicked: {:?}",
            named.processor_id,
            panic_payload
        ),
    }
}

fn abandon_a_processor_thread(
    thread: SignalledProcessorThread,
    budget: Duration,
    abandoned_by_force: bool,
) -> AbandonedProcessorThreadStillRunning {
    let named = thread.abandoned_if_it_outlives_its_budget;
    if abandoned_by_force {
        tracing::error!(
            "[{}] processor '{}' was still inside its callback when shutdown was forced; its \
             thread is abandoned",
            named.processor_id,
            named.processor_display_name,
        );
    } else {
        tracing::error!(
            "[{}] processor '{}' ignored shutdown for {}s; its thread is abandoned",
            named.processor_id,
            named.processor_display_name,
            budget.as_secs_f64(),
        );
    }
    AbandonedProcessorThreadStillRunning {
        abandoned: named,
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

    /// A processor node in `graph` carrying a shutdown channel and a thread
    /// that behaves as `behaviour` says, the way the spawn op leaves one.
    fn a_processor_node_running_a_thread(
        graph: &mut Graph,
        display_name: &str,
        behaviour: ThreadOnceToldToStop,
        hosts_a_helper_process: bool,
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
        let join_handle = std::thread::spawn(move || {
            let _ = told_to_stop.recv();
            match behaviour {
                ThreadOnceToldToStop::ReturnsAfter(delay) => std::thread::sleep(delay),
                ThreadOnceToldToStop::IgnoresShutdown => {
                    std::thread::sleep(Duration::from_secs(60))
                }
            }
        });
        node.insert(ThreadHandleComponent {
            join_handle,
            hosts_a_helper_process,
        });
        processor_id
    }

    /// Three helpers each needing most of a second to walk their ladder cost
    /// about one ladder, not three.
    ///
    /// Fail-without-fix: joining each thread before signalling the next makes
    /// every helper wait out the ones ahead of it, and this takes over two
    /// seconds.
    #[test]
    fn every_thread_is_told_to_stop_before_any_is_waited_on() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let ladder = Duration::from_millis(700);
        let processor_ids: Vec<ProcessorUniqueId> = ["First", "Second", "Third"]
            .into_iter()
            .map(|name| {
                a_processor_node_running_a_thread(
                    &mut graph,
                    name,
                    ThreadOnceToldToStop::ReturnsAfter(ladder),
                    true,
                )
            })
            .collect();
        let graph_arc = Arc::new(RwLock::new(graph));

        let started = Instant::now();
        let abandoned = remove_processors_signalling_every_thread_before_joining_any(
            &graph_arc,
            &processor_ids,
            BUDGETS_A_TEST_CAN_OUTLIVE,
            never_forced,
        )
        .expect("the removal completes");

        assert!(abandoned.is_empty(), "no thread outlived its budget");
        assert!(
            started.elapsed() < ladder * 2,
            "three ladders took {:?}; they were walked one after another",
            started.elapsed()
        );
        assert!(graph_arc.read().traversal().v(()).ids().is_empty());
    }

    /// A native thread that ignores shutdown is abandoned at its budget and
    /// named, and every node is removed all the same.
    #[test]
    fn a_native_thread_that_ignores_shutdown_is_abandoned_at_its_budget_and_named() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let stuck = a_processor_node_running_a_thread(
            &mut graph,
            "StuckEncoder",
            ThreadOnceToldToStop::IgnoresShutdown,
            false,
        );
        let cooperative = a_processor_node_running_a_thread(
            &mut graph,
            "TidySink",
            ThreadOnceToldToStop::ReturnsAfter(Duration::ZERO),
            false,
        );
        let graph_arc = Arc::new(RwLock::new(graph));

        let started = Instant::now();
        let abandoned = remove_processors_signalling_every_thread_before_joining_any(
            &graph_arc,
            &[stuck.clone(), cooperative],
            BUDGETS_A_TEST_CAN_OUTLIVE,
            never_forced,
        )
        .expect("the removal completes");

        let elapsed = started.elapsed();
        assert!(
            elapsed >= BUDGETS_A_TEST_CAN_OUTLIVE.native_processor_thread
                && elapsed < BUDGETS_A_TEST_CAN_OUTLIVE.helper_process_host_thread,
            "the stuck thread was let go after {elapsed:?}, not at its native budget"
        );
        let abandoned: Vec<AbandonedProcessorThread> = abandoned
            .into_iter()
            .map(|thread| thread.abandoned)
            .collect();
        assert_eq!(
            abandoned,
            vec![AbandonedProcessorThread {
                processor_id: stuck,
                processor_display_name: "StuckEncoder".to_string(),
            }]
        );
        assert!(
            graph_arc.read().traversal().v(()).ids().is_empty(),
            "an abandoned processor's node must still leave the graph"
        );

        let refusal = refusal_naming_the_abandoned_processor_threads(&abandoned).to_string();
        assert!(
            refusal.contains("'StuckEncoder'")
                && refusal.contains(abandoned[0].processor_id.as_str()),
            "the refusal must name the processor by display name and id: {refusal}"
        );
    }

    /// A helper's host thread gets the ladder's budget, never the native one.
    #[test]
    fn a_helper_host_thread_is_waited_on_past_the_native_budget() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let helper_host = a_processor_node_running_a_thread(
            &mut graph,
            "PythonProbe",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(700)),
            true,
        );
        let graph_arc = Arc::new(RwLock::new(graph));

        let abandoned = remove_processors_signalling_every_thread_before_joining_any(
            &graph_arc,
            &[helper_host],
            BUDGETS_A_TEST_CAN_OUTLIVE,
            never_forced,
        )
        .expect("the removal completes");

        assert!(
            abandoned.is_empty(),
            "a helper walking its ladder was abandoned at the native budget"
        );
    }

    /// A second interrupt abandons a native thread still in its callback at
    /// once, and keeps waiting on a helper's host thread, whose ladder now skips
    /// to terminating the process group.
    #[test]
    fn a_forced_shutdown_abandons_a_native_thread_at_once_and_still_waits_on_a_helper_host() {
        ensure_test_mocks_registered();
        let mut graph = Graph::new();
        let stuck = a_processor_node_running_a_thread(
            &mut graph,
            "StuckEncoder",
            ThreadOnceToldToStop::IgnoresShutdown,
            false,
        );
        let helper_host = a_processor_node_running_a_thread(
            &mut graph,
            "PythonProbe",
            ThreadOnceToldToStop::ReturnsAfter(Duration::from_millis(250)),
            true,
        );
        let graph_arc = Arc::new(RwLock::new(graph));

        let started = Instant::now();
        let abandoned = remove_processors_signalling_every_thread_before_joining_any(
            &graph_arc,
            &[stuck.clone(), helper_host],
            ProcessorThreadJoinBudgets::ENGINE_CHOSEN,
            || true,
        )
        .expect("the removal completes");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a forced shutdown still waited on the native thread: {:?}",
            started.elapsed()
        );
        assert_eq!(
            abandoned
                .iter()
                .map(|thread| thread.abandoned.processor_id.clone())
                .collect::<Vec<_>>(),
            vec![stuck],
            "only the native thread is abandoned; the helper host returned"
        );
    }
}
