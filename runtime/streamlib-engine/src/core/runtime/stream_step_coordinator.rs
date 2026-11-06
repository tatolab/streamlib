// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The step barrier of a stream whose clock steps: one thread opens each step
//! and closes it once every node finished it and nothing written in it is
//! still in flight. A scaled clock is behind the barrier only while it runs
//! faster than real time; the thread idles while it runs free.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::core::context::{StreamClock, StreamClockSpec};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, GraphWeight, LinkState,
    LinkStateComponent, ProcessorMetrics, ProcessorPauseGateComponent, StateComponent,
    StepProgressOfOneNodeComponent, StepProgressReadingOfOneNode,
};
use crate::core::media_clock::MediaClock;
use crate::core::processors::ProcessorState;
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::{Error, Result};

/// Longest the coordinator waits for a node to note progress before it reads
/// the ledger again.
const STEP_COORDINATOR_LEDGER_RECHECK: Duration = Duration::from_millis(5);

/// Longest the coordinator waits for the stream's nodes to start before it
/// looks for a stop or a failed node again.
const STEP_COORDINATOR_START_RECHECK: Duration = Duration::from_millis(100);

/// Longest the coordinator waits, while its clock runs free of the barrier or
/// a paced step is not due, before it looks at the clock and for a stop again.
const STEP_COORDINATOR_CLOCK_RECHECK: Duration = Duration::from_millis(50);

/// One node's entry on a stream's step ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepLedgerEntryOfOneNode {
    pub(crate) display_name: String,
    pub(crate) step_progress: StepProgressReadingOfOneNode,
    pub(crate) has_failed: bool,
    /// Whether a new helper is being started in place of one that died, for
    /// a node neither paused nor paced by real time.
    pub(crate) is_restarting_its_helper_inside_the_barrier: bool,
}

impl StepLedgerEntryOfOneNode {
    /// Whether the node will never finish a callback again.
    fn has_ended(&self) -> bool {
        self.has_failed || self.step_progress.panicked_mid_callback
    }
}

/// One link's bag counts on a stream's step ledger: what its producer wrote,
/// and what its destination took or lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StepLedgerEntryOfOneLink {
    pub(crate) written_bags: u64,
    pub(crate) taken_bags: u64,
    pub(crate) dropped_bags: u64,
    /// Whether the link's destination is subscribed, so every bag written
    /// from here on reaches it.
    pub(crate) is_wired: bool,
    /// Whether a new helper of the link's destination is being started in
    /// place of one that died, so a bag written now may never reach it.
    pub(crate) destination_is_restarting_its_helper: bool,
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
    /// Links in the graph left off the balance on this reading.
    pub(crate) link_ids_off_the_balance: BTreeSet<String>,
}

impl StepLedgerReading {
    /// Read the ledger off `graph`: each node's step progress and state, and
    /// each link's producer's written count beside its destination's taken and
    /// dropped counts.
    ///
    /// A link into a continuous node or a paused one is left off: such a node
    /// takes what waits for it when it next runs, never within the step that
    /// wrote it. So is a link into a windowed port, which takes its bags only
    /// once they fill a window, however many steps that spans, and a link to
    /// or from a node paced by real time, which stands outside the barrier.
    pub(crate) fn read_off(graph: &Graph) -> Self {
        let mut reading = Self::default();
        let mut processor_ids_that_take_bags_on_their_own_pace = std::collections::HashSet::new();
        let mut written_bags_by_output_port: HashMap<(String, String), u64> = HashMap::new();
        let mut taken_and_dropped_bags_by_link_id: HashMap<String, (u64, u64)> = HashMap::new();
        let mut link_ids_into_a_windowed_port = std::collections::HashSet::new();
        let mut processor_ids_paced_by_real_time = std::collections::HashSet::new();
        let mut processor_ids_restarting_their_helper = std::collections::HashSet::new();
        for node in graph.traversal().v(()).iter() {
            let metrics = node.get::<ProcessorMetrics>();
            let processor_state = node.get::<StateComponent>().map(StateComponent::current);
            if processor_state == Some(ProcessorState::Restarting) {
                processor_ids_restarting_their_helper.insert(node.id().to_string());
            }
            let step_progress_of_its_host = node
                .get::<StepProgressOfOneNodeComponent>()
                .map(|step_progress| step_progress.0.reading())
                .unwrap_or_default();
            let step_progress = match metrics
                .and_then(|metrics| metrics.loss_counts.step_progress_its_helper_process_wrote())
            {
                Some(written_by_its_helper_process) => step_progress_of_its_host
                    .with_what_its_helper_process_wrote(written_by_its_helper_process),
                None => step_progress_of_its_host,
            };
            if step_progress.stands_outside_the_step_barrier {
                processor_ids_paced_by_real_time.insert(node.id().to_string());
            }
            let is_paused = node
                .get::<ProcessorPauseGateComponent>()
                .is_some_and(ProcessorPauseGateComponent::is_paused);
            if step_progress.finishes_every_step || is_paused {
                processor_ids_that_take_bags_on_their_own_pace.insert(node.id().to_string());
            }
            reading.nodes_by_processor_id.insert(
                node.id().to_string(),
                StepLedgerEntryOfOneNode {
                    display_name: node.display_name.clone(),
                    step_progress,
                    has_failed: processor_state == Some(ProcessorState::Error),
                    is_restarting_its_helper_inside_the_barrier: processor_state
                        == Some(ProcessorState::Restarting)
                        && !is_paused
                        && !step_progress.stands_outside_the_step_barrier,
                },
            );
            let Some(metrics) = metrics else {
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
            link_ids_into_a_windowed_port
                .extend(loss_counts.discarded_samples_by_inbound_link.into_keys());
            for (link_id, dropped_bags) in loss_counts.dropped_bags_by_inbound_link {
                taken_and_dropped_bags_by_link_id
                    .entry(link_id)
                    .or_default()
                    .1 = dropped_bags;
            }
        }
        for link in graph.traversal().e(()).iter() {
            if processor_ids_that_take_bags_on_their_own_pace
                .contains(link.to_port().processor_id().as_str())
                || link_ids_into_a_windowed_port.contains(link.id.as_str())
                || processor_ids_paced_by_real_time.contains(link.source().processor_id().as_str())
                || processor_ids_paced_by_real_time.contains(link.to_port().processor_id().as_str())
            {
                reading.link_ids_off_the_balance.insert(link.id.to_string());
                continue;
            }
            let is_wired = link
                .get::<LinkStateComponent>()
                .map_or(link.state, |link_state| link_state.0)
                == LinkState::Wired;
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
                    is_wired,
                    destination_is_restarting_its_helper: processor_ids_restarting_their_helper
                        .contains(link.to_port().processor_id().as_str()),
                },
            );
        }
        reading
    }

    /// The display name of a node that failed or panicked, when one has.
    pub(crate) fn a_failed_node(&self) -> Option<&str> {
        self.nodes_by_processor_id
            .values()
            .find(|node| node.has_ended())
            .map(|node| node.display_name.as_str())
    }

    /// Whether step `step_index` closes on this reading: no node inside the
    /// barrier restarting its helper, every node idle, every node that
    /// finishes every step finished it, and every link carries no bag beyond the ones `in_flight_before_the_ledger_saw_it` holds.
    pub(crate) fn lets_the_step_close(
        &self,
        step_index: u64,
        in_flight_before_the_ledger_saw_it: &StepLedgerLinkBaselines,
    ) -> bool {
        self.nodes_by_processor_id.values().all(|node| {
            !node.is_restarting_its_helper_inside_the_barrier
                && node.step_progress.lets_the_step_close(step_index)
        }) && self.links_by_link_id.iter().all(|(link_id, link)| {
            link.bags_in_flight()
                <= in_flight_before_the_ledger_saw_it.bags_in_flight_when_first_seen(link_id)
        })
    }
}

/// The bags each link held in flight when its destination subscribed: a link
/// wired mid-run joins with whatever its producer wrote before it, and a link
/// whose destination's helper was restarted with whatever the dead helper
/// left unread, neither of which will ever reach it. A baseline only falls
/// after that, to what the link holds in flight once its destination takes a
/// bag the baseline counted as never reaching it.
#[derive(Debug, Default)]
pub(crate) struct StepLedgerLinkBaselines {
    bags_in_flight_by_link_id: HashMap<String, u64>,
}

impl StepLedgerLinkBaselines {
    /// Follow the links of `reading`: a link not wired yet, or whose
    /// destination is restarting its helper, is baselined at what it holds in
    /// flight now; a wired one keeps the baseline it had when it was first seen
    /// so, also while it is off the balance, lowered to what it holds in
    /// flight whenever that is less; and a link gone from the graph is
    /// forgotten.
    pub(crate) fn follow_the_links_in(&mut self, reading: &StepLedgerReading) {
        self.bags_in_flight_by_link_id.retain(|link_id, _| {
            reading.links_by_link_id.contains_key(link_id)
                || reading.link_ids_off_the_balance.contains(link_id)
        });
        for (link_id, link) in &reading.links_by_link_id {
            if link.is_wired && !link.destination_is_restarting_its_helper {
                let baseline = self
                    .bags_in_flight_by_link_id
                    .entry(link_id.clone())
                    .or_insert_with(|| link.bags_in_flight());
                *baseline = (*baseline).min(link.bags_in_flight());
            } else {
                self.bags_in_flight_by_link_id
                    .insert(link_id.clone(), link.bags_in_flight());
            }
        }
    }

    fn bags_in_flight_when_first_seen(&self, link_id: &str) -> u64 {
        self.bags_in_flight_by_link_id
            .get(link_id)
            .copied()
            .unwrap_or_default()
    }
}

/// The thread running the step barrier of one stream whose clock is not real.
pub(crate) struct StreamStepCoordinatorOfOneStream {
    stop_requested: crossbeam_channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for StreamStepCoordinatorOfOneStream {
    fn drop(&mut self) {
        let _ = self.stop_requested.try_send(());
    }
}

impl StreamStepCoordinatorOfOneStream {
    /// Start the step barrier of `stream`, which runs `stream_clock`.
    pub(crate) fn start(
        stream: Weak<LoadedStreamInThisRuntime>,
        stream_id: &str,
        stream_clock: Arc<StreamClock>,
    ) -> Result<Self> {
        let (stop_requested, stop_request_arrival) = crossbeam_channel::bounded::<()>(1);
        let thread = std::thread::Builder::new()
            .name(format!("stream-steps-{stream_id}"))
            .spawn(move || run_the_step_barrier(&stream, &stream_clock, &stop_request_arrival))
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

/// How the coordinator's watch over one open step ended.
enum OpenStepWatchOutcome {
    Closed,
    /// The clock left the barrier, or the step is no longer the one open.
    NoLongerOpen,
    StopTheCoordinator,
}

fn run_the_step_barrier(
    stream: &Weak<LoadedStreamInThisRuntime>,
    stream_clock: &StreamClock,
    stop_request_arrival: &crossbeam_channel::Receiver<()>,
) {
    let a_stop_was_requested = || {
        !matches!(
            stop_request_arrival.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        )
    };
    let ask_for_the_streams_shutdown = |reason: &str| {
        if let Some(stream) = stream.upgrade() {
            stream.ask_for_this_streams_shutdown(reason);
        }
    };
    let mut baselines = StepLedgerLinkBaselines::default();
    let mut waits_for_every_node_to_start = stream_clock.runs_behind_the_step_barrier();

    loop {
        if a_stop_was_requested() {
            return;
        }
        let Some(stream_now) = stream.upgrade() else {
            return;
        };
        let Some(step_index) = stream_clock.step_open_behind_the_barrier() else {
            if !stream_clock
                .wait_until_it_runs_behind_the_step_barrier(STEP_COORDINATOR_CLOCK_RECHECK)
            {
                waits_for_every_node_to_start = false;
                continue;
            }
            if let Some(failed_node) = stream_now.step_ledger_reading().a_failed_node() {
                ask_for_the_streams_shutdown(&format!(
                    "the node `{failed_node}` failed before the stream's next step opened"
                ));
                return;
            }
            let the_stream_runs = !waits_for_every_node_to_start
                || stream_now
                    .observable_graph_readiness()
                    .wait_until_every_processor_has_finished_setup(STEP_COORDINATOR_START_RECHECK);
            if the_stream_runs {
                baselines.follow_the_links_in(&stream_now.step_ledger_reading());
                if let Some(opened) = stream_clock.open_the_waiting_step() {
                    waits_for_every_node_to_start = false;
                    tracing::trace!("step {opened} opened");
                }
            }
            continue;
        };
        drop(stream_now);

        match watch_the_open_step_until_it_closes(
            stream,
            stream_clock,
            step_index,
            &mut baselines,
            &a_stop_was_requested,
        ) {
            OpenStepWatchOutcome::StopTheCoordinator => return,
            OpenStepWatchOutcome::NoLongerOpen => continue,
            OpenStepWatchOutcome::Closed => {}
        }
        tracing::trace!("step {step_index} closed");

        if let StreamClockSpec::Stepped {
            stop_after_steps: Some(stop_after_steps),
            ..
        } = stream_clock.spec()
            && step_index.saturating_add(1) >= stop_after_steps
        {
            ask_for_the_streams_shutdown(&format!(
                "its stepped clock stops after {} steps",
                step_index.saturating_add(1)
            ));
            return;
        }

        loop {
            if stream_clock.step_open_behind_the_barrier() != Some(step_index) {
                break;
            }
            let Some(opens_at_ns) =
                stream_clock.machine_time_ns_the_step_opens(step_index.saturating_add(1))
            else {
                break;
            };
            let machine_ns_to_go = opens_at_ns.saturating_sub(MediaClock::now().as_nanos() as i64);
            if machine_ns_to_go <= 0 {
                break;
            }
            let wait =
                Duration::from_nanos(machine_ns_to_go as u64).min(STEP_COORDINATOR_CLOCK_RECHECK);
            if !matches!(
                stop_request_arrival.recv_timeout(wait),
                Err(crossbeam_channel::RecvTimeoutError::Timeout)
            ) {
                return;
            }
        }
        if stream_clock.open_the_step_after(step_index) {
            tracing::trace!("step {} opened", step_index.saturating_add(1));
        }
    }
}

/// Read the ledger until step `step_index` closes on two equal readings in a
/// row, a node fails, or the step is no longer the one open.
fn watch_the_open_step_until_it_closes(
    stream: &Weak<LoadedStreamInThisRuntime>,
    stream_clock: &StreamClock,
    step_index: u64,
    baselines: &mut StepLedgerLinkBaselines,
    a_stop_was_requested: &impl Fn() -> bool,
) -> OpenStepWatchOutcome {
    let mut progress_notes_seen = 0;
    let mut previous_reading: Option<StepLedgerReading> = None;
    loop {
        if a_stop_was_requested() {
            return OpenStepWatchOutcome::StopTheCoordinator;
        }
        if stream_clock.step_open_behind_the_barrier() != Some(step_index) {
            return OpenStepWatchOutcome::NoLongerOpen;
        }
        let Some(stream) = stream.upgrade() else {
            return OpenStepWatchOutcome::StopTheCoordinator;
        };
        let reading = stream.step_ledger_reading();
        if let Some(failed_node) = reading.a_failed_node() {
            stream.ask_for_this_streams_shutdown(&format!(
                "the node `{failed_node}` failed while step {step_index} was open"
            ));
            return OpenStepWatchOutcome::StopTheCoordinator;
        }
        drop(stream);
        baselines.follow_the_links_in(&reading);
        let closes = reading.lets_the_step_close(step_index, baselines);
        if closes && previous_reading.as_ref() == Some(&reading) {
            return OpenStepWatchOutcome::Closed;
        }
        previous_reading = closes.then_some(reading);
        if !closes {
            progress_notes_seen = stream_clock
                .wait_for_step_progress_after(progress_notes_seen, STEP_COORDINATOR_LEDGER_RECHECK);
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
            is_restarting_its_helper_inside_the_barrier: false,
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
            link_ids_off_the_balance: BTreeSet::new(),
        }
    }

    const A_CONTINUOUS_NODE_THAT_FINISHED_STEP_0: StepProgressReadingOfOneNode =
        StepProgressReadingOfOneNode {
            finishes_every_step: true,
            steps_finished: 1,
            callbacks_in_flight: 0,
            panicked_mid_callback: false,
            stands_outside_the_step_barrier: false,
        };

    const A_REACTIVE_NODE_IN_A_CALLBACK: StepProgressReadingOfOneNode =
        StepProgressReadingOfOneNode {
            finishes_every_step: false,
            steps_finished: 0,
            callbacks_in_flight: 1,
            panicked_mid_callback: false,
            stands_outside_the_step_barrier: false,
        };

    const AN_IDLE_REACTIVE_NODE: StepProgressReadingOfOneNode = StepProgressReadingOfOneNode {
        finishes_every_step: false,
        steps_finished: 0,
        callbacks_in_flight: 0,
        panicked_mid_callback: false,
        stands_outside_the_step_barrier: false,
    };

    fn a_link(written_bags: u64, taken_bags: u64, dropped_bags: u64) -> StepLedgerEntryOfOneLink {
        StepLedgerEntryOfOneLink {
            written_bags,
            taken_bags,
            dropped_bags,
            is_wired: true,
            destination_is_restarting_its_helper: false,
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
                panicked_mid_callback: false,
                stands_outside_the_step_barrier: false,
            }],
            [],
        );
        assert!(!reading.lets_the_step_close(0, &StepLedgerLinkBaselines::default()));
    }

    #[test]
    fn bags_written_before_a_link_was_wired_never_hold_a_step_open() {
        let mut baselines = StepLedgerLinkBaselines::default();
        baselines.follow_the_links_in(&a_reading(
            [],
            [StepLedgerEntryOfOneLink {
                is_wired: false,
                ..a_link(3, 0, 0)
            }],
        ));
        baselines.follow_the_links_in(&a_reading([], [a_link(5, 0, 0)]));
        assert!(
            a_reading([AN_IDLE_REACTIVE_NODE], [a_link(5, 2, 0)])
                .lets_the_step_close(0, &baselines)
        );
        assert!(
            !a_reading([AN_IDLE_REACTIVE_NODE], [a_link(6, 2, 0)])
                .lets_the_step_close(0, &baselines)
        );
    }

    #[test]
    fn bags_queued_while_a_link_was_off_the_balance_hold_the_step_open_once_it_is_back() {
        let mut baselines = StepLedgerLinkBaselines::default();
        baselines.follow_the_links_in(&a_reading([], [a_link(2, 2, 0)]));
        let mut off_the_balance_while_its_reader_is_paused = a_reading([], []);
        off_the_balance_while_its_reader_is_paused
            .link_ids_off_the_balance
            .insert("l0".to_owned());
        baselines.follow_the_links_in(&off_the_balance_while_its_reader_is_paused);
        let back_with_a_backlog_and_a_new_bag =
            a_reading([AN_IDLE_REACTIVE_NODE], [a_link(6, 3, 0)]);
        baselines.follow_the_links_in(&back_with_a_backlog_and_a_new_bag);
        assert!(!back_with_a_backlog_and_a_new_bag.lets_the_step_close(0, &baselines));
        assert!(
            a_reading([AN_IDLE_REACTIVE_NODE], [a_link(6, 6, 0)])
                .lets_the_step_close(0, &baselines)
        );
    }

    #[test]
    fn a_node_restarting_its_helper_holds_the_step_open() {
        let mut reading = a_reading([AN_IDLE_REACTIVE_NODE], []);
        reading
            .nodes_by_processor_id
            .values_mut()
            .for_each(|node| node.is_restarting_its_helper_inside_the_barrier = true);
        assert!(!reading.lets_the_step_close(0, &StepLedgerLinkBaselines::default()));
    }

    #[test]
    fn bags_a_restarted_helper_never_read_never_hold_a_step_open() {
        let mut baselines = StepLedgerLinkBaselines::default();
        baselines.follow_the_links_in(&a_reading([], [a_link(2, 0, 0)]));
        let one_left_unread_by_a_helper_that_died =
            a_reading([AN_IDLE_REACTIVE_NODE], [a_link(4, 1, 0)]);
        baselines.follow_the_links_in(&one_left_unread_by_a_helper_that_died);
        assert!(!one_left_unread_by_a_helper_that_died.lets_the_step_close(0, &baselines));

        let mut its_helper_restarting = a_link(4, 1, 0);
        its_helper_restarting.destination_is_restarting_its_helper = true;
        baselines.follow_the_links_in(&a_reading([AN_IDLE_REACTIVE_NODE], [its_helper_restarting]));
        let after_the_restart = a_reading([AN_IDLE_REACTIVE_NODE], [a_link(4, 1, 0)]);
        baselines.follow_the_links_in(&after_the_restart);
        assert!(after_the_restart.lets_the_step_close(0, &baselines));

        let with_a_new_bag = a_reading([AN_IDLE_REACTIVE_NODE], [a_link(5, 1, 0)]);
        baselines.follow_the_links_in(&with_a_new_bag);
        assert!(!with_a_new_bag.lets_the_step_close(0, &baselines));
    }

    #[test]
    fn a_bag_queued_for_a_restarted_helper_and_taken_lowers_the_baseline() {
        let mut baselines = StepLedgerLinkBaselines::default();
        let mut its_helper_restarting = a_link(4, 1, 0);
        its_helper_restarting.destination_is_restarting_its_helper = true;
        baselines.follow_the_links_in(&a_reading([], [its_helper_restarting]));
        baselines.follow_the_links_in(&a_reading([], [a_link(4, 2, 0)]));

        let one_left_unread_a_step_later = a_reading([AN_IDLE_REACTIVE_NODE], [a_link(5, 2, 0)]);
        baselines.follow_the_links_in(&one_left_unread_a_step_later);
        assert!(!one_left_unread_a_step_later.lets_the_step_close(0, &baselines));
    }

    #[test]
    fn a_link_gone_from_the_graph_takes_its_baseline_with_it() {
        let mut baselines = StepLedgerLinkBaselines::default();
        baselines.follow_the_links_in(&a_reading([], [a_link(5, 0, 0)]));
        baselines.follow_the_links_in(&a_reading([], []));
        assert_eq!(baselines.bags_in_flight_when_first_seen("l0"), 0);
    }

    #[test]
    fn a_node_that_panicked_mid_callback_is_named_as_failed() {
        let reading = a_reading(
            [StepProgressReadingOfOneNode {
                panicked_mid_callback: true,
                ..AN_IDLE_REACTIVE_NODE
            }],
            [],
        );
        assert_eq!(reading.a_failed_node(), Some("node"));
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
}
