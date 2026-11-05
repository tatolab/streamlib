// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The step barrier of a stepped stream: one thread opens each step and
//! closes it once every node finished it and nothing written in it is still in
//! flight.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::core::context::{StreamClock, StreamClockSpec};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, GraphWeight, ProcessorMetrics, StateComponent,
    StepProgressOfOneNodeComponent, StepProgressReadingOfOneNode,
};
use crate::core::processors::ProcessorState;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::{Error, Result};

/// Longest the coordinator waits for a node to note progress before it reads
/// the ledger again.
const STEP_COORDINATOR_LEDGER_RECHECK: Duration = Duration::from_millis(5);

/// Longest the coordinator waits for the stream's nodes to start before it
/// looks for a stop or a failed node again.
const STEP_COORDINATOR_START_RECHECK: Duration = Duration::from_millis(100);

/// One node's entry on a stream's step ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepLedgerEntryOfOneNode {
    pub(crate) display_name: String,
    pub(crate) step_progress: StepProgressReadingOfOneNode,
    pub(crate) has_failed: bool,
}

/// One link's bag counts on a stream's step ledger: what its producer wrote,
/// and what its destination took or lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StepLedgerEntryOfOneLink {
    pub(crate) written_bags: u64,
    pub(crate) taken_bags: u64,
    pub(crate) dropped_bags: u64,
}

impl StepLedgerEntryOfOneLink {
    /// Bags written and neither taken nor lost yet.
    fn bags_in_flight(&self) -> u64 {
        self.written_bags
            .saturating_sub(self.taken_bags.saturating_add(self.dropped_bags))
    }
}

/// A stream's step ledger as read at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct StepLedgerReading {
    pub(crate) nodes_by_processor_id: BTreeMap<String, StepLedgerEntryOfOneNode>,
    pub(crate) links_by_link_id: BTreeMap<String, StepLedgerEntryOfOneLink>,
}

impl StepLedgerReading {
    /// Read the ledger off `graph`: each node's step progress and state, and
    /// each link's producer's written count beside its destination's taken and
    /// dropped counts.
    pub(crate) fn read_off(graph: &Graph) -> Self {
        let mut reading = Self::default();
        let mut written_bags_by_output_port: HashMap<(String, String), u64> = HashMap::new();
        let mut taken_and_dropped_bags_by_link_id: HashMap<String, (u64, u64)> = HashMap::new();
        for node in graph.traversal().v(()).iter() {
            reading.nodes_by_processor_id.insert(
                node.id().to_string(),
                StepLedgerEntryOfOneNode {
                    display_name: node.display_name.clone(),
                    step_progress: node
                        .get::<StepProgressOfOneNodeComponent>()
                        .map(|step_progress| step_progress.0.reading())
                        .unwrap_or_default(),
                    has_failed: node
                        .get::<StateComponent>()
                        .is_some_and(|state| state.current() == ProcessorState::Error),
                },
            );
            let Some(metrics) = node.get::<ProcessorMetrics>() else {
                continue;
            };
            let loss_counts = metrics.loss_counts.loss_count_snapshot();
            for (output_port, written_bags) in loss_counts.written_bags_by_output_port {
                written_bags_by_output_port
                    .insert((node.id().to_string(), output_port), written_bags);
            }
            for (link_id, taken_bags) in loss_counts.taken_bags_by_inbound_link {
                taken_and_dropped_bags_by_link_id
                    .entry(link_id)
                    .or_default()
                    .0 = taken_bags;
            }
            for (link_id, dropped_bags) in loss_counts.dropped_bags_by_inbound_link {
                taken_and_dropped_bags_by_link_id
                    .entry(link_id)
                    .or_default()
                    .1 = dropped_bags;
            }
        }
        for link in graph.traversal().e(()).iter() {
            let (taken_bags, dropped_bags) = taken_and_dropped_bags_by_link_id
                .get(link.id.as_str())
                .copied()
                .unwrap_or_default();
            let written_bags = written_bags_by_output_port
                .get(&(
                    link.source().processor_id().to_string(),
                    link.source().port_name().to_string(),
                ))
                .copied()
                .unwrap_or_default();
            reading.links_by_link_id.insert(
                link.id.to_string(),
                StepLedgerEntryOfOneLink {
                    written_bags,
                    taken_bags,
                    dropped_bags,
                },
            );
        }
        reading
    }

    /// The display name of a node that failed, when one has.
    pub(crate) fn a_failed_node(&self) -> Option<&str> {
        self.nodes_by_processor_id
            .values()
            .find(|node| node.has_failed)
            .map(|node| node.display_name.as_str())
    }

    /// Whether step `step_index` closes on this reading: every node idle,
    /// every node that finishes every step finished it, and every link carries
    /// no bag beyond the ones `in_flight_before_the_ledger_saw_it` holds.
    pub(crate) fn lets_the_step_close(
        &self,
        step_index: u64,
        in_flight_before_the_ledger_saw_it: &StepLedgerLinkBaselines,
    ) -> bool {
        self.nodes_by_processor_id
            .values()
            .all(|node| node.step_progress.lets_the_step_close(step_index))
            && self.links_by_link_id.iter().all(|(link_id, link)| {
                link.bags_in_flight()
                    <= in_flight_before_the_ledger_saw_it.bags_in_flight_when_first_seen(link_id)
            })
    }
}

/// The bags each link held in flight when the coordinator first read it: a
/// link wired mid-run joins with whatever its producer wrote before it, which
/// will never reach it.
#[derive(Debug, Default)]
pub(crate) struct StepLedgerLinkBaselines {
    bags_in_flight_by_link_id: HashMap<String, u64>,
}

impl StepLedgerLinkBaselines {
    /// Note every link of `reading` not seen before.
    pub(crate) fn note_the_links_first_seen_in(&mut self, reading: &StepLedgerReading) {
        for (link_id, link) in &reading.links_by_link_id {
            self.bags_in_flight_by_link_id
                .entry(link_id.clone())
                .or_insert_with(|| link.bags_in_flight());
        }
    }

    fn bags_in_flight_when_first_seen(&self, link_id: &str) -> u64 {
        self.bags_in_flight_by_link_id
            .get(link_id)
            .copied()
            .unwrap_or_default()
    }
}

/// The stepped clock a coordinator runs: how to pace its steps and when to stop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SteppedClockTheCoordinatorRuns {
    pub(crate) step_ns: u64,
    pub(crate) pace: Option<f64>,
    pub(crate) stop_after_steps: Option<u64>,
}

impl SteppedClockTheCoordinatorRuns {
    /// The stepped clock `spec` names; `None` for any other.
    pub(crate) fn of(spec: StreamClockSpec) -> Option<Self> {
        match spec {
            StreamClockSpec::Stepped {
                step_ns,
                pace,
                stop_after_steps,
            } => Some(Self {
                step_ns,
                pace,
                stop_after_steps,
            }),
            _ => None,
        }
    }

    /// Machine nanoseconds after the first step opened that step
    /// `step_index` opens, no earlier; `None` when unpaced.
    pub(crate) fn machine_ns_from_the_first_step_to(&self, step_index: u64) -> Option<u64> {
        let pace = self.pace?;
        Some(((step_index as f64) * (self.step_ns as f64) / pace) as u64)
    }
}

/// The thread running one stepped stream's step barrier.
pub(crate) struct StreamStepCoordinatorOfOneStream {
    stop_requested: crossbeam_channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl StreamStepCoordinatorOfOneStream {
    /// Start the step barrier of `stream`, which runs `stream_clock`.
    pub(crate) fn start(
        stream: Weak<LoadedStreamInThisRuntime>,
        stream_id: &str,
        stream_clock: Arc<StreamClock>,
        stepped_clock: SteppedClockTheCoordinatorRuns,
    ) -> Result<Self> {
        let (stop_requested, stop_request_arrival) = crossbeam_channel::bounded::<()>(1);
        let thread = std::thread::Builder::new()
            .name(format!("stream-steps-{stream_id}"))
            .spawn(move || {
                run_the_step_barrier(&stream, &stream_clock, stepped_clock, &stop_request_arrival)
            })
            .map_err(|spawn_failure| {
                Error::Runtime(format!(
                    "the step coordinator of the stream `{stream_id}` could not start: \
                     {spawn_failure}"
                ))
            })?;
        Ok(Self {
            stop_requested,
            thread: Some(thread),
        })
    }

    /// Stop opening steps and wait for the thread to end.
    pub(crate) fn stop(mut self) {
        let _ = self.stop_requested.try_send(());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the step coordinator thread panicked");
        }
    }
}

fn run_the_step_barrier(
    stream: &Weak<LoadedStreamInThisRuntime>,
    stream_clock: &StreamClock,
    stepped_clock: SteppedClockTheCoordinatorRuns,
    stop_request_arrival: &crossbeam_channel::Receiver<()>,
) {
    let a_stop_was_requested = || !stop_request_arrival.is_empty();
    let read_the_ledger = || stream.upgrade().map(|stream| stream.step_ledger_reading());
    let ask_for_the_streams_shutdown = |reason: &str| {
        if let Some(stream) = stream.upgrade() {
            stream.ask_for_this_streams_shutdown(reason);
        }
    };

    loop {
        if a_stop_was_requested() {
            return;
        }
        let Some(stream_now) = stream.upgrade() else {
            return;
        };
        if let Some(failed_node) = stream_now.step_ledger_reading().a_failed_node() {
            ask_for_the_streams_shutdown(&format!(
                "the node `{failed_node}` failed before the stream's first step opened"
            ));
            return;
        }
        if stream_now
            .wait_until_every_processor_is_running(STEP_COORDINATOR_START_RECHECK)
            .is_ok()
        {
            break;
        }
    }

    let Some(first_reading) = read_the_ledger() else {
        return;
    };
    let mut baselines = StepLedgerLinkBaselines::default();
    baselines.note_the_links_first_seen_in(&first_reading);
    let first_step_opened_at = Instant::now();
    let mut step_index: u64 = 0;

    loop {
        stream_clock.open_step(step_index);
        tracing::trace!("step {step_index} opened");

        let mut progress_notes_seen = 0;
        let mut previous_reading: Option<StepLedgerReading> = None;
        loop {
            if a_stop_was_requested() {
                return;
            }
            let Some(reading) = read_the_ledger() else {
                return;
            };
            if let Some(failed_node) = reading.a_failed_node() {
                ask_for_the_streams_shutdown(&format!(
                    "the node `{failed_node}` failed while step {step_index} was open"
                ));
                return;
            }
            baselines.note_the_links_first_seen_in(&reading);
            let closes = reading.lets_the_step_close(step_index, &baselines);
            if closes && previous_reading.as_ref() == Some(&reading) {
                break;
            }
            previous_reading = closes.then_some(reading);
            if !closes {
                progress_notes_seen = stream_clock.wait_for_step_progress_after(
                    progress_notes_seen,
                    STEP_COORDINATOR_LEDGER_RECHECK,
                );
            }
        }
        tracing::trace!("step {step_index} closed");

        if stepped_clock
            .stop_after_steps
            .is_some_and(|stop_after_steps| step_index.saturating_add(1) >= stop_after_steps)
        {
            ask_for_the_streams_shutdown(&format!(
                "its stepped clock stops after {} steps",
                step_index.saturating_add(1)
            ));
            return;
        }

        step_index = step_index.saturating_add(1);
        if let Some(machine_ns_from_the_first_step) =
            stepped_clock.machine_ns_from_the_first_step_to(step_index)
        {
            let opens_at =
                first_step_opened_at + Duration::from_nanos(machine_ns_from_the_first_step);
            if stop_request_arrival.recv_deadline(opens_at).is_ok() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_node(step_progress: StepProgressReadingOfOneNode) -> StepLedgerEntryOfOneNode {
        StepLedgerEntryOfOneNode {
            display_name: "node".to_owned(),
            step_progress,
            has_failed: false,
        }
    }

    fn a_reading(
        nodes: impl IntoIterator<Item = StepProgressReadingOfOneNode>,
        links: impl IntoIterator<Item = StepLedgerEntryOfOneLink>,
    ) -> StepLedgerReading {
        StepLedgerReading {
            nodes_by_processor_id: nodes
                .into_iter()
                .enumerate()
                .map(|(index, progress)| (format!("p{index}"), a_node(progress)))
                .collect(),
            links_by_link_id: links
                .into_iter()
                .enumerate()
                .map(|(index, link)| (format!("l{index}"), link))
                .collect(),
        }
    }

    const A_CONTINUOUS_NODE_THAT_FINISHED_STEP_0: StepProgressReadingOfOneNode =
        StepProgressReadingOfOneNode {
            finishes_every_step: true,
            steps_finished: 1,
            callbacks_in_flight: 0,
        };

    const A_REACTIVE_NODE_IN_A_CALLBACK: StepProgressReadingOfOneNode =
        StepProgressReadingOfOneNode {
            finishes_every_step: false,
            steps_finished: 0,
            callbacks_in_flight: 1,
        };

    const AN_IDLE_REACTIVE_NODE: StepProgressReadingOfOneNode = StepProgressReadingOfOneNode {
        finishes_every_step: false,
        steps_finished: 0,
        callbacks_in_flight: 0,
    };

    fn a_link(written_bags: u64, taken_bags: u64, dropped_bags: u64) -> StepLedgerEntryOfOneLink {
        StepLedgerEntryOfOneLink {
            written_bags,
            taken_bags,
            dropped_bags,
        }
    }

    #[test]
    fn a_step_closes_once_every_node_is_idle_and_every_bag_is_taken_or_lost() {
        let reading = a_reading(
            [
                A_CONTINUOUS_NODE_THAT_FINISHED_STEP_0,
                AN_IDLE_REACTIVE_NODE,
            ],
            [a_link(3, 2, 1)],
        );
        assert!(reading.lets_the_step_close(0, &StepLedgerLinkBaselines::default()));
        assert!(!reading.lets_the_step_close(1, &StepLedgerLinkBaselines::default()));
    }

    #[test]
    fn a_bag_still_in_flight_holds_the_step_open() {
        let reading = a_reading(
            [
                A_CONTINUOUS_NODE_THAT_FINISHED_STEP_0,
                AN_IDLE_REACTIVE_NODE,
            ],
            [a_link(3, 2, 0)],
        );
        assert!(!reading.lets_the_step_close(0, &StepLedgerLinkBaselines::default()));
    }

    #[test]
    fn a_node_mid_callback_holds_the_step_open_though_it_took_its_bag() {
        let reading = a_reading(
            [
                A_CONTINUOUS_NODE_THAT_FINISHED_STEP_0,
                A_REACTIVE_NODE_IN_A_CALLBACK,
            ],
            [a_link(1, 1, 0)],
        );
        assert!(!reading.lets_the_step_close(0, &StepLedgerLinkBaselines::default()));
    }

    #[test]
    fn a_continuous_node_that_has_not_finished_the_step_holds_it_open() {
        let reading = a_reading(
            [StepProgressReadingOfOneNode {
                finishes_every_step: true,
                steps_finished: 0,
                callbacks_in_flight: 0,
            }],
            [],
        );
        assert!(!reading.lets_the_step_close(0, &StepLedgerLinkBaselines::default()));
    }

    #[test]
    fn bags_a_link_missed_before_the_ledger_saw_it_never_hold_a_step_open() {
        let mut baselines = StepLedgerLinkBaselines::default();
        baselines.note_the_links_first_seen_in(&a_reading([], [a_link(5, 0, 0)]));
        assert!(
            a_reading([AN_IDLE_REACTIVE_NODE], [a_link(7, 2, 0)])
                .lets_the_step_close(0, &baselines)
        );
        assert!(
            !a_reading([AN_IDLE_REACTIVE_NODE], [a_link(8, 2, 0)])
                .lets_the_step_close(0, &baselines)
        );
    }

    #[test]
    fn a_failed_node_is_named() {
        let mut reading = a_reading([AN_IDLE_REACTIVE_NODE], []);
        assert_eq!(reading.a_failed_node(), None);
        if let Some(node) = reading.nodes_by_processor_id.values_mut().next() {
            node.has_failed = true;
        }
        assert_eq!(reading.a_failed_node(), Some("node"));
    }

    #[test]
    fn a_paced_step_opens_no_earlier_than_its_share_of_real_time() {
        let paced_at_twice_real_time = SteppedClockTheCoordinatorRuns {
            step_ns: 10_000_000,
            pace: Some(2.0),
            stop_after_steps: None,
        };
        assert_eq!(
            paced_at_twice_real_time.machine_ns_from_the_first_step_to(4),
            Some(20_000_000)
        );
        let unpaced = SteppedClockTheCoordinatorRuns {
            pace: None,
            ..paced_at_twice_real_time
        };
        assert_eq!(unpaced.machine_ns_from_the_first_step_to(4), None);
    }
}
